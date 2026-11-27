//! Content-addressed raster image attachments stored beside journal artifacts.
//!
//! Blobs live under `<hot>/attachments/<sha256>.<ext>` and
//! `<cold>/attachments/<sha256>.<ext>` and are referenced in Markdown by the
//! relative path `attachments/<sha256>.<ext>`. Only structurally valid PNG,
//! JPEG, WebP, and GIF payloads up to [`MAX_ATTACHMENT_BYTES`] are accepted;
//! SVG, HTML, truncated/magic-only payloads, and other active formats are
//! refused. Directory and blob opens inside the journal never follow symbolic
//! links and verify container structure and SHA-256 digest on every write,
//! read, import, and rendered transform.

use super::private_text::{Finding, Scanner};
use super::{archive_dir, parse_artifact_name, relocation, DoctorFinding, JournalEvent};
use anyhow::{bail, Context, Result};
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Subdirectory holding content-addressed image attachments beside artifacts.
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";

/// Maximum accepted byte length of one attached image (5 MiB).
pub(crate) const MAX_ATTACHMENT_BYTES: u64 = 5 * 1024 * 1024;

/// Maximum decompressed byte length accepted while scanning one compressed
/// metadata chunk (2 MiB) to bound memory against decompression bombs.
const MAX_METADATA_DECOMPRESS_BYTES: usize = 2 * 1024 * 1024;

/// Allowed raster image formats. Active and vector formats are excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageFormat {
    pub(crate) const fn media_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Gif => "image/gif",
        }
    }

    pub(crate) const fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
            ImageFormat::Gif => "gif",
        }
    }

    pub(crate) fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "png" => Some(ImageFormat::Png),
            "jpg" => Some(ImageFormat::Jpeg),
            "webp" => Some(ImageFormat::Webp),
            "gif" => Some(ImageFormat::Gif),
            _ => None,
        }
    }

    /// Sniff an allowed raster image format from its container signature.
    /// Callers validating a blob must use [`validate_image_bytes`], which
    /// validates the full container structure rather than the prefix alone.
    pub(crate) fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        {
            return Some(ImageFormat::Png);
        }
        if bytes.len() >= 4
            && bytes[0] == 0xFF
            && bytes[1] == 0xD8
            && bytes[2] == 0xFF
            && bytes[3] != 0x00
            && bytes[3] != 0xFF
        {
            return Some(ImageFormat::Jpeg);
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(ImageFormat::Webp);
        }
        if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
            return Some(ImageFormat::Gif);
        }
        None
    }
}

/// Content-addressed identity of one stored attachment: `<sha256>.<ext>`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct AttachmentId {
    pub(crate) sha256: String,
    pub(crate) format: ImageFormat,
}

impl AttachmentId {
    pub(crate) fn filename(&self) -> String {
        format!("{}.{}", self.sha256, self.format.ext())
    }

    pub(crate) fn rel_path(&self) -> String {
        format!("{ATTACHMENTS_DIR}/{}.{}", self.sha256, self.format.ext())
    }

    pub(crate) fn digest(&self) -> String {
        format!("sha256:{}", self.sha256)
    }

    /// Parse a canonical relative Markdown path `attachments/<64-hex>.<ext>`.
    pub(crate) fn parse_rel_path(value: &str) -> Option<Self> {
        let name = value.strip_prefix("attachments/")?;
        Self::parse_filename(name)
    }

    /// Parse a bare blob filename `<64-hex>.<ext>`, rejecting separators and
    /// any non-lowercase or malformed digest.
    pub(crate) fn parse_filename(name: &str) -> Option<Self> {
        if name.contains(['/', '\\']) {
            return None;
        }
        let (hash, ext) = name.split_once('.')?;
        if ext.contains('.') {
            return None;
        }
        if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let format = ImageFormat::from_ext(ext)?;
        Some(Self {
            sha256: hash.to_string(),
            format,
        })
    }
}

/// Privacy-safe attachment metadata recorded on journal events and JSON views.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachmentRef {
    pub(crate) path: String,
    pub(crate) sha256: String,
    pub(crate) media_type: String,
    pub(crate) bytes: u64,
}

impl AttachmentRef {
    pub(crate) fn well_formed(&self) -> bool {
        let Some(id) = AttachmentId::parse_rel_path(&self.path) else {
            return false;
        };
        self.sha256 == id.digest()
            && self.media_type == id.format.media_type()
            && (1..=MAX_ATTACHMENT_BYTES).contains(&self.bytes)
    }
}

/// Self-verifying attachment payload carried in journal bundles and spools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundledAttachment {
    pub(crate) path: String,
    pub(crate) sha256: String,
    pub(crate) media_type: String,
    pub(crate) bytes: u64,
    pub(crate) data: String,
}

impl BundledAttachment {
    pub(crate) fn from_verified(id: &AttachmentId, bytes: &[u8]) -> Self {
        Self {
            path: id.rel_path(),
            sha256: id.digest(),
            media_type: id.format.media_type().to_string(),
            bytes: bytes.len() as u64,
            data: encode_base64(bytes),
        }
    }

    /// Validate metadata, base64 encoding, size bounds, full container
    /// structure, and SHA-256 digest.
    pub(crate) fn validate_and_decode(&self) -> Result<(AttachmentId, Vec<u8>)> {
        let id = AttachmentId::parse_rel_path(&self.path)
            .with_context(|| format!("invalid bundled attachment path {:?}", self.path))?;
        if self.sha256 != id.digest() {
            bail!(
                "bundled attachment {} digest {} does not match its path",
                self.path,
                self.sha256
            );
        }
        if self.media_type != id.format.media_type() {
            bail!(
                "bundled attachment {} media type {:?} does not match its extension",
                self.path,
                self.media_type
            );
        }
        if self.bytes == 0 || self.bytes > MAX_ATTACHMENT_BYTES {
            bail!(
                "bundled attachment {} size {} is outside 1..={MAX_ATTACHMENT_BYTES} bytes",
                self.path,
                self.bytes
            );
        }
        let max_encoded = ((MAX_ATTACHMENT_BYTES as usize).div_ceil(3)) * 4;
        if self.data.len() > max_encoded {
            bail!(
                "bundled attachment {} encoded payload exceeds the {MAX_ATTACHMENT_BYTES}-byte limit",
                self.path
            );
        }
        let decoded = decode_base64(&self.data)
            .with_context(|| format!("bundled attachment {} has invalid base64", self.path))?;
        if decoded.len() as u64 != self.bytes {
            bail!(
                "bundled attachment {} declared {} bytes, decoded {}",
                self.path,
                self.bytes,
                decoded.len()
            );
        }
        let validated = validate_image_bytes(&decoded)
            .with_context(|| format!("bundled attachment {} is invalid", self.path))?;
        if validated != id.format {
            bail!(
                "bundled attachment {} bytes do not match {}",
                self.path,
                id.format.media_type()
            );
        }
        let actual = hex::encode(Sha256::digest(&decoded));
        if actual != id.sha256 {
            bail!(
                "bundled attachment {} checksum mismatch (expected {}, got {actual})",
                self.path,
                id.sha256
            );
        }
        Ok((id, decoded))
    }
}

/// One validated attachment staged from a caller-supplied source file.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedAttachment {
    pub(crate) id: AttachmentId,
    pub(crate) bytes: Vec<u8>,
    pub(crate) alt: String,
}

impl ValidatedAttachment {
    pub(crate) fn reference(&self) -> AttachmentRef {
        AttachmentRef {
            path: self.id.rel_path(),
            sha256: self.id.digest(),
            media_type: self.id.format.media_type().to_string(),
            bytes: self.bytes.len() as u64,
        }
    }

    pub(crate) fn markdown_line(&self) -> String {
        format!("![{}]({})", self.alt, self.id.rel_path())
    }

    pub(crate) fn to_bundled(&self) -> BundledAttachment {
        BundledAttachment::from_verified(&self.id, &self.bytes)
    }
}

/// Which journal storage tier holds a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StorageTier {
    Hot,
    Cold,
}

/// Outcome of reading and verifying one attachment blob from journal storage.
#[derive(Debug)]
pub(crate) enum ReadOutcome {
    Available { bytes: Vec<u8> },
    Missing,
    Corrupt(String),
}

/// Read, validate, and hash one caller-supplied image file before any write.
///
/// The source file's host path is never stored in the returned metadata or alt
/// text. Non-regular files (directories, FIFOs, devices), empty files, files
/// larger than [`MAX_ATTACHMENT_BYTES`], truncated/malformed containers, and
/// non-raster/active formats (such as SVG or HTML) are refused.
pub(super) fn load_source(path: &Path, scanner: &Scanner) -> Result<ValidatedAttachment> {
    if path.as_os_str().is_empty() {
        bail!("attachment path must not be empty");
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if is_active_extension(ext) {
            bail!(
                "active or markup format (.{ext}) is not allowed as a journal attachment; \
                 supported raster image formats are PNG, JPEG, WebP, and GIF"
            );
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("cannot open attachment {}", path.display()))?;
    let meta = file
        .metadata()
        .with_context(|| format!("cannot inspect attachment {}", path.display()))?;
    if !meta.is_file() {
        bail!("attachment {} is not a regular file", path.display());
    }
    if meta.len() == 0 {
        bail!("attachment {} is empty", path.display());
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        bail!(
            "attachment {} is {} bytes, exceeding the {MAX_ATTACHMENT_BYTES}-byte (5 MiB) limit",
            path.display(),
            meta.len()
        );
    }
    let mut bytes = Vec::with_capacity((meta.len() as usize).min(MAX_ATTACHMENT_BYTES as usize));
    Read::by_ref(&mut file)
        .take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read attachment {}", path.display()))?;
    if bytes.is_empty() {
        bail!("attachment {} is empty", path.display());
    }
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        bail!(
            "attachment {} exceeds the {MAX_ATTACHMENT_BYTES}-byte (5 MiB) limit",
            path.display()
        );
    }
    let format = validate_image_bytes(&bytes)?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let id = AttachmentId { sha256, format };
    let alt = safe_alt_from_path(path, scanner);
    Ok(ValidatedAttachment { id, bytes, alt })
}

/// Load and validate all `--attach` source files, preserving order while
/// deduplicating identical content hashes.
pub(super) fn load_sources(
    paths: &[PathBuf],
    scanner: &Scanner,
) -> Result<Vec<ValidatedAttachment>> {
    let mut loaded = Vec::with_capacity(paths.len());
    let mut seen = BTreeSet::new();
    for path in paths {
        let attachment = load_source(path, scanner)?;
        if seen.insert(attachment.id.clone()) {
            loaded.push(attachment);
        }
    }
    Ok(loaded)
}

fn is_active_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "svg" | "svgz" | "html" | "htm" | "xhtml" | "xml" | "js" | "mjs" | "cjs" | "css"
    )
}

fn looks_like_markup_or_svg(bytes: &[u8]) -> bool {
    let trimmed = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let trimmed = trimmed
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(&[][..], |idx| &trimmed[idx..]);
    let prefix: Vec<u8> = trimmed
        .iter()
        .take(32)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    prefix.starts_with(b"<svg")
        || prefix.starts_with(b"<?xml")
        || prefix.starts_with(b"<!doctype")
        || prefix.starts_with(b"<html")
        || prefix.starts_with(b"<script")
        || prefix.starts_with(b"<!--")
}

/// Validate size bounds, reject active markup, and structurally validate the
/// full PNG, JPEG, WebP, or GIF container framing.
pub(crate) fn validate_image_bytes(bytes: &[u8]) -> Result<ImageFormat> {
    if bytes.is_empty() {
        bail!("attachment is empty");
    }
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        bail!("attachment exceeds the {MAX_ATTACHMENT_BYTES}-byte (5 MiB) limit");
    }
    if looks_like_markup_or_svg(bytes) {
        bail!(
            "SVG and markup/active content are not allowed as journal attachments; \
             supported raster image formats are PNG, JPEG, WebP, and GIF"
        );
    }
    let format = ImageFormat::detect(bytes).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported attachment format: magic bytes do not match PNG, JPEG, WebP, or GIF"
        )
    })?;
    match format {
        ImageFormat::Png => validate_png(bytes)?,
        ImageFormat::Jpeg => validate_jpeg(bytes)?,
        ImageFormat::Webp => validate_webp(bytes)?,
        ImageFormat::Gif => validate_gif(bytes)?,
    }
    Ok(format)
}

fn validate_png(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 8 + 25 + 12 {
        bail!("truncated PNG image");
    }
    let mut pos = 8usize;
    let mut saw_ihdr = false;
    let mut needs_plte = false;
    let mut saw_plte = false;
    let mut idat_bytes = 0usize;
    let mut idat_head = [0u8; 2];
    let mut idat_head_len = 0usize;
    let mut saw_iend = false;

    while pos < bytes.len() {
        if saw_iend {
            bail!("trailing bytes after PNG IEND chunk");
        }
        if pos + 12 > bytes.len() {
            bail!("truncated PNG chunk header");
        }
        let len_u32 =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        if len_u32 > 0x7FFF_FFFF {
            bail!("PNG chunk length exceeds 31-bit limit");
        }
        let len = len_u32 as usize;
        let kind = &bytes[pos + 4..pos + 8];
        if !kind.iter().all(|b| b.is_ascii_alphabetic()) {
            bail!("malformed PNG chunk type");
        }
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(len) else {
            bail!("PNG chunk length overflow");
        };
        let Some(chunk_end) = data_end.checked_add(4) else {
            bail!("PNG chunk length overflow");
        };
        if chunk_end > bytes.len() {
            bail!("truncated PNG chunk");
        }
        let expected_crc = u32::from_be_bytes([
            bytes[data_end],
            bytes[data_end + 1],
            bytes[data_end + 2],
            bytes[data_end + 3],
        ]);
        let actual_crc = crc32(&bytes[pos + 4..data_end]);
        if actual_crc != expected_crc {
            bail!("PNG chunk CRC mismatch");
        }
        let data = &bytes[data_start..data_end];
        if !saw_ihdr {
            if kind != b"IHDR" || len != 13 {
                bail!("PNG must begin with a 13-byte IHDR chunk");
            }
            let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            if width == 0 || height == 0 || width > 0x7FFF_FFFF || height > 0x7FFF_FFFF {
                bail!("PNG IHDR dimensions are invalid");
            }
            let bit_depth = data[8];
            let color_type = data[9];
            let valid_depth = match color_type {
                0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
                2 | 4 | 6 => matches!(bit_depth, 8 | 16),
                3 => matches!(bit_depth, 1 | 2 | 4 | 8),
                _ => false,
            };
            if !valid_depth || data[10] != 0 || data[11] != 0 || data[12] > 1 {
                bail!("PNG IHDR parameters are invalid");
            }
            needs_plte = color_type == 3;
            saw_ihdr = true;
        } else {
            match kind {
                b"IHDR" => bail!("duplicate PNG IHDR chunk"),
                b"PLTE" => {
                    if len == 0 || !len.is_multiple_of(3) || len > 768 {
                        bail!("invalid PNG PLTE chunk length");
                    }
                    saw_plte = true;
                }
                b"IDAT" => {
                    for &b in data {
                        if idat_head_len < 2 {
                            idat_head[idat_head_len] = b;
                            idat_head_len += 1;
                        } else {
                            break;
                        }
                    }
                    idat_bytes = idat_bytes.saturating_add(len);
                }
                b"IEND" => {
                    if len != 0 {
                        bail!("PNG IEND chunk must be empty");
                    }
                    saw_iend = true;
                }
                _ => {}
            }
        }
        pos = chunk_end;
    }

    if !saw_ihdr || !saw_iend {
        bail!("incomplete PNG container (missing IHDR or IEND)");
    }
    if needs_plte && !saw_plte {
        bail!("indexed-color PNG is missing PLTE chunk");
    }
    if idat_bytes < 6 {
        bail!("PNG is missing compressed IDAT image data");
    }
    let cmf = idat_head[0];
    let flg = idat_head[1];
    if (cmf & 0x0F) != 8
        || (cmf >> 4) > 7
        || !((u16::from(cmf) << 8) | u16::from(flg)).is_multiple_of(31)
    {
        bail!("PNG IDAT stream does not begin with a valid zlib header");
    }
    Ok(())
}

/// One framed JPEG segment or scan returned by [`walk_jpeg_segments`].
struct JpegSegment {
    marker: u8,
    /// Full byte slice start (at the marker's `0xFF`).
    start: usize,
    /// Payload start (after length bytes for length-bearing segments, or at
    /// `start + 2` for standalone markers).
    payload_start: usize,
    /// End of marker header (or end of segment for non-SOS markers).
    header_end: usize,
    /// End of segment including any entropy-coded scan data for SOS (`0xDA`).
    end: usize,
}

/// Walk every JPEG marker segment and entropy-coded scan from `SOI` through
/// `EOI`, skipping byte-stuffed `0xFF 0x00` and restart markers `0xFFD0..=0xFFD7`
/// inside scan data. Rejects truncated segments, nested `SOI`, missing `EOI`,
/// and trailing bytes after `EOI`.
fn walk_jpeg_segments(bytes: &[u8]) -> Result<Vec<JpegSegment>> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        bail!("truncated JPEG SOI header");
    }
    let mut segments = Vec::new();
    let mut pos = 2usize;
    let mut saw_eoi = false;

    while pos < bytes.len() {
        if saw_eoi {
            bail!("trailing bytes after JPEG EOI marker");
        }
        if bytes[pos] != 0xFF {
            bail!("malformed JPEG marker stream");
        }
        let seg_start = pos;
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            bail!("truncated JPEG marker");
        }
        let marker = bytes[pos];
        pos += 1;
        if marker == 0x00 {
            bail!("unexpected stuffed byte outside JPEG scan");
        }
        if marker == 0xD8 {
            bail!("unexpected nested JPEG SOI marker");
        }
        if marker == 0xD9 {
            segments.push(JpegSegment {
                marker,
                start: seg_start,
                payload_start: pos,
                header_end: pos,
                end: pos,
            });
            saw_eoi = true;
            continue;
        }
        if matches!(marker, 0xD0..=0xD7 | 0x01) {
            segments.push(JpegSegment {
                marker,
                start: seg_start,
                payload_start: pos,
                header_end: pos,
                end: pos,
            });
            continue;
        }
        if pos + 2 > bytes.len() {
            bail!("truncated JPEG segment length");
        }
        let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        if len < 2 || pos + len > bytes.len() {
            bail!("invalid JPEG segment length");
        }
        let payload_start = pos + 2;
        let header_end = pos + len;
        if marker == 0xDA {
            // Walk entropy-coded scan until the next non-stuffed, non-restart marker.
            let mut scan = header_end;
            let scan_end = loop {
                if scan >= bytes.len() {
                    bail!("truncated JPEG entropy-coded scan");
                }
                if bytes[scan] != 0xFF {
                    scan += 1;
                    continue;
                }
                let ff_start = scan;
                while scan < bytes.len() && bytes[scan] == 0xFF {
                    scan += 1;
                }
                if scan >= bytes.len() {
                    bail!("truncated JPEG marker inside scan");
                }
                let next = bytes[scan];
                if next == 0x00 || (0xD0..=0xD7).contains(&next) {
                    scan += 1;
                    continue;
                }
                break ff_start;
            };
            if scan_end == header_end {
                bail!("empty JPEG entropy-coded scan");
            }
            segments.push(JpegSegment {
                marker,
                start: seg_start,
                payload_start,
                header_end,
                end: scan_end,
            });
            pos = scan_end;
        } else {
            segments.push(JpegSegment {
                marker,
                start: seg_start,
                payload_start,
                header_end,
                end: header_end,
            });
            pos = header_end;
        }
    }

    if !saw_eoi {
        bail!("incomplete JPEG container (missing EOI marker)");
    }
    Ok(segments)
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

fn validate_jpeg(bytes: &[u8]) -> Result<()> {
    let segments = walk_jpeg_segments(bytes)?;
    let mut saw_sof = false;
    let mut saw_sos = false;
    for seg in &segments {
        if is_jpeg_sof_marker(seg.marker) {
            let payload = &bytes[seg.payload_start..seg.header_end];
            if payload.len() < 6 {
                bail!("truncated JPEG SOF frame header");
            }
            let precision = payload[0];
            let height = u16::from_be_bytes([payload[1], payload[2]]);
            let width = u16::from_be_bytes([payload[3], payload[4]]);
            let components = payload[5] as usize;
            if !matches!(precision, 8 | 12)
                || width == 0
                || height == 0
                || !(1..=4).contains(&components)
                || payload.len() != 6 + 3 * components
            {
                bail!("invalid JPEG SOF frame parameters");
            }
            saw_sof = true;
        } else if seg.marker == 0xDA {
            let payload = &bytes[seg.payload_start..seg.header_end];
            if payload.len() < 4 {
                bail!("truncated JPEG SOS header");
            }
            let components = payload[0] as usize;
            if !(1..=4).contains(&components) || payload.len() != 1 + 2 * components + 3 {
                bail!("invalid JPEG SOS header parameters");
            }
            saw_sos = true;
        }
    }
    if !saw_sof || !saw_sos {
        bail!("incomplete JPEG image (missing SOF frame or SOS scan)");
    }
    Ok(())
}

fn validate_webp_vp8(data: &[u8]) -> Result<()> {
    if data.len() < 11 {
        bail!("truncated WebP VP8 bitstream");
    }
    let frame_tag = u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16);
    let is_keyframe = (frame_tag & 0x01) == 0;
    let version = (frame_tag >> 1) & 0x07;
    let first_part_size = (frame_tag >> 5) as usize;
    if !is_keyframe || version > 3 || &data[3..6] != b"\x9d\x01\x2a" {
        bail!("invalid WebP VP8 keyframe header");
    }
    if first_part_size == 0
        || 10usize
            .checked_add(first_part_size)
            .is_none_or(|end| end > data.len())
    {
        bail!("invalid WebP VP8 first partition length");
    }
    let width = u16::from_le_bytes([data[6], data[7]]) & 0x3FFF;
    let height = u16::from_le_bytes([data[8], data[9]]) & 0x3FFF;
    if width == 0 || height == 0 {
        bail!("WebP VP8 dimensions must be non-zero");
    }
    Ok(())
}

const VP8L_CL_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

const VP8L_DISTANCE_MAP: [(i32, i32); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

/// Bounds for VP8L bitstream validation so crafted headers cannot trigger
/// unbounded memory or CPU work.
const MAX_VP8L_META_PIXELS: usize = 4 * 1024 * 1024;
const MAX_VP8L_PREFIX_GROUPS: usize = 16_384;
const MAX_VP8L_DECODE_STEPS: usize = 33_554_432;

enum Vp8lHuffman {
    Single(u16),
    Pair(u16, u16),
    Tree(Huffman),
}

impl Vp8lHuffman {
    fn from_lengths(lengths: &[u8]) -> Result<Self> {
        let mut nonzero_count = 0usize;
        let mut single_sym = 0u16;
        let mut single_len = 0u8;
        let mut counts = [0u16; 16];
        for (sym, &len) in lengths.iter().enumerate() {
            if len as usize >= counts.len() {
                bail!("invalid WebP VP8L Huffman code length");
            }
            if len > 0 {
                nonzero_count += 1;
                single_sym = sym as u16;
                single_len = len;
                counts[len as usize] += 1;
            }
        }
        if nonzero_count == 0 {
            bail!("empty WebP VP8L Huffman table");
        }
        if nonzero_count == 1 {
            if single_len != 1 {
                bail!("invalid single-symbol WebP VP8L Huffman code length");
            }
            return Ok(Self::Single(single_sym));
        }
        let mut left = 1i32;
        for &count in &counts[1..] {
            left = (left << 1) - i32::from(count);
            if left < 0 {
                bail!("over-subscribed WebP VP8L Huffman tree");
            }
        }
        if left != 0 {
            bail!("under-subscribed WebP VP8L Huffman tree");
        }
        Ok(Self::Tree(Huffman::from_lengths(lengths)?))
    }

    fn single_symbol(&self) -> Option<u16> {
        match self {
            Self::Single(sym) => Some(*sym),
            _ => None,
        }
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16> {
        match self {
            Self::Single(sym) => Ok(*sym),
            Self::Pair(s0, s1) => Ok(if reader.read_bits(1)? == 0 { *s0 } else { *s1 }),
            Self::Tree(tree) => tree.decode(reader),
        }
    }
}

struct Vp8lPrefixGroup {
    green: Vp8lHuffman,
    red: Vp8lHuffman,
    blue: Vp8lHuffman,
    alpha: Vp8lHuffman,
    dist: Vp8lHuffman,
}

fn read_vp8l_prefix_code(reader: &mut BitReader<'_>, alphabet_size: usize) -> Result<Vp8lHuffman> {
    let is_simple = reader.read_bits(1)? != 0;
    if is_simple {
        let num_symbols = usize::from(reader.read_bits(1)?) + 1;
        let is_first_8bits = reader.read_bits(1)? != 0;
        let sym0 = reader.read_bits(if is_first_8bits { 8 } else { 1 })?;
        if usize::from(sym0) >= alphabet_size {
            bail!("WebP VP8L simple Huffman symbol out of range");
        }
        if num_symbols == 1 {
            return Ok(Vp8lHuffman::Single(sym0));
        }
        let sym1 = reader.read_bits(8)?;
        if usize::from(sym1) >= alphabet_size {
            bail!("WebP VP8L simple Huffman symbol out of range");
        }
        return Ok(Vp8lHuffman::Pair(sym0, sym1));
    }

    let num_code_lengths = usize::from(reader.read_bits(4)?) + 4;
    let mut cl_lengths = [0u8; 19];
    for &slot in &VP8L_CL_ORDER[..num_code_lengths] {
        cl_lengths[slot] = reader.read_bits(3)? as u8;
    }
    let cl_tree = Vp8lHuffman::from_lengths(&cl_lengths)?;

    let max_symbol = if reader.read_bits(1)? != 0 {
        let length_nbits = 2 + 2 * u32::from(reader.read_bits(3)?);
        let max_sym = 2 + usize::from(reader.read_bits(length_nbits)?);
        if max_sym > alphabet_size {
            bail!("WebP VP8L max_symbol exceeds alphabet size");
        }
        max_sym
    } else {
        alphabet_size
    };

    let mut code_lengths = vec![0u8; alphabet_size];
    let mut prev_nonzero = 8u8;
    let mut sym = 0usize;
    let mut remaining = max_symbol;
    while sym < alphabet_size && remaining > 0 {
        remaining -= 1;
        let code = cl_tree.decode(reader)?;
        match code {
            0..=15 => {
                code_lengths[sym] = code as u8;
                sym += 1;
                if code != 0 {
                    prev_nonzero = code as u8;
                }
            }
            16 => {
                let repeat = usize::from(reader.read_bits(2)?) + 3;
                if sym + repeat > alphabet_size {
                    bail!("WebP VP8L Huffman repeat overflow");
                }
                code_lengths[sym..sym + repeat].fill(prev_nonzero);
                sym += repeat;
            }
            17 => {
                let repeat = usize::from(reader.read_bits(3)?) + 3;
                if sym + repeat > alphabet_size {
                    bail!("WebP VP8L Huffman zero-repeat overflow");
                }
                sym += repeat;
            }
            18 => {
                let repeat = usize::from(reader.read_bits(7)?) + 11;
                if sym + repeat > alphabet_size {
                    bail!("WebP VP8L Huffman zero-repeat overflow");
                }
                sym += repeat;
            }
            _ => bail!("invalid WebP VP8L code-length symbol"),
        }
    }
    Vp8lHuffman::from_lengths(&code_lengths)
}

fn decode_vp8l_prefix_val(reader: &mut BitReader<'_>, prefix_code: u16) -> Result<usize> {
    if prefix_code < 4 {
        return Ok(usize::from(prefix_code) + 1);
    }
    let extra_bits = u32::from((prefix_code - 2) >> 1);
    let offset = (2 + usize::from(prefix_code & 1)) << extra_bits;
    Ok(offset + usize::from(reader.read_bits(extra_bits)?) + 1)
}

fn decode_vp8l_image_stream(
    reader: &mut BitReader<'_>,
    width: usize,
    height: usize,
    is_top_level: bool,
    capture_meta: bool,
    steps: &mut usize,
) -> Result<Option<Vec<u16>>> {
    let total_pixels = width
        .checked_mul(height)
        .ok_or_else(|| anyhow::anyhow!("WebP VP8L pixel count overflow"))?;
    if total_pixels == 0 {
        bail!("WebP VP8L image stream has zero pixels");
    }
    if capture_meta && total_pixels > MAX_VP8L_META_PIXELS {
        bail!("WebP VP8L meta-prefix image exceeds size limit");
    }

    let use_color_cache = reader.read_bits(1)? != 0;
    let (cc_bits, cc_size) = if use_color_cache {
        let bits = u32::from(reader.read_bits(4)?);
        if !(1..=11).contains(&bits) {
            bail!("invalid WebP VP8L color cache size");
        }
        (bits, 1usize << bits)
    } else {
        (0u32, 0usize)
    };

    let (prefix_bits, prefix_width, meta_groups, num_groups) =
        if is_top_level && reader.read_bits(1)? != 0 {
            let pbits = usize::from(reader.read_bits(3)?) + 2;
            let pwidth = width.div_ceil(1usize << pbits);
            let pheight = height.div_ceil(1usize << pbits);
            let groups = decode_vp8l_image_stream(reader, pwidth, pheight, false, true, steps)?
                .ok_or_else(|| anyhow::anyhow!("missing WebP VP8L meta-prefix image"))?;
            let max_group = groups.iter().copied().max().unwrap_or(0) as usize;
            let count = max_group + 1;
            if count > MAX_VP8L_PREFIX_GROUPS {
                bail!("WebP VP8L prefix-code group count exceeds limit");
            }
            (pbits, pwidth, Some(groups), count)
        } else {
            (0usize, 0usize, None, 1usize)
        };

    let mut groups = Vec::with_capacity(num_groups);
    for _ in 0..num_groups {
        groups.push(Vp8lPrefixGroup {
            green: read_vp8l_prefix_code(reader, 256 + 24 + cc_size)?,
            red: read_vp8l_prefix_code(reader, 256)?,
            blue: read_vp8l_prefix_code(reader, 256)?,
            alpha: read_vp8l_prefix_code(reader, 256)?,
            dist: read_vp8l_prefix_code(reader, 40)?,
        });
    }

    // Fast-path constant 0-bit single-symbol images when not capturing meta-groups
    // so large solid-color lossless images validate in O(1) time.
    if !capture_meta && meta_groups.is_none() {
        let g = &groups[0];
        if let Some(gs) = g.green.single_symbol() {
            if gs < 256
                && g.red.single_symbol().is_some()
                && g.blue.single_symbol().is_some()
                && g.alpha.single_symbol().is_some()
            {
                return Ok(None);
            }
            if gs >= 280 {
                let idx = usize::from(gs - 280);
                if idx >= cc_size {
                    bail!("WebP VP8L color cache index out of bounds");
                }
                return Ok(None);
            }
        }
    }

    let mut out_meta = if capture_meta {
        Vec::with_capacity(total_pixels)
    } else {
        Vec::new()
    };
    let mut out_argb = if capture_meta {
        Vec::with_capacity(total_pixels)
    } else {
        Vec::new()
    };
    let mut cache = if capture_meta && cc_size > 0 {
        vec![0u32; cc_size]
    } else {
        Vec::new()
    };

    let mut px = 0usize;
    while px < total_pixels {
        *steps += 1;
        if *steps > MAX_VP8L_DECODE_STEPS {
            bail!("WebP VP8L image exceeds decoding step bound");
        }
        let group = if let Some(ref mg) = meta_groups {
            let x = px % width;
            let y = px / width;
            let idx = usize::from(mg[(y >> prefix_bits) * prefix_width + (x >> prefix_bits)]);
            &groups[idx]
        } else {
            &groups[0]
        };

        let gs = group.green.decode(reader)?;
        if gs < 256 {
            let rv = group.red.decode(reader)?;
            let bv = group.blue.decode(reader)?;
            let av = group.alpha.decode(reader)?;
            if capture_meta {
                let argb = (u32::from(av) << 24)
                    | (u32::from(rv) << 16)
                    | (u32::from(gs) << 8)
                    | u32::from(bv);
                out_argb.push(argb);
                out_meta.push((rv << 8) | gs);
                if cc_size > 0 {
                    let h = (argb.wrapping_mul(0x1e35_a7bd) >> (32 - cc_bits)) as usize;
                    cache[h] = argb;
                }
            }
            px += 1;
        } else if gs < 280 {
            let length = decode_vp8l_prefix_val(reader, gs - 256)?;
            let dist_sym = group.dist.decode(reader)?;
            let dist_code = decode_vp8l_prefix_val(reader, dist_sym)?;
            let dist = if dist_code > 120 {
                dist_code - 120
            } else {
                let (xi, yi) = VP8L_DISTANCE_MAP[dist_code - 1];
                let d = yi * (width as i32) + xi;
                d.max(1) as usize
            };
            if dist == 0 || dist > px || px.checked_add(length).is_none_or(|end| end > total_pixels)
            {
                bail!("WebP VP8L LZ77 backward reference out of bounds");
            }
            if capture_meta {
                for _ in 0..length {
                    let argb = out_argb[out_argb.len() - dist];
                    out_argb.push(argb);
                    let r = ((argb >> 16) & 0xFF) as u16;
                    let g = ((argb >> 8) & 0xFF) as u16;
                    out_meta.push((r << 8) | g);
                    if cc_size > 0 {
                        let h = (argb.wrapping_mul(0x1e35_a7bd) >> (32 - cc_bits)) as usize;
                        cache[h] = argb;
                    }
                }
            }
            px += length;
        } else {
            let idx = usize::from(gs - 280);
            if idx >= cc_size {
                bail!("WebP VP8L color cache index out of bounds");
            }
            if capture_meta {
                let argb = cache[idx];
                out_argb.push(argb);
                let r = ((argb >> 16) & 0xFF) as u16;
                let g = ((argb >> 8) & 0xFF) as u16;
                out_meta.push((r << 8) | g);
            }
            px += 1;
        }
    }

    Ok(capture_meta.then_some(out_meta))
}

fn validate_webp_vp8l(data: &[u8]) -> Result<()> {
    if data.len() < 8 || data[0] != 0x2F {
        bail!("invalid or truncated WebP VP8L bitstream");
    }
    let header = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
    let width = ((header & 0x3FFF) + 1) as usize;
    let height = (((header >> 14) & 0x3FFF) + 1) as usize;
    let version = (header >> 29) & 0x07;
    if version != 0 {
        bail!("unsupported WebP VP8L version");
    }
    let mut reader = BitReader::new(&data[5..]);
    let mut seen_transforms = [false; 4];
    let mut cur_width = width;
    let mut steps = 0usize;

    while reader.read_bits(1)? == 1 {
        let transform = reader.read_bits(2)? as usize;
        if seen_transforms[transform] {
            bail!("duplicate WebP VP8L transform");
        }
        seen_transforms[transform] = true;
        match transform {
            0 | 1 => {
                let size_bits = usize::from(reader.read_bits(3)?) + 2;
                let block_width = cur_width.div_ceil(1usize << size_bits);
                let block_height = height.div_ceil(1usize << size_bits);
                decode_vp8l_image_stream(
                    &mut reader,
                    block_width,
                    block_height,
                    false,
                    false,
                    &mut steps,
                )?;
            }
            2 => {
                // SubtractGreen transform has no subimage payload.
            }
            3 => {
                let table_size = usize::from(reader.read_bits(8)?) + 1;
                decode_vp8l_image_stream(&mut reader, table_size, 1, false, false, &mut steps)?;
                let xbits = if table_size <= 2 {
                    3
                } else if table_size <= 4 {
                    2
                } else if table_size <= 16 {
                    1
                } else {
                    0
                };
                cur_width = cur_width.div_ceil(1usize << xbits);
            }
            _ => unreachable!(),
        }
    }

    decode_vp8l_image_stream(&mut reader, cur_width, height, true, false, &mut steps)?;
    Ok(())
}

fn validate_webp_alph(data: &[u8]) -> Result<()> {
    if data.len() < 2 {
        bail!("truncated WebP ALPH chunk");
    }
    let compression = data[0] & 0x03;
    if compression > 1 {
        bail!("invalid WebP ALPH compression method");
    }
    Ok(())
}

fn validate_webp_anmf(data: &[u8]) -> Result<()> {
    if data.len() < 24 {
        bail!("truncated WebP ANMF frame");
    }
    let mut pos = 16usize;
    let mut saw_frame_image = false;
    while pos < data.len() {
        if pos + 8 > data.len() {
            bail!("truncated WebP ANMF sub-chunk header");
        }
        let fourcc = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(size) else {
            bail!("WebP ANMF sub-chunk size overflow");
        };
        if data_end > data.len() {
            bail!("truncated WebP ANMF sub-chunk payload");
        }
        let padded_end = data_end
            .checked_add(size & 1)
            .ok_or_else(|| anyhow::anyhow!("WebP ANMF sub-chunk size overflow"))?;
        if padded_end > data.len() {
            bail!("truncated WebP ANMF sub-chunk padding byte");
        }
        let sub_data = &data[data_start..data_end];
        match fourcc {
            b"ALPH" => validate_webp_alph(sub_data)?,
            b"VP8 " => {
                validate_webp_vp8(sub_data)?;
                saw_frame_image = true;
            }
            b"VP8L" => {
                validate_webp_vp8l(sub_data)?;
                saw_frame_image = true;
            }
            _ => bail!("unexpected sub-chunk inside WebP ANMF frame"),
        }
        pos = padded_end;
    }
    if !saw_frame_image {
        bail!("WebP ANMF frame carries no VP8/VP8L image bitstream");
    }
    Ok(())
}

fn validate_webp(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 20 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        bail!("truncated WebP container");
    }
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if riff_size < 12 || riff_size + 8 != bytes.len() {
        bail!("WebP RIFF size does not match file length");
    }
    let mut pos = 12usize;
    let mut saw_image = false;
    let mut saw_vp8x = false;
    let mut vp8x_flags = 0u8;
    let mut saw_anim = false;
    let mut saw_anmf = false;

    while pos < bytes.len() {
        if pos + 8 > bytes.len() {
            bail!("truncated WebP chunk header");
        }
        let fourcc = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(size) else {
            bail!("WebP chunk size overflow");
        };
        if data_end > bytes.len() {
            bail!("truncated WebP chunk payload");
        }
        let padded_end = data_end
            .checked_add(size & 1)
            .ok_or_else(|| anyhow::anyhow!("WebP chunk size overflow"))?;
        if padded_end > bytes.len() {
            bail!("truncated WebP chunk padding byte");
        }
        let data = &bytes[data_start..data_end];
        match fourcc {
            b"VP8 " => {
                validate_webp_vp8(data)?;
                saw_image = true;
            }
            b"VP8L" => {
                validate_webp_vp8l(data)?;
                saw_image = true;
            }
            b"ALPH" => validate_webp_alph(data)?,
            b"VP8X" => {
                if data.len() != 10 {
                    bail!("invalid WebP VP8X header length");
                }
                vp8x_flags = data[0];
                saw_vp8x = true;
            }
            b"ANIM" => {
                if data.len() != 6 {
                    bail!("invalid WebP ANIM chunk length");
                }
                saw_anim = true;
            }
            b"ANMF" => {
                validate_webp_anmf(data)?;
                saw_anmf = true;
            }
            _ => {}
        }
        pos = padded_end;
    }

    if saw_anmf && (!saw_vp8x || !saw_anim) {
        bail!("WebP ANMF frame requires VP8X and ANIM headers");
    }
    if saw_vp8x && (vp8x_flags & 0x02) != 0 && (!saw_anim || !saw_anmf) {
        bail!("animated WebP container missing ANIM or ANMF frames");
    }
    if !saw_image && !(saw_vp8x && saw_anim && saw_anmf) {
        bail!("WebP container carries no VP8/VP8L/ANMF image bitstream");
    }
    Ok(())
}

fn validate_gif(bytes: &[u8]) -> Result<()> {
    if bytes.len() < 14 || (&bytes[0..6] != b"GIF87a" && &bytes[0..6] != b"GIF89a") {
        bail!("truncated GIF header");
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]);
    let height = u16::from_le_bytes([bytes[8], bytes[9]]);
    if width == 0 || height == 0 {
        bail!("GIF logical screen dimensions must be non-zero");
    }
    let mut pos = 13usize;
    let packed = bytes[10];
    if (packed & 0x80) != 0 {
        let gct = 3usize << ((packed & 0x07) + 1);
        pos = pos
            .checked_add(gct)
            .ok_or_else(|| anyhow::anyhow!("GIF color table overflow"))?;
        if pos > bytes.len() {
            bail!("truncated GIF global color table");
        }
    }
    let mut saw_image = false;
    let mut saw_trailer = false;

    while pos < bytes.len() {
        if saw_trailer {
            bail!("trailing bytes after GIF trailer");
        }
        match bytes[pos] {
            0x3B => {
                saw_trailer = true;
                pos += 1;
            }
            0x21 => {
                if pos + 2 > bytes.len() {
                    bail!("truncated GIF extension");
                }
                let Some(next) = gif_skip_subblocks(bytes, pos + 2) else {
                    bail!("truncated GIF extension sub-blocks");
                };
                pos = next;
            }
            0x2C => {
                if pos + 10 > bytes.len() {
                    bail!("truncated GIF image descriptor");
                }
                let img_w = u16::from_le_bytes([bytes[pos + 5], bytes[pos + 6]]);
                let img_h = u16::from_le_bytes([bytes[pos + 7], bytes[pos + 8]]);
                if img_w == 0 || img_h == 0 {
                    bail!("GIF image descriptor dimensions must be non-zero");
                }
                let lpacked = bytes[pos + 9];
                pos += 10;
                if (lpacked & 0x80) != 0 {
                    let lct = 3usize << ((lpacked & 0x07) + 1);
                    pos = pos
                        .checked_add(lct)
                        .ok_or_else(|| anyhow::anyhow!("GIF local color table overflow"))?;
                    if pos > bytes.len() {
                        bail!("truncated GIF local color table");
                    }
                }
                if pos >= bytes.len() {
                    bail!("truncated GIF image LZW header");
                }
                let lzw_min = bytes[pos];
                if !(2..=8).contains(&lzw_min) {
                    bail!("invalid GIF LZW minimum code size");
                }
                pos += 1;
                if pos >= bytes.len() || bytes[pos] == 0 {
                    bail!("GIF image descriptor carries no LZW sub-blocks");
                }
                let Some(next) = gif_skip_subblocks(bytes, pos) else {
                    bail!("truncated GIF image sub-blocks");
                };
                pos = next;
                saw_image = true;
            }
            _ => bail!("unexpected GIF block marker"),
        }
    }

    if !saw_trailer || !saw_image {
        bail!("incomplete GIF container (missing image data or trailer)");
    }
    Ok(())
}

/// Derive a privacy-safe Markdown alt label from a source file's basename stem
/// alone. Never includes directory components, brackets, control characters, or
/// private stretches flagged by `scanner`.
fn safe_alt_from_path(path: &Path, scanner: &Scanner) -> String {
    let raw = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut cleaned = String::new();
    let mut last_space = false;
    for ch in raw.chars() {
        if ch.is_control() || matches!(ch, '[' | ']' | '\\' | '/' | '<' | '>' | '"' | '\'') {
            if !last_space && !cleaned.is_empty() {
                cleaned.push(' ');
                last_space = true;
            }
            continue;
        }
        if ch.is_whitespace() {
            if !last_space && !cleaned.is_empty() {
                cleaned.push(' ');
                last_space = true;
            }
        } else {
            cleaned.push(ch);
            last_space = false;
        }
        if cleaned.len() >= 120 {
            break;
        }
    }
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return "attached image".to_string();
    }
    let stripped = if !scanner.findings(cleaned).is_empty() {
        scanner.strip(cleaned)
    } else {
        cleaned.to_string()
    };
    let final_alt = stripped
        .replace(['[', ']'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if final_alt.is_empty() {
        "attached image".to_string()
    } else {
        final_alt
    }
}

/// Append canonical Markdown image references for `attachments` that are not
/// already referenced as Markdown images in `body`.
pub(crate) fn append_markdown_references(
    body: &str,
    attachments: &[ValidatedAttachment],
) -> String {
    if attachments.is_empty() {
        return body.to_string();
    }
    let existing = referenced_in_body(body);
    let lines: Vec<String> = attachments
        .iter()
        .filter(|att| !existing.contains(&att.id))
        .map(ValidatedAttachment::markdown_line)
        .collect();
    if lines.is_empty() {
        return body.to_string();
    }
    let block = lines.join("\n");
    if body.trim().is_empty() {
        format!("{block}\n")
    } else {
        format!("{}\n\n{block}\n", body.trim_end_matches('\n'))
    }
}

/// Every canonical attachment referenced as a Markdown image in `body`.
pub(crate) fn referenced_in_body(body: &str) -> BTreeSet<AttachmentId> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut ids = BTreeSet::new();
    for event in Parser::new_ext(body, options) {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            if let Some(id) = AttachmentId::parse_rel_path(&dest_url) {
                ids.insert(id);
            }
        }
    }
    ids
}

/// Every attachment referenced in `body` or recorded on `events` for `file`.
pub(crate) fn referenced_for_artifact(
    body: &str,
    events: &[JournalEvent],
    file: &str,
) -> BTreeSet<AttachmentId> {
    let mut ids = referenced_in_body(body);
    for event in events
        .iter()
        .filter(|event| event.known() && event.file.as_deref() == Some(file))
    {
        for att in &event.attachments {
            if let Some(id) = AttachmentId::parse_rel_path(&att.path) {
                ids.insert(id);
            }
        }
    }
    ids
}

// ---------------------------------------------------------------------------
// Confined no-follow filesystem operations
// ---------------------------------------------------------------------------

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

fn c_name(name: &str) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
}

fn open_dir_nofollow(path: &Path) -> io::Result<OwnedFd> {
    let c = c_path(path)?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn open_subdir_nofollow(parent: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    let c = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn open_file_nofollow(dir: &OwnedFd, name: &str) -> io::Result<File> {
    let c = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn fstatat_nofollow(dir: &OwnedFd, name: &str) -> io::Result<libc::stat> {
    let c = c_name(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { stat.assume_init() })
    }
}

fn ensure_attachments_dir(store_dir: &Path, dir_mode: Option<u32>) -> Result<(PathBuf, OwnedFd)> {
    std::fs::create_dir_all(store_dir)
        .with_context(|| format!("cannot create {}", store_dir.display()))?;
    let root_fd = open_dir_nofollow(store_dir)
        .with_context(|| format!("{} must be a real directory", store_dir.display()))?;
    let dir_path = store_dir.join(ATTACHMENTS_DIR);
    match open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR) {
        Ok(fd) => Ok((dir_path, fd)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let mode: libc::mode_t = dir_mode.unwrap_or(0o755) as libc::mode_t;
            let c = c_name(ATTACHMENTS_DIR)?;
            let rc = unsafe { libc::mkdirat(root_fd.as_raw_fd(), c.as_ptr(), mode) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::AlreadyExists {
                    return Err(err)
                        .with_context(|| format!("cannot create {}", dir_path.display()));
                }
            }
            if let Some(m) = dir_mode {
                let _ = std::fs::set_permissions(&dir_path, std::fs::Permissions::from_mode(m));
            }
            let fd = open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR).with_context(|| {
                format!(
                    "{} must be a real directory (not a symbolic link)",
                    dir_path.display()
                )
            })?;
            Ok((dir_path, fd))
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "{} must be a real directory (not a symbolic link)",
                dir_path.display()
            )
        }),
    }
}

/// Read-only preflight verifying that `<store_dir>/attachments/<id>` is either
/// absent or already holds an identical, verified regular blob.
///
/// Refuses symlinked roots/directories, non-regular entries, or corrupt/mismatched
/// destination files before any transaction file or sibling blob is published.
pub(crate) fn check_destination_available(store_dir: &Path, id: &AttachmentId) -> Result<()> {
    let root_fd = match open_dir_nofollow(store_dir) {
        Ok(fd) => fd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("{} must be a real directory", store_dir.display()));
        }
    };
    let att_fd = match open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR) {
        Ok(fd) => fd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "{} must be a real directory (not a symbolic link)",
                    store_dir.join(ATTACHMENTS_DIR).display()
                )
            });
        }
    };
    let name = id.filename();
    match fstatat_nofollow(&att_fd, &name) {
        Ok(stat) => {
            #[allow(clippy::unnecessary_cast)]
            let is_reg = (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFREG as u32;
            if !is_reg {
                bail!("refusing non-regular attachment entry at {}", id.rel_path());
            }
            match verify_opened_blob(&att_fd, id) {
                ReadOutcome::Available { .. } => Ok(()),
                ReadOutcome::Corrupt(reason) => {
                    bail!(
                        "existing attachment {} is corrupt ({reason}); refusing overwrite",
                        id.rel_path()
                    );
                }
                ReadOutcome::Missing => Ok(()),
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| {
            format!(
                "cannot inspect {}",
                store_dir.join(ATTACHMENTS_DIR).join(&name).display()
            )
        }),
    }
}

/// Atomically store verified attachment bytes under
/// `<store_dir>/attachments/<sha256>.<ext>`.
///
/// Deduplicates when an identical verified blob already exists; refuses when
/// the destination is a symlink, non-regular file, or holds different bytes.
pub(crate) fn store_bytes(
    store_dir: &Path,
    id: &AttachmentId,
    bytes: &[u8],
    file_mode: Option<u32>,
    dir_mode: Option<u32>,
) -> Result<()> {
    let detected = validate_image_bytes(bytes)?;
    if detected != id.format {
        bail!(
            "attachment {} bytes do not match expected format {}",
            id.rel_path(),
            id.format.media_type()
        );
    }
    let actual_hash = hex::encode(Sha256::digest(bytes));
    if actual_hash != id.sha256 {
        bail!(
            "attachment {} digest mismatch (expected {}, got {actual_hash})",
            id.rel_path(),
            id.sha256
        );
    }
    let (att_dir, att_fd) = ensure_attachments_dir(store_dir, dir_mode)?;
    let name = id.filename();
    match fstatat_nofollow(&att_fd, &name) {
        Ok(stat) => {
            #[allow(clippy::unnecessary_cast)]
            let is_reg = (stat.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFREG as u32;
            if !is_reg {
                bail!("refusing non-regular attachment entry at {}", id.rel_path());
            }
            match verify_opened_blob(&att_fd, id) {
                ReadOutcome::Available { .. } => return Ok(()),
                ReadOutcome::Corrupt(reason) => {
                    bail!(
                        "existing attachment {} is corrupt ({reason}); refusing overwrite",
                        id.rel_path()
                    );
                }
                ReadOutcome::Missing => {}
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("cannot inspect {}", att_dir.join(&name).display()));
        }
    }

    let tmp_name = format!(".{}.{}.tmp", name, ulid::Ulid::new());
    let tmp_path = att_dir.join(&tmp_name);
    let dest_path = att_dir.join(&name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    if let Some(mode) = file_mode {
        opts.mode(mode);
    }
    let mut out = opts
        .open(&tmp_path)
        .with_context(|| format!("cannot stage attachment {}", tmp_path.display()))?;
    if let Err(err) = out.write_all(bytes).and_then(|()| out.sync_all()) {
        drop(out);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("cannot write attachment {}", id.rel_path()));
    }
    drop(out);

    let linked = std::fs::hard_link(&tmp_path, &dest_path);
    let _ = std::fs::remove_file(&tmp_path);
    match linked {
        Ok(()) => {
            let _ = unsafe { libc::fsync(att_fd.as_raw_fd()) };
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            match verify_opened_blob(&att_fd, id) {
                ReadOutcome::Available { .. } => Ok(()),
                ReadOutcome::Corrupt(reason) => bail!(
                    "existing attachment {} is corrupt ({reason})",
                    id.rel_path()
                ),
                ReadOutcome::Missing => {
                    bail!("attachment {} disappeared during publish", id.rel_path())
                }
            }
        }
        Err(err) => {
            Err(err).with_context(|| format!("cannot publish attachment {}", id.rel_path()))
        }
    }
}

fn verify_opened_blob(att_fd: &OwnedFd, id: &AttachmentId) -> ReadOutcome {
    let name = id.filename();
    let mut file = match open_file_nofollow(att_fd, &name) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return ReadOutcome::Missing,
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            return ReadOutcome::Corrupt("symbolic link is not allowed".to_string());
        }
        Err(err) => return ReadOutcome::Corrupt(format!("cannot open: {err}")),
    };
    let meta = match file.metadata() {
        Ok(meta) => meta,
        Err(err) => return ReadOutcome::Corrupt(format!("cannot stat: {err}")),
    };
    if !meta.is_file() {
        return ReadOutcome::Corrupt("not a regular file".to_string());
    }
    if meta.len() == 0 {
        return ReadOutcome::Corrupt("empty file".to_string());
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        return ReadOutcome::Corrupt(format!("exceeds {MAX_ATTACHMENT_BYTES}-byte limit"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    if let Err(err) = Read::by_ref(&mut file)
        .take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
    {
        return ReadOutcome::Corrupt(format!("read failed: {err}"));
    }
    if bytes.is_empty() || bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return ReadOutcome::Corrupt("size out of bounds".to_string());
    }
    match validate_image_bytes(&bytes) {
        Ok(fmt) if fmt == id.format => {}
        Ok(_) => {
            return ReadOutcome::Corrupt(format!(
                "container format does not match {}",
                id.format.media_type()
            ));
        }
        Err(err) => return ReadOutcome::Corrupt(err.to_string()),
    }
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != id.sha256 {
        return ReadOutcome::Corrupt("content hash mismatch".to_string());
    }
    ReadOutcome::Available { bytes }
}

/// Read and verify one attachment strictly from one storage root (`hot` or `cold`).
pub(crate) fn read_verified_in_store(store_dir: &Path, id: &AttachmentId) -> ReadOutcome {
    let root_fd = match open_dir_nofollow(store_dir) {
        Ok(fd) => fd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return ReadOutcome::Missing,
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            return ReadOutcome::Corrupt("journal directory is a symbolic link".to_string());
        }
        Err(err) => return ReadOutcome::Corrupt(format!("cannot open store: {err}")),
    };
    let att_fd = match open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR) {
        Ok(fd) => fd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return ReadOutcome::Missing,
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            return ReadOutcome::Corrupt("attachments directory is a symbolic link".to_string());
        }
        Err(err) => return ReadOutcome::Corrupt(format!("cannot open attachments dir: {err}")),
    };
    verify_opened_blob(&att_fd, id)
}

/// Read and verify one attachment from `hot` first, then its cold sibling archive.
pub(crate) fn read_verified(hot: &Path, id: &AttachmentId) -> ReadOutcome {
    match read_verified_in_store(hot, id) {
        ReadOutcome::Missing => read_verified_in_store(&archive_dir(hot), id),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Lifecycle transfer: archive, unarchive, relocate, promote
// ---------------------------------------------------------------------------

/// Collect all attachments referenced by regular artifact files currently in
/// `store_dir`, excluding `skip_files` and relocated tombstones.
fn attachments_used_in_store(
    store_dir: &Path,
    events: &[JournalEvent],
    skip_files: &BTreeSet<&str>,
) -> Result<BTreeSet<AttachmentId>> {
    let mut used = BTreeSet::new();
    if !store_dir.is_dir() {
        return Ok(used);
    }
    for entry in std::fs::read_dir(store_dir)
        .with_context(|| format!("cannot read {}", store_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if parse_artifact_name(&name).is_none()
            || skip_files.contains(name.as_str())
            || relocation(events, &name).is_some()
        {
            continue;
        }
        if let Ok(body) = std::fs::read_to_string(entry.path()) {
            used.extend(referenced_for_artifact(&body, events, &name));
        }
    }
    Ok(used)
}

/// Copy every attachment referenced by `filename` (body + events) from
/// `source_dir` into `dest_dir`, verifying hash and container structure.
pub(super) fn copy_for_artifact(
    source_dir: &Path,
    dest_dir: &Path,
    body: &str,
    events: &[JournalEvent],
    filename: &str,
) -> Result<Vec<AttachmentId>> {
    let ids = referenced_for_artifact(body, events, filename);
    let mut copied = Vec::new();
    for id in ids {
        match read_verified_in_store(source_dir, &id) {
            ReadOutcome::Available { bytes } => {
                store_bytes(dest_dir, &id, &bytes, None, None)?;
                copied.push(id);
            }
            ReadOutcome::Missing => {
                // If already present and verified in dest_dir (e.g. retry after
                // partial move), accept it; otherwise fail loudly.
                match read_verified_in_store(dest_dir, &id) {
                    ReadOutcome::Available { .. } => copied.push(id),
                    ReadOutcome::Corrupt(reason) => bail!(
                        "{filename} references {} which is corrupt in destination ({reason})",
                        id.rel_path()
                    ),
                    ReadOutcome::Missing => bail!(
                        "{filename} references {}, which is missing from {}",
                        id.rel_path(),
                        source_dir.display()
                    ),
                }
            }
            ReadOutcome::Corrupt(reason) => {
                bail!(
                    "{filename} references {}, which is corrupt ({reason})",
                    id.rel_path()
                );
            }
        }
    }
    Ok(copied)
}

/// Remove unreferenced blobs from `store_dir/attachments/` after an artifact
/// leaves `store_dir`.
///
/// Blobs still referenced by any remaining artifact in `store_dir` stay in
/// place. If a surviving artifact in `sibling_dir` (the other storage tier)
/// still references a candidate blob, a verified copy is ensured in
/// `sibling_dir/attachments/` before unlinking the copy in `store_dir` so no
/// surviving artifact loses its only journal copy.
pub(crate) fn prune_unreferenced_from_store(
    store_dir: &Path,
    sibling_dir: &Path,
    events: &[JournalEvent],
    candidates: &[AttachmentId],
    leaving_files: &BTreeSet<&str>,
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    let still_used_here = attachments_used_in_store(store_dir, events, leaving_files)?;
    let still_used_sibling = attachments_used_in_store(sibling_dir, events, leaving_files)?;
    let Ok(root_fd) = open_dir_nofollow(store_dir) else {
        return Ok(());
    };
    let Ok(att_fd) = open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR) else {
        return Ok(());
    };
    let mut removed_any = false;
    for id in candidates {
        if still_used_here.contains(id) {
            continue;
        }
        if still_used_sibling.contains(id)
            && !matches!(
                read_verified_in_store(sibling_dir, id),
                ReadOutcome::Available { .. }
            )
        {
            match verify_opened_blob(&att_fd, id) {
                ReadOutcome::Available { bytes } => {
                    store_bytes(sibling_dir, id, &bytes, None, None)?;
                }
                ReadOutcome::Missing | ReadOutcome::Corrupt(_) => {
                    // Never delete when we cannot ensure a verified copy for the surviving reference.
                    continue;
                }
            }
        }
        let name = id.filename();
        let c = c_name(&name)?;
        let rc = unsafe { libc::unlinkat(att_fd.as_raw_fd(), c.as_ptr(), 0) };
        if rc == 0 {
            removed_any = true;
        } else {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::NotFound {
                return Err(err)
                    .with_context(|| format!("cannot remove attachment {}", id.rel_path()));
            }
        }
    }
    if removed_any {
        let _ = unsafe { libc::fsync(att_fd.as_raw_fd()) };
    }
    Ok(())
}

/// Check that every attachment referenced by `filename` can be relocated to
/// `destination_dir` (exists and verifies in journal or already matches in
/// `destination_dir`, with no conflicting file at destination).
pub(crate) fn plan_relocation_attachments(
    hot: &Path,
    events: &[JournalEvent],
    filename: &str,
    body: &str,
    destination_dir: &Path,
) -> Result<Vec<AttachmentId>> {
    let ids = referenced_for_artifact(body, events, filename);
    let mut planned = Vec::new();
    for id in ids {
        let dest_blob = destination_dir.join(ATTACHMENTS_DIR).join(id.filename());
        let already_dest = match std::fs::symlink_metadata(&dest_blob) {
            Ok(meta) => {
                if !meta.is_file() {
                    bail!(
                        "{} already exists and is not a regular file; nothing was relocated",
                        dest_blob.display()
                    );
                }
                match read_verified_in_store(destination_dir, &id) {
                    ReadOutcome::Available { .. } => true,
                    ReadOutcome::Corrupt(_) | ReadOutcome::Missing => {
                        bail!(
                            "{} already exists with different or corrupt content; nothing was relocated",
                            dest_blob.display()
                        );
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => {
                return Err(err).with_context(|| format!("cannot inspect {}", dest_blob.display()));
            }
        };
        if !already_dest {
            match read_verified(hot, &id) {
                ReadOutcome::Available { .. } => {}
                ReadOutcome::Missing => {
                    bail!(
                        "{filename} references {}, which is missing from the journal; nothing was relocated",
                        id.rel_path()
                    );
                }
                ReadOutcome::Corrupt(reason) => {
                    bail!(
                        "{filename} references {}, which is corrupt ({reason}); nothing was relocated",
                        id.rel_path()
                    );
                }
            }
        }
        planned.push(id);
    }
    Ok(planned)
}

/// Copy planned attachments to `destination_dir/attachments/` with mode 0600
/// (directory mode 0700) and verify them before any journal files are removed.
pub(crate) fn copy_relocation_attachments(
    hot: &Path,
    destination_dir: &Path,
    ids: &[AttachmentId],
) -> Result<()> {
    for id in ids {
        if matches!(
            read_verified_in_store(destination_dir, id),
            ReadOutcome::Available { .. }
        ) {
            continue;
        }
        match read_verified(hot, id) {
            ReadOutcome::Available { bytes } => {
                store_bytes(destination_dir, id, &bytes, Some(0o600), Some(0o700))?;
            }
            ReadOutcome::Missing => {
                bail!("attachment {} is missing during relocation", id.rel_path());
            }
            ReadOutcome::Corrupt(reason) => {
                bail!(
                    "attachment {} is corrupt during relocation ({reason})",
                    id.rel_path()
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Doctor inspection
// ---------------------------------------------------------------------------

/// Inspect `<hot>/attachments/` and `<cold>/attachments/` for integrity,
/// confinement, missing (dangling), and orphaned attachments.
pub(super) fn inspect(
    hot: &Path,
    cold: &Path,
    events: &[JournalEvent],
    problems: &mut Vec<DoctorFinding>,
    advice: &mut Vec<DoctorFinding>,
) -> Result<()> {
    let mut referenced_by_tier: BTreeMap<StorageTier, BTreeSet<AttachmentId>> = BTreeMap::new();
    referenced_by_tier.insert(StorageTier::Hot, BTreeSet::new());
    referenced_by_tier.insert(StorageTier::Cold, BTreeSet::new());

    // Gather all attachment references from non-relocated hot and cold artifacts.
    for (dir, tier, tier_label) in [
        (hot, StorageTier::Hot, "hot"),
        (cold, StorageTier::Cold, "cold"),
    ] {
        if !dir.is_dir() {
            continue;
        }
        let mut names = Vec::new();
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if parse_artifact_name(&name).is_some() && relocation(events, &name).is_none() {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            let Ok(body) = std::fs::read_to_string(dir.join(&name)) else {
                continue;
            };
            let refs = referenced_for_artifact(&body, events, &name);
            for id in refs {
                referenced_by_tier
                    .entry(tier)
                    .or_default()
                    .insert(id.clone());
                match read_verified_in_store(dir, &id) {
                    ReadOutcome::Available { .. } => {}
                    ReadOutcome::Missing => {
                        problems.push(DoctorFinding {
                            code: "missing-attachment",
                            detail: format!("{name} ({tier_label}): {} is missing", id.rel_path()),
                        });
                    }
                    ReadOutcome::Corrupt(reason) => {
                        problems.push(DoctorFinding {
                            code: "corrupt-attachment",
                            detail: format!(
                                "{name} ({tier_label}): {} is corrupt ({reason})",
                                id.rel_path()
                            ),
                        });
                    }
                }
            }
        }
    }

    // Inspect the attachment directories themselves for symlinks, malformed
    // entries, corrupt blobs, and unreferenced orphans.
    for (dir, tier, tier_label) in [
        (hot, StorageTier::Hot, "hot"),
        (cold, StorageTier::Cold, "cold"),
    ] {
        let att_path = dir.join(ATTACHMENTS_DIR);
        let meta = match std::fs::symlink_metadata(&att_path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                problems.push(DoctorFinding {
                    code: "invalid-attachment-store",
                    detail: format!("{tier_label} attachments/: {err}"),
                });
                continue;
            }
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            problems.push(DoctorFinding {
                code: "invalid-attachment-store",
                detail: format!("{tier_label} attachments/ is not a real directory"),
            });
            continue;
        }
        let Ok(root_fd) = open_dir_nofollow(dir) else {
            continue;
        };
        let Ok(att_fd) = open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR) else {
            problems.push(DoctorFinding {
                code: "invalid-attachment-store",
                detail: format!(
                    "{tier_label} attachments/ cannot be opened without following links"
                ),
            });
            continue;
        };
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&att_path)
            .with_context(|| format!("cannot read {}", att_path.display()))?
        {
            let entry = entry?;
            entries.push(entry.file_name().to_string_lossy().to_string());
        }
        entries.sort();
        let referenced = referenced_by_tier.get(&tier).cloned().unwrap_or_default();
        for name in entries {
            let stat = match fstatat_nofollow(&att_fd, &name) {
                Ok(stat) => stat,
                Err(err) => {
                    problems.push(DoctorFinding {
                        code: "malformed-attachment",
                        detail: format!("{tier_label} {ATTACHMENTS_DIR}/{name}: {err}"),
                    });
                    continue;
                }
            };
            #[allow(clippy::unnecessary_cast)]
            let (is_link, is_reg) = {
                let kind = stat.st_mode as u32 & libc::S_IFMT as u32;
                (kind == libc::S_IFLNK as u32, kind == libc::S_IFREG as u32)
            };
            if is_link {
                problems.push(DoctorFinding {
                    code: "symlink-attachment",
                    detail: format!("{tier_label} {ATTACHMENTS_DIR}/{name} is a symbolic link"),
                });
                continue;
            }
            if !is_reg {
                problems.push(DoctorFinding {
                    code: "malformed-attachment",
                    detail: format!("{tier_label} {ATTACHMENTS_DIR}/{name} is not a regular file"),
                });
                continue;
            }
            let Some(id) = AttachmentId::parse_filename(&name) else {
                problems.push(DoctorFinding {
                    code: "malformed-attachment",
                    detail: format!("{tier_label} {ATTACHMENTS_DIR}/{name}"),
                });
                continue;
            };
            // Verify unreferenced blobs too so corruption never hides in an orphan.
            if !referenced.contains(&id) {
                match verify_opened_blob(&att_fd, &id) {
                    ReadOutcome::Available { .. } => {
                        advice.push(DoctorFinding {
                            code: "orphaned-attachment",
                            detail: format!(
                                "{tier_label} {} is not referenced by any {tier_label} artifact",
                                id.rel_path()
                            ),
                        });
                    }
                    ReadOutcome::Corrupt(reason) => {
                        problems.push(DoctorFinding {
                            code: "corrupt-attachment",
                            detail: format!("{tier_label} {}: {reason}", id.rel_path()),
                        });
                    }
                    ReadOutcome::Missing => {}
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Image metadata privacy scanning & stripping
// ---------------------------------------------------------------------------

/// Result of scanning an image's metadata chunks: matched private stretches
/// plus any compressed/structured metadata chunks that could not be inspected.
#[derive(Debug, Default)]
pub(super) struct MetadataScan {
    pub(super) findings: Vec<Finding>,
    pub(super) unreadable: Vec<String>,
}

/// Scan textual and compressed metadata chunks embedded inside a validated
/// raster image for private paths, addresses, or hostnames using `scanner`.
pub(super) fn scan_metadata(format: ImageFormat, bytes: &[u8], scanner: &Scanner) -> MetadataScan {
    let mut scan = MetadataScan::default();
    let (texts, unreadable) = extract_metadata_strings(format, bytes);
    for text in texts {
        scan.findings.extend(scanner.findings(&text));
    }
    scan.unreadable = unreadable;
    scan
}

/// Remove textual/EXIF/XMP/comment metadata chunks from a validated raster
/// image so rendered `data:` URIs never carry private metadata under
/// `--strip-private`, validating the resulting container before returning it.
pub(crate) fn strip_metadata(format: ImageFormat, bytes: &[u8]) -> Result<Vec<u8>> {
    let stripped = match format {
        ImageFormat::Png => strip_png_metadata(bytes)?,
        ImageFormat::Jpeg => strip_jpeg_metadata(bytes)?,
        ImageFormat::Webp => strip_webp_metadata(bytes)?,
        ImageFormat::Gif => strip_gif_metadata(bytes)?,
    };
    let validated =
        validate_image_bytes(&stripped).context("stripped image failed container validation")?;
    if validated != format {
        bail!("stripped image format changed unexpectedly");
    }
    Ok(stripped)
}

fn extract_metadata_strings(format: ImageFormat, bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    match format {
        ImageFormat::Png => extract_png_metadata(bytes),
        ImageFormat::Jpeg => (extract_jpeg_metadata(bytes), Vec::new()),
        ImageFormat::Webp => (extract_webp_metadata(bytes), Vec::new()),
        ImageFormat::Gif => (extract_gif_metadata(bytes), Vec::new()),
    }
}

fn printable_runs(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for &b in data {
        if matches!(b, 0x20..=0x7E | b'\t' | b'\n' | b'\r') {
            cur.push(b);
        } else if cur.len() >= 4 {
            if let Ok(s) = String::from_utf8(std::mem::take(&mut cur)) {
                out.push(s);
            }
        } else {
            cur.clear();
        }
    }
    if cur.len() >= 4 {
        if let Ok(s) = String::from_utf8(cur) {
            out.push(s);
        }
    }
    out
}

/// Extract printable UTF-16LE and UTF-16BE ASCII runs at both even and odd byte
/// offsets so EXIF `UserComment` (`UNICODE\0`) and Windows `XP*` tags are
/// scanned alongside single-byte metadata text.
fn utf16_printable_runs(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for offset in [0usize, 1usize] {
        if data.len() < offset + 8 {
            continue;
        }
        let slice = &data[offset..];
        for big_endian in [false, true] {
            let mut cur = Vec::new();
            for pair in slice.chunks_exact(2) {
                let unit = if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                };
                if matches!(unit, 0x0020..=0x007E | 0x0009 | 0x000A | 0x000D) {
                    cur.push(unit as u8);
                } else if cur.len() >= 4 {
                    if let Ok(s) = String::from_utf8(std::mem::take(&mut cur)) {
                        out.push(s);
                    }
                } else {
                    cur.clear();
                }
            }
            if cur.len() >= 4 {
                if let Ok(s) = String::from_utf8(cur) {
                    out.push(s);
                }
            }
        }
    }
    out
}

fn metadata_text_views(data: &[u8]) -> Vec<String> {
    let mut out = printable_runs(data);
    out.extend(utf16_printable_runs(data));
    out
}

fn push_utf8_metadata_field(out: &mut Vec<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    if text.contains('\0') {
        out.push(text.replace('\0', "\n"));
    } else {
        out.push(text.to_string());
    }
}

fn extract_png_metadata(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut out = Vec::new();
    let mut unreadable = Vec::new();
    let mut pos = 8;
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let Some(chunk_end) = data_start.checked_add(len).and_then(|n| n.checked_add(4)) else {
            break;
        };
        if chunk_end > bytes.len() {
            break;
        }
        let data = &bytes[data_start..data_start + len];
        match kind {
            b"tEXt" | b"eXIf" => out.extend(metadata_text_views(data)),
            b"zTXt" => {
                if let Some(nul) = data.iter().position(|&b| b == 0) {
                    out.extend(metadata_text_views(&data[..nul]));
                    if nul + 2 <= data.len() && data[nul + 1] == 0 {
                        match inflate_zlib_bounded(&data[nul + 2..], MAX_METADATA_DECOMPRESS_BYTES)
                        {
                            Ok(decompressed) => out.extend(metadata_text_views(&decompressed)),
                            Err(err) => {
                                unreadable.push(format!("unreadable PNG zTXt chunk: {err}"))
                            }
                        }
                    } else {
                        unreadable.push(
                            "unreadable PNG zTXt chunk: unsupported compression method".to_string(),
                        );
                    }
                } else {
                    unreadable
                        .push("unreadable PNG zTXt chunk: missing keyword delimiter".to_string());
                }
            }
            b"iTXt" => {
                // <keyword>\0<compression_flag><compression_method><lang>\0<translated>\0<text>
                if let Some(k_nul) = data.iter().position(|&b| b == 0) {
                    out.extend(metadata_text_views(&data[..k_nul]));
                    if k_nul + 3 <= data.len() {
                        let comp_flag = data[k_nul + 1];
                        let comp_method = data[k_nul + 2];
                        let rest = &data[k_nul + 3..];
                        if let Some(l_nul) = rest.iter().position(|&b| b == 0) {
                            let lang_bytes = &rest[..l_nul];
                            match std::str::from_utf8(lang_bytes) {
                                Ok(lang) => push_utf8_metadata_field(&mut out, lang),
                                Err(_) => unreadable.push(
                                    "unreadable PNG iTXt chunk: invalid UTF-8 language tag"
                                        .to_string(),
                                ),
                            }
                            let after_lang = &rest[l_nul + 1..];
                            if let Some(t_nul) = after_lang.iter().position(|&b| b == 0) {
                                let trans_bytes = &after_lang[..t_nul];
                                match std::str::from_utf8(trans_bytes) {
                                    Ok(trans) => push_utf8_metadata_field(&mut out, trans),
                                    Err(_) => unreadable.push(
                                        "unreadable PNG iTXt chunk: invalid UTF-8 translated keyword"
                                            .to_string(),
                                    ),
                                }
                                let text_bytes = &after_lang[t_nul + 1..];
                                match (comp_flag, comp_method) {
                                    (0, 0) => match std::str::from_utf8(text_bytes) {
                                        Ok(text) => push_utf8_metadata_field(&mut out, text),
                                        Err(_) => unreadable.push(
                                            "unreadable PNG iTXt chunk: invalid UTF-8 text"
                                                .to_string(),
                                        ),
                                    },
                                    (1, 0) => {
                                        match inflate_zlib_bounded(
                                            text_bytes,
                                            MAX_METADATA_DECOMPRESS_BYTES,
                                        ) {
                                            Ok(decompressed) => {
                                                match std::str::from_utf8(&decompressed) {
                                                    Ok(text) => {
                                                        push_utf8_metadata_field(&mut out, text)
                                                    }
                                                    Err(_) => unreadable.push(
                                                        "unreadable compressed PNG iTXt chunk: invalid UTF-8 text"
                                                            .to_string(),
                                                    ),
                                                }
                                            }
                                            Err(err) => unreadable.push(format!(
                                                "unreadable compressed PNG iTXt chunk: {err}"
                                            )),
                                        }
                                    }
                                    _ => unreadable.push(
                                        "unreadable PNG iTXt chunk: unsupported compression"
                                            .to_string(),
                                    ),
                                }
                            } else {
                                unreadable.push(
                                    "unreadable PNG iTXt chunk: missing translated keyword delimiter"
                                        .to_string(),
                                );
                            }
                        } else {
                            unreadable.push(
                                "unreadable PNG iTXt chunk: missing language tag delimiter"
                                    .to_string(),
                            );
                        }
                    } else {
                        unreadable.push("unreadable PNG iTXt chunk: truncated header".to_string());
                    }
                } else {
                    unreadable
                        .push("unreadable PNG iTXt chunk: missing keyword delimiter".to_string());
                }
            }
            _ => {}
        }
        pos = chunk_end;
    }
    (out, unreadable)
}

fn strip_png_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 8 {
        bail!("truncated PNG header");
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..8]);
    let mut pos = 8;
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let Some(chunk_end) = (pos + 8).checked_add(len).and_then(|n| n.checked_add(4)) else {
            bail!("malformed PNG chunk length");
        };
        if chunk_end > bytes.len() {
            bail!("truncated PNG chunk");
        }
        if !matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf") {
            out.extend_from_slice(&bytes[pos..chunk_end]);
        }
        pos = chunk_end;
    }
    Ok(out)
}

fn is_jpeg_metadata_marker(marker: u8) -> bool {
    marker == 0xFE || (0xE1..=0xEF).contains(&marker)
}

fn extract_jpeg_metadata(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(segments) = walk_jpeg_segments(bytes) else {
        return out;
    };
    for seg in segments {
        if is_jpeg_metadata_marker(seg.marker) {
            out.extend(metadata_text_views(
                &bytes[seg.payload_start..seg.header_end],
            ));
        } else if seg.marker == 0xE0 {
            let payload = &bytes[seg.payload_start..seg.header_end];
            if !payload.starts_with(b"JFIF\0") || payload.len() > 14 {
                out.extend(metadata_text_views(payload));
            }
        }
    }
    out
}

fn strip_jpeg_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    let segments = walk_jpeg_segments(bytes)?;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..2]); // SOI
    for seg in segments {
        let drop_app0 = seg.marker == 0xE0 && {
            let payload = &bytes[seg.payload_start..seg.header_end];
            !payload.starts_with(b"JFIF\0") || payload.len() > 14
        };
        if !is_jpeg_metadata_marker(seg.marker) && !drop_app0 {
            out.extend_from_slice(&bytes[seg.start..seg.end]);
        }
    }
    Ok(out)
}

fn extract_webp_metadata(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let fourcc = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let data_start = pos + 8;
        let padded = size.saturating_add(size & 1);
        let Some(chunk_end) = data_start.checked_add(padded) else {
            break;
        };
        if data_start + size > bytes.len() {
            break;
        }
        if matches!(fourcc, b"EXIF" | b"XMP ") {
            out.extend(metadata_text_views(&bytes[data_start..data_start + size]));
        }
        pos = chunk_end.min(bytes.len());
    }
    out
}

fn strip_webp_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        bail!("truncated WebP header");
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..12]);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let fourcc = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let data_start = pos + 8;
        let padded = size.saturating_add(size & 1);
        let Some(chunk_end) = data_start.checked_add(padded) else {
            bail!("malformed WebP chunk length");
        };
        if data_start + size > bytes.len() {
            bail!("truncated WebP chunk");
        }
        let end = chunk_end.min(bytes.len());
        if !matches!(fourcc, b"EXIF" | b"XMP ") {
            let chunk_start_out = out.len();
            out.extend_from_slice(&bytes[pos..end]);
            // Clear EXIF (bit 3) and XMP (bit 2) flags in VP8X when present.
            if fourcc == b"VP8X" && size >= 1 {
                out[chunk_start_out + 8] &= !(0x08 | 0x04);
            }
        }
        pos = end;
    }
    let riff_size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());
    Ok(out)
}

fn gif_skip_subblocks(bytes: &[u8], mut pos: usize) -> Option<usize> {
    while pos < bytes.len() {
        let len = bytes[pos] as usize;
        pos += 1;
        if len == 0 {
            return Some(pos);
        }
        pos = pos.checked_add(len)?;
        if pos > bytes.len() {
            return None;
        }
    }
    None
}

fn extract_gif_metadata(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if bytes.len() < 13 {
        return out;
    }
    let mut pos: usize = 13;
    let packed = bytes[10];
    if (packed & 0x80) != 0 {
        let gct = 3usize << ((packed & 0x07) + 1);
        pos = pos.saturating_add(gct);
    }
    while pos < bytes.len() {
        match bytes[pos] {
            0x3B => break,
            0x21 => {
                if pos + 2 > bytes.len() {
                    break;
                }
                let label = bytes[pos + 1];
                let mut sub = pos + 2;
                let mut payload = Vec::new();
                while sub < bytes.len() {
                    let len = bytes[sub] as usize;
                    sub += 1;
                    if len == 0 {
                        break;
                    }
                    if sub + len > bytes.len() {
                        return out;
                    }
                    if label == 0xFE || label == 0xFF {
                        payload.extend_from_slice(&bytes[sub..sub + len]);
                    }
                    sub += len;
                }
                if label == 0xFE || (label == 0xFF && payload.starts_with(b"XMP DataXMP")) {
                    out.extend(metadata_text_views(&payload));
                }
                pos = sub;
            }
            0x2C => {
                if pos + 10 > bytes.len() {
                    break;
                }
                let lpacked = bytes[pos + 9];
                pos += 10;
                if (lpacked & 0x80) != 0 {
                    let lct = 3usize << ((lpacked & 0x07) + 1);
                    pos = pos.saturating_add(lct);
                }
                if pos >= bytes.len() {
                    break;
                }
                pos += 1; // LZW minimum code size
                let Some(next) = gif_skip_subblocks(bytes, pos) else {
                    break;
                };
                pos = next;
            }
            _ => break,
        }
    }
    out
}

fn strip_gif_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 13 {
        bail!("truncated GIF header");
    }
    let mut header_end = 13;
    let packed = bytes[10];
    if (packed & 0x80) != 0 {
        header_end += 3usize << ((packed & 0x07) + 1);
    }
    if header_end > bytes.len() {
        bail!("truncated GIF color table");
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..header_end]);
    let mut pos = header_end;
    while pos < bytes.len() {
        match bytes[pos] {
            0x3B => {
                out.push(0x3B);
                break;
            }
            0x21 => {
                if pos + 2 > bytes.len() {
                    bail!("truncated GIF extension");
                }
                let label = bytes[pos + 1];
                let Some(end) = gif_skip_subblocks(bytes, pos + 2) else {
                    bail!("truncated GIF extension sub-blocks");
                };
                let is_xmp = label == 0xFF
                    && bytes
                        .get(pos + 2..pos + 14)
                        .is_some_and(|head| head[0] == 11 && &head[1..12] == b"XMP DataXMP");
                if label != 0xFE && !is_xmp {
                    out.extend_from_slice(&bytes[pos..end]);
                }
                pos = end;
            }
            0x2C => {
                if pos + 10 > bytes.len() {
                    bail!("truncated GIF image descriptor");
                }
                let lpacked = bytes[pos + 9];
                let mut cursor = pos + 10;
                if (lpacked & 0x80) != 0 {
                    cursor += 3usize << ((lpacked & 0x07) + 1);
                }
                if cursor >= bytes.len() {
                    bail!("truncated GIF image data");
                }
                cursor += 1; // LZW min code size
                let Some(end) = gif_skip_subblocks(bytes, cursor) else {
                    bail!("truncated GIF image sub-blocks");
                };
                out.extend_from_slice(&bytes[pos..end]);
                pos = end;
            }
            _ => bail!("unexpected GIF block marker"),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Checksums & bounded RFC 1950 / RFC 1951 (zlib / DEFLATE) decompressor
// ---------------------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buf: 0,
            bits: 0,
        }
    }

    fn fill(&mut self, needed: u32) -> Result<()> {
        while self.bits < needed {
            if self.pos >= self.data.len() {
                bail!("unexpected end of deflate stream");
            }
            self.buf |= u64::from(self.data[self.pos]) << self.bits;
            self.pos += 1;
            self.bits += 8;
        }
        Ok(())
    }

    fn read_bits(&mut self, count: u32) -> Result<u16> {
        if count == 0 {
            return Ok(0);
        }
        self.fill(count)?;
        let val = (self.buf & ((1u64 << count) - 1)) as u16;
        self.buf >>= count;
        self.bits -= count;
        Ok(val)
    }

    fn align_byte(&mut self) {
        let drop_bits = self.bits % 8;
        self.buf >>= drop_bits;
        self.bits -= drop_bits;
    }

    fn remaining_aligned_bytes(&self) -> usize {
        debug_assert_eq!(self.bits % 8, 0);
        (self.data.len() - self.pos) + (self.bits as usize / 8)
    }

    fn read_u16_le(&mut self) -> Result<u16> {
        self.align_byte();
        let lo = self.read_bits(8)?;
        let hi = self.read_bits(8)?;
        Ok(lo | (hi << 8))
    }

    fn read_exact_aligned(&mut self, len: usize, out: &mut Vec<u8>) -> Result<()> {
        self.align_byte();
        let mut remaining = len;
        while remaining > 0 && self.bits >= 8 {
            out.push((self.buf & 0xFF) as u8);
            self.buf >>= 8;
            self.bits -= 8;
            remaining -= 1;
        }
        if self.pos + remaining > self.data.len() {
            bail!("truncated deflate stored block");
        }
        out.extend_from_slice(&self.data[self.pos..self.pos + remaining]);
        self.pos += remaining;
        Ok(())
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Result<Self> {
        let mut counts = [0u16; 16];
        for &len in lengths {
            if len as usize >= counts.len() {
                bail!("invalid Huffman code length");
            }
            if len > 0 {
                counts[len as usize] += 1;
            }
        }
        let mut left = 1i32;
        for &count in &counts[1..] {
            left = (left << 1) - i32::from(count);
            if left < 0 {
                bail!("over-subscribed Huffman tree");
            }
        }
        let mut offsets = [0usize; 16];
        for bits in 1..15 {
            offsets[bits + 1] = offsets[bits] + usize::from(counts[bits]);
        }
        let total: usize = counts[1..].iter().map(|&c| usize::from(c)).sum();
        let mut symbols = vec![0u16; total];
        for (sym, &len) in lengths.iter().enumerate() {
            if len > 0 {
                let idx = offsets[len as usize];
                symbols[idx] = sym as u16;
                offsets[len as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0usize;
        for len in 1..=15 {
            code |= i32::from(reader.read_bits(1)?);
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return Ok(self.symbols[index + (code - first) as usize]);
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        bail!("invalid Huffman symbol")
    }
}

/// Bounded RFC 1950 (zlib) + RFC 1951 (DEFLATE) decompressor for inspecting
/// compressed PNG metadata (`zTXt`, compressed `iTXt`).
fn inflate_zlib_bounded(data: &[u8], max_bytes: usize) -> Result<Vec<u8>> {
    if data.len() < 6 {
        bail!("truncated zlib stream");
    }
    let cmf = data[0];
    let flg = data[1];
    if (cmf & 0x0F) != 8 || (cmf >> 4) > 7 {
        bail!("unsupported zlib compression method or window size");
    }
    if !((u16::from(cmf) << 8) | u16::from(flg)).is_multiple_of(31) {
        bail!("invalid zlib header checksum");
    }
    if (flg & 0x20) != 0 {
        bail!("preset zlib dictionaries are not supported");
    }
    let deflate_slice = &data[2..data.len() - 4];
    let expected_adler = u32::from_be_bytes([
        data[data.len() - 4],
        data[data.len() - 3],
        data[data.len() - 2],
        data[data.len() - 1],
    ]);

    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u32; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u32; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    const CL_ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    let mut reader = BitReader::new(deflate_slice);
    let mut out = Vec::new();
    loop {
        let bfinal = reader.read_bits(1)? != 0;
        let btype = reader.read_bits(2)?;
        match btype {
            0 => {
                let len = reader.read_u16_le()?;
                let nlen = reader.read_u16_le()?;
                if len != !nlen {
                    bail!("corrupt deflate stored block length");
                }
                let len = usize::from(len);
                if out.len().saturating_add(len) > max_bytes {
                    bail!("compressed metadata exceeds {max_bytes}-byte decompression cap");
                }
                reader.read_exact_aligned(len, &mut out)?;
            }
            1 | 2 => {
                let (litlen_tree, dist_tree) = if btype == 1 {
                    let mut lit = [0u8; 288];
                    lit[..144].fill(8);
                    lit[144..256].fill(9);
                    lit[256..280].fill(7);
                    lit[280..288].fill(8);
                    let dist = [5u8; 32];
                    (Huffman::from_lengths(&lit)?, Huffman::from_lengths(&dist)?)
                } else {
                    let hlit = usize::from(reader.read_bits(5)?) + 257;
                    let hdist = usize::from(reader.read_bits(5)?) + 1;
                    let hclen = usize::from(reader.read_bits(4)?) + 4;
                    if hlit > 286 || hdist > 30 {
                        bail!("invalid dynamic Huffman table sizes");
                    }
                    let mut cl_lengths = [0u8; 19];
                    for &slot in &CL_ORDER[..hclen] {
                        cl_lengths[slot] = reader.read_bits(3)? as u8;
                    }
                    let cl_tree = Huffman::from_lengths(&cl_lengths)?;
                    let total = hlit + hdist;
                    let mut lengths = Vec::with_capacity(total);
                    while lengths.len() < total {
                        let sym = cl_tree.decode(&mut reader)?;
                        match sym {
                            0..=15 => lengths.push(sym as u8),
                            16 => {
                                let prev = *lengths.last().ok_or_else(|| {
                                    anyhow::anyhow!("repeat without prior length")
                                })?;
                                let repeat = usize::from(reader.read_bits(2)?) + 3;
                                if lengths.len() + repeat > total {
                                    bail!("Huffman length repeat overflow");
                                }
                                lengths.resize(lengths.len() + repeat, prev);
                            }
                            17 => {
                                let repeat = usize::from(reader.read_bits(3)?) + 3;
                                if lengths.len() + repeat > total {
                                    bail!("Huffman zero repeat overflow");
                                }
                                lengths.resize(lengths.len() + repeat, 0);
                            }
                            18 => {
                                let repeat = usize::from(reader.read_bits(7)?) + 11;
                                if lengths.len() + repeat > total {
                                    bail!("Huffman zero repeat overflow");
                                }
                                lengths.resize(lengths.len() + repeat, 0);
                            }
                            _ => bail!("invalid code-length Huffman symbol"),
                        }
                    }
                    (
                        Huffman::from_lengths(&lengths[..hlit])?,
                        Huffman::from_lengths(&lengths[hlit..])?,
                    )
                };

                loop {
                    let sym = litlen_tree.decode(&mut reader)?;
                    if sym < 256 {
                        if out.len() >= max_bytes {
                            bail!("compressed metadata exceeds {max_bytes}-byte decompression cap");
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let idx = usize::from(sym - 257);
                        if idx >= LEN_BASE.len() {
                            bail!("invalid deflate length symbol");
                        }
                        let length = usize::from(LEN_BASE[idx])
                            + usize::from(reader.read_bits(LEN_EXTRA[idx])?);
                        let dsym = usize::from(dist_tree.decode(&mut reader)?);
                        if dsym >= DIST_BASE.len() {
                            bail!("invalid deflate distance symbol");
                        }
                        let dist = usize::from(DIST_BASE[dsym])
                            + usize::from(reader.read_bits(DIST_EXTRA[dsym])?);
                        if dist == 0 || dist > out.len() {
                            bail!("deflate back-reference out of bounds");
                        }
                        if out.len().saturating_add(length) > max_bytes {
                            bail!("compressed metadata exceeds {max_bytes}-byte decompression cap");
                        }
                        for _ in 0..length {
                            let b = out[out.len() - dist];
                            out.push(b);
                        }
                    }
                }
            }
            _ => bail!("invalid deflate block type"),
        }
        if bfinal {
            break;
        }
    }
    reader.align_byte();
    if reader.remaining_aligned_bytes() != 0 {
        bail!("unexpected trailing bytes before zlib Adler-32 checksum");
    }
    if adler32(&out) != expected_adler {
        bail!("zlib Adler-32 checksum mismatch");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Standard RFC 4648 base64 codec (no external dependency)
// ---------------------------------------------------------------------------

const B64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let b2 = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_TABLE[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_TABLE[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn b64_val(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>> {
    let bytes = input.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        bail!("base64 length must be a multiple of 4");
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let chunks = bytes.chunks_exact(4);
    let total_chunks = chunks.len();
    for (idx, chunk) in chunks.enumerate() {
        let last = idx + 1 == total_chunks;
        let v0 = b64_val(chunk[0]).ok_or_else(|| anyhow::anyhow!("invalid base64 character"))?;
        let v1 = b64_val(chunk[1]).ok_or_else(|| anyhow::anyhow!("invalid base64 character"))?;
        let pad2 = chunk[2] == b'=';
        let pad3 = chunk[3] == b'=';
        if (pad2 || pad3) && !last {
            bail!("unexpected base64 padding before final quantum");
        }
        if pad2 && !pad3 {
            bail!("invalid base64 padding");
        }
        if pad2 {
            if (v1 & 0x0F) != 0 {
                bail!("non-zero trailing bits in base64 padding");
            }
            out.push((v0 << 2) | (v1 >> 4));
        } else if pad3 {
            let v2 =
                b64_val(chunk[2]).ok_or_else(|| anyhow::anyhow!("invalid base64 character"))?;
            if (v2 & 0x03) != 0 {
                bail!("non-zero trailing bits in base64 padding");
            }
            out.push((v0 << 2) | (v1 >> 4));
            out.push((v1 << 4) | (v2 >> 2));
        } else {
            let v2 =
                b64_val(chunk[2]).ok_or_else(|| anyhow::anyhow!("invalid base64 character"))?;
            let v3 =
                b64_val(chunk[3]).ok_or_else(|| anyhow::anyhow!("invalid base64 character"))?;
            out.push((v0 << 2) | (v1 >> 4));
            out.push((v1 << 4) | (v2 >> 2));
            out.push((v2 << 6) | v3);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoder-valid 1x1 PNG (Pillow-verified).
    const VALID_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 224, 18, 145, 3, 0,
        0, 104, 0, 61, 84, 8, 163, 247, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    /// Decoder-valid 1x1 multi-scan progressive JPEG (Pillow-verified).
    const VALID_PROGRESSIVE_JPEG: &[u8] = &[
        255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 255, 219, 0, 67,
        0, 8, 6, 6, 7, 6, 5, 8, 7, 7, 7, 9, 9, 8, 10, 12, 20, 13, 12, 11, 11, 12, 25, 18, 19, 15,
        20, 29, 26, 31, 30, 29, 26, 28, 28, 32, 36, 46, 39, 32, 34, 44, 35, 28, 28, 40, 55, 41, 44,
        48, 49, 52, 52, 52, 31, 39, 57, 61, 56, 50, 60, 46, 51, 52, 50, 255, 219, 0, 67, 1, 9, 9,
        9, 12, 11, 12, 24, 13, 13, 24, 50, 33, 28, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
        50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
        50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 255, 194, 0, 17, 8, 0, 1,
        0, 1, 3, 1, 34, 0, 2, 17, 1, 3, 17, 1, 255, 196, 0, 21, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 6, 255, 196, 0, 20, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
        255, 218, 0, 12, 3, 1, 0, 2, 16, 3, 16, 0, 0, 1, 136, 12, 127, 255, 196, 0, 20, 16, 1, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 1, 0, 1, 5, 2, 127, 255,
        196, 0, 20, 17, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 3, 1,
        1, 63, 1, 127, 255, 196, 0, 20, 17, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255,
        218, 0, 8, 1, 2, 1, 1, 63, 1, 127, 255, 196, 0, 20, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 1, 0, 6, 63, 2, 127, 255, 196, 0, 20, 16, 1, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 1, 0, 1, 63, 33, 127, 255, 218, 0,
        12, 3, 1, 0, 2, 0, 3, 0, 0, 0, 16, 7, 255, 196, 0, 20, 17, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 3, 1, 1, 63, 16, 127, 255, 196, 0, 20, 17, 1, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 2, 1, 1, 63, 16, 127, 255, 196,
        0, 20, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 1, 0, 1,
        63, 16, 127, 255, 217,
    ];

    /// Decoder-valid 1x1 lossless WebP (Pillow-verified).
    const VALID_WEBP: &[u8] = &[
        82, 73, 70, 70, 30, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 17, 0, 0, 0, 47, 0, 0, 0, 0,
        7, 80, 138, 42, 212, 163, 255, 129, 136, 232, 127, 0, 0,
    ];

    /// Decoder-valid 1x1 lossy WebP (Pillow-verified).
    const VALID_WEBP_LOSSY: &[u8] = &[
        82, 73, 70, 70, 46, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 32, 34, 0, 0, 0, 112, 1, 0, 157,
        1, 42, 1, 0, 1, 0, 1, 64, 38, 37, 148, 2, 116, 1, 64, 0, 0, 254, 252, 55, 129, 87, 247,
        212, 232, 62, 43, 224, 0, 0,
    ];

    /// Decoder-valid 2x2 lossy RGBA WebP with VP8X + ALPH + VP8 (Pillow-verified).
    const VALID_WEBP_LOSSY_RGBA: &[u8] = &[
        82, 73, 70, 70, 78, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 88, 10, 0, 0, 0, 16, 0, 0, 0, 1,
        0, 0, 1, 0, 0, 65, 76, 80, 72, 5, 0, 0, 0, 0, 128, 128, 128, 128, 0, 86, 80, 56, 32, 34, 0,
        0, 0, 112, 1, 0, 157, 1, 42, 2, 0, 2, 0, 1, 64, 38, 37, 148, 2, 116, 1, 64, 0, 0, 254, 252,
        55, 129, 87, 247, 212, 232, 62, 43, 224, 0, 0,
    ];

    /// Decoder-valid 2-frame animated WebP with VP8X + ANIM + ANMF (Pillow-verified).
    const VALID_WEBP_ANIM: &[u8] = &[
        82, 73, 70, 70, 168, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 88, 10, 0, 0, 0, 2, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 65, 78, 73, 77, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 65, 78, 77, 70, 58, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0, 2, 86, 80, 56, 32, 34, 0, 0, 0, 48, 1, 0,
        157, 1, 42, 1, 0, 1, 0, 1, 64, 38, 37, 148, 0, 3, 112, 0, 254, 252, 55, 139, 219, 101, 162,
        80, 62, 228, 253, 17, 128, 0, 0, 65, 78, 77, 70, 58, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 100, 0, 0, 0, 86, 80, 56, 32, 34, 0, 0, 0, 52, 1, 0, 157, 1, 42, 1, 0, 1, 0, 0, 0,
        38, 37, 148, 0, 3, 112, 0, 254, 249, 79, 63, 182, 203, 68, 160, 125, 201, 250, 35, 0, 0, 0,
    ];

    /// Decoder-valid 16x16 gradient lossless WebP using SubtractGreen + Predictor transforms (Pillow-verified).
    const VALID_WEBP_LOSSLESS_PREDICTOR: &[u8] = &[
        82, 73, 70, 70, 40, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 27, 0, 0, 0, 47, 15, 192, 3,
        0, 205, 101, 68, 255, 3, 68, 218, 54, 179, 48, 5, 243, 175, 244, 240, 52, 34, 38, 0, 215,
        238, 2, 0,
    ];

    /// Decoder-valid 8x8 2-color lossless WebP using 1-bit ColorIndexing packing (Pillow-verified).
    const VALID_WEBP_LOSSLESS_PALETTE: &[u8] = &[
        82, 73, 70, 70, 34, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 21, 0, 0, 0, 47, 7, 192, 1, 0,
        15, 48, 53, 243, 48, 97, 243, 31, 120, 120, 149, 106, 68, 255, 163, 10, 0,
    ];

    /// Decoder-valid 1x1 GIF87a (Pillow-verified).
    const VALID_GIF: &[u8] = &[
        71, 73, 70, 56, 55, 97, 1, 0, 1, 0, 129, 0, 0, 10, 20, 30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 44,
        0, 0, 0, 0, 1, 0, 1, 0, 0, 8, 4, 0, 1, 4, 4, 0, 59,
    ];

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(12 + data.len());
        chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
        chunk.extend_from_slice(kind);
        chunk.extend_from_slice(data);
        let crc = crc32(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());
        chunk
    }

    fn zlib_stored(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01]; // zlib header, deflate 32K
        let len = payload.len() as u16;
        out.push(0x01); // BFINAL=1, BTYPE=00 (stored)
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&adler32(payload).to_be_bytes());
        out
    }

    fn png_with_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        // Insert chunk right before the 12-byte IEND chunk.
        let iend_start = VALID_PNG.len() - 12;
        let mut out = VALID_PNG[..iend_start].to_vec();
        out.extend_from_slice(&png_chunk(kind, data));
        out.extend_from_slice(&VALID_PNG[iend_start..]);
        out
    }

    fn jpeg_with_trailing_com(comment: &[u8]) -> Vec<u8> {
        // Insert COM segment after the final SOS scan, right before EOI (0xFF 0xD9).
        let eoi = VALID_PROGRESSIVE_JPEG.len() - 2;
        let mut out = VALID_PROGRESSIVE_JPEG[..eoi].to_vec();
        out.push(0xFF);
        out.push(0xFE);
        out.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(comment);
        out.extend_from_slice(&VALID_PROGRESSIVE_JPEG[eoi..]);
        out
    }

    fn utf16_encoded(text: &str, big_endian: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            out.extend_from_slice(&if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        out
    }

    fn webp_with_chunk(fourcc: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut chunks = Vec::new();
        // VP8X (10 bytes): EXIF flag 0x08, 1x1 canvas.
        chunks.extend_from_slice(b"VP8X");
        chunks.extend_from_slice(&10u32.to_le_bytes());
        chunks.extend_from_slice(&[0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // Copy the valid VP8L chunk from VALID_WEBP (bytes 12..).
        chunks.extend_from_slice(&VALID_WEBP[12..]);
        chunks.extend_from_slice(fourcc);
        chunks.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        chunks.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            chunks.push(0);
        }
        let mut out = Vec::with_capacity(12 + chunks.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((4 + chunks.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&chunks);
        out
    }

    fn jpeg_with_app1_exif(exif_payload: &[u8]) -> Vec<u8> {
        let mut out = VALID_PROGRESSIVE_JPEG[..2].to_vec();
        out.push(0xFF);
        out.push(0xE1);
        out.extend_from_slice(&((exif_payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(exif_payload);
        out.extend_from_slice(&VALID_PROGRESSIVE_JPEG[2..]);
        out
    }

    #[test]
    fn validates_genuine_containers_and_rejects_magic_only_svg_and_truncated() {
        assert_eq!(validate_image_bytes(VALID_PNG).unwrap(), ImageFormat::Png);
        assert_eq!(
            validate_image_bytes(VALID_PROGRESSIVE_JPEG).unwrap(),
            ImageFormat::Jpeg
        );
        assert_eq!(validate_image_bytes(VALID_WEBP).unwrap(), ImageFormat::Webp);
        assert_eq!(
            validate_image_bytes(VALID_WEBP_LOSSY).unwrap(),
            ImageFormat::Webp
        );
        assert_eq!(
            validate_image_bytes(VALID_WEBP_LOSSY_RGBA).unwrap(),
            ImageFormat::Webp
        );
        assert_eq!(
            validate_image_bytes(VALID_WEBP_ANIM).unwrap(),
            ImageFormat::Webp
        );
        assert_eq!(
            validate_image_bytes(VALID_WEBP_LOSSLESS_PREDICTOR).unwrap(),
            ImageFormat::Webp
        );
        assert_eq!(
            validate_image_bytes(VALID_WEBP_LOSSLESS_PALETTE).unwrap(),
            ImageFormat::Webp
        );
        assert_eq!(validate_image_bytes(VALID_GIF).unwrap(), ImageFormat::Gif);

        // Header-only VP8L (5-byte chunk payload `2f 00 00 00 00` + RIFF pad byte).
        let header_only_vp8l =
            b"RIFF\x12\x00\x00\x00WEBPVP8L\x05\x00\x00\x00\x2f\x00\x00\x00\x00\x00";
        // Zero-padded 8-byte VP8L with empty normal prefix code table.
        let zero_vp8l =
            b"RIFF\x14\x00\x00\x00WEBPVP8L\x08\x00\x00\x00\x2f\x00\x00\x00\x00\x00\x00\x00";
        // 30-byte VP8L with predictor transform flag followed by zero padding.
        let predictor_zero_vp8l =
            b"RIFF\x16\x00\x00\x00WEBPVP8L\x0a\x00\x00\x00\x2f\x00\x00\x00\x00\x01\x00\x00\x00\x00";
        // VP8L with Color transform flag (bit0=1, type=1 -> 0b011 = 0x03) and zero padding.
        let color_zero_vp8l =
            b"RIFF\x16\x00\x00\x00WEBPVP8L\x0a\x00\x00\x00\x2f\x00\x00\x00\x00\x03\x00\x00\x00\x00";
        // VP8L with ColorIndexing transform flag (bit0=1, type=3 -> 0b111 = 0x07) and zero padding.
        let palette_zero_vp8l =
            b"RIFF\x16\x00\x00\x00WEBPVP8L\x0a\x00\x00\x00\x2f\x00\x00\x00\x00\x07\x00\x00\x00\x00";
        // Truncated genuine lossless predictor WebP (missing final bytes of main image).
        let mut trunc_predictor_vp8l =
            VALID_WEBP_LOSSLESS_PREDICTOR[..VALID_WEBP_LOSSLESS_PREDICTOR.len() - 6].to_vec();
        let trunc_riff = (trunc_predictor_vp8l.len() - 8) as u32;
        trunc_predictor_vp8l[4..8].copy_from_slice(&trunc_riff.to_le_bytes());
        let trunc_vp8l_sz = (trunc_predictor_vp8l.len() - 20) as u32;
        trunc_predictor_vp8l[16..20].copy_from_slice(&trunc_vp8l_sz.to_le_bytes());
        // Animated WebP whose ANMF chunk carries only the 16-byte frame header and no image data.
        let mut empty_anmf = Vec::new();
        empty_anmf.extend_from_slice(b"RIFF");
        empty_anmf.extend_from_slice(&(4u32 + 18 + 14 + 24).to_le_bytes());
        empty_anmf.extend_from_slice(b"WEBP");
        empty_anmf.extend_from_slice(b"VP8X");
        empty_anmf.extend_from_slice(&10u32.to_le_bytes());
        empty_anmf.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        empty_anmf.extend_from_slice(b"ANIM");
        empty_anmf.extend_from_slice(&6u32.to_le_bytes());
        empty_anmf.extend_from_slice(&[0u8; 6]);
        empty_anmf.extend_from_slice(b"ANMF");
        empty_anmf.extend_from_slice(&16u32.to_le_bytes());
        empty_anmf.extend_from_slice(&[0u8; 16]);

        for bad in [
            // Magic-only and truncated payloads for each format.
            b"\x89PNG\r\n\x1a\n".as_slice(),
            &VALID_PNG[..VALID_PNG.len() - 4],
            &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02, 0xFF, 0xD9],
            &VALID_PROGRESSIVE_JPEG[..VALID_PROGRESSIVE_JPEG.len() - 2],
            b"RIFF\x10\x00\x00\x00WEBPVP8 ".as_slice(),
            header_only_vp8l.as_slice(),
            zero_vp8l.as_slice(),
            predictor_zero_vp8l.as_slice(),
            color_zero_vp8l.as_slice(),
            palette_zero_vp8l.as_slice(),
            trunc_predictor_vp8l.as_slice(),
            empty_anmf.as_slice(),
            b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".as_slice(),
            // Active / non-image payloads.
            b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>".as_slice(),
            b"<?xml version='1.0'?><svg></svg>".as_slice(),
            b"<!doctype html><html><body>hi</body></html>".as_slice(),
            b"plain text".as_slice(),
            b"".as_slice(),
        ] {
            assert!(validate_image_bytes(bad).is_err(), "accepted bad payload");
        }
    }

    #[test]
    fn base64_round_trips_and_rejects_malformed() {
        for input in [b"".as_slice(), b"a", b"ab", b"abc", b"hello world\x00\xff"] {
            let enc = encode_base64(input);
            assert_eq!(decode_base64(&enc).unwrap(), input);
        }
        for bad in ["abc", "ab=c", "!!!!", "AA=A", "AB=="] {
            assert!(decode_base64(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rel_path_parser_rejects_traversal_and_bad_digests() {
        let hash = "a".repeat(64);
        assert!(AttachmentId::parse_rel_path(&format!("attachments/{hash}.png")).is_some());
        for bad in [
            format!("attachments/../{hash}.png"),
            format!("../attachments/{hash}.png"),
            format!("attachments/{}.png", "A".repeat(64)),
            format!("attachments/{hash}.svg"),
            format!("attachments/{hash}.png?x=1"),
            "attachments/short.png".to_string(),
        ] {
            assert!(AttachmentId::parse_rel_path(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn png_ztxt_and_compressed_itxt_metadata_are_scanned_and_stripped() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());

        // zTXt compressed comment.
        let mut ztxt = b"Comment\0\x00".to_vec();
        ztxt.extend_from_slice(&zlib_stored(b"captured at /home/alice/secret.png"));
        let png_ztxt = png_with_chunk(b"zTXt", &ztxt);
        assert_eq!(validate_image_bytes(&png_ztxt).unwrap(), ImageFormat::Png);
        let scan = scan_metadata(ImageFormat::Png, &png_ztxt, &scanner);
        assert!(scan.unreadable.is_empty(), "{scan:?}");
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].text, "/home/alice/secret.png");

        let stripped = strip_metadata(ImageFormat::Png, &png_ztxt).unwrap();
        let after = scan_metadata(ImageFormat::Png, &stripped, &scanner);
        assert!(after.findings.is_empty() && after.unreadable.is_empty());

        // Compressed iTXt comment.
        let mut itxt = b"Comment\0\x01\x00en\0\0".to_vec();
        itxt.extend_from_slice(&zlib_stored(b"host 192.168.1.99"));
        let png_itxt = png_with_chunk(b"iTXt", &itxt);
        assert_eq!(validate_image_bytes(&png_itxt).unwrap(), ImageFormat::Png);
        let scan = scan_metadata(ImageFormat::Png, &png_itxt, &scanner);
        assert!(scan.unreadable.is_empty(), "{scan:?}");
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].text, "192.168.1.99");

        // Corrupt zTXt stream is reported as unreadable so default render refuses.
        let corrupt_ztxt = png_with_chunk(b"zTXt", b"Comment\0\x00not-zlib");
        let scan = scan_metadata(ImageFormat::Png, &corrupt_ztxt, &scanner);
        assert!(!scan.unreadable.is_empty(), "{scan:?}");

        // Extra bytes inserted between the final DEFLATE block and the Adler-32 checksum must fail.
        let good_zlib = zlib_stored(b"harmless");
        let mut padded_zlib = good_zlib[..good_zlib.len() - 4].to_vec();
        padded_zlib.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        padded_zlib.extend_from_slice(&good_zlib[good_zlib.len() - 4..]);
        let mut extra_ztxt = b"Comment\0\x00".to_vec();
        extra_ztxt.extend_from_slice(&padded_zlib);
        let extra_ztxt_png = png_with_chunk(b"zTXt", &extra_ztxt);
        let scan = scan_metadata(ImageFormat::Png, &extra_ztxt_png, &scanner);
        assert!(!scan.unreadable.is_empty(), "{scan:?}");

        // Invalid UTF-8 in uncompressed and compressed iTXt must be reported as unreadable.
        let bad_utf8_itxt = png_with_chunk(b"iTXt", b"Comment\0\x00\x00\0\0\xffhello");
        let scan = scan_metadata(ImageFormat::Png, &bad_utf8_itxt, &scanner);
        assert!(!scan.unreadable.is_empty(), "{scan:?}");

        let mut bad_comp_itxt = b"Comment\0\x01\x00\0\0".to_vec();
        bad_comp_itxt.extend_from_slice(&zlib_stored(b"\xffhello"));
        let bad_comp_itxt_png = png_with_chunk(b"iTXt", &bad_comp_itxt);
        let scan = scan_metadata(ImageFormat::Png, &bad_comp_itxt_png, &scanner);
        assert!(!scan.unreadable.is_empty(), "{scan:?}");
    }

    #[test]
    fn utf16_exif_comments_are_scanned_and_stripped_across_jpeg_png_and_webp() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());
        for (big_endian, odd_pad) in [(true, false), (false, false), (true, true), (false, true)] {
            let mut exif = b"Exif\0\0MM\x00*\x00\x00\x00\x08UNICODE\0".to_vec();
            if odd_pad {
                exif.push(0x01);
            }
            let encoded_path = utf16_encoded("/home/alice/private.png", big_endian);
            exif.extend_from_slice(&encoded_path);

            let cases = [
                (ImageFormat::Jpeg, jpeg_with_app1_exif(&exif)),
                (ImageFormat::Png, png_with_chunk(b"eXIf", &exif)),
                (ImageFormat::Webp, webp_with_chunk(b"EXIF", &exif)),
            ];
            for (fmt, bytes) in cases {
                assert_eq!(validate_image_bytes(&bytes).unwrap(), fmt);
                let scan = scan_metadata(fmt, &bytes, &scanner);
                assert!(
                    scan.findings
                        .iter()
                        .any(|f| f.text == "/home/alice/private.png"),
                    "{fmt:?} be={big_endian} odd={odd_pad}: {scan:?}"
                );
                let stripped = strip_metadata(fmt, &bytes).unwrap();
                assert!(
                    !stripped
                        .windows(encoded_path.len())
                        .any(|w| w == encoded_path.as_slice()),
                    "{fmt:?} stripped output retained UTF-16 path bytes"
                );
                assert!(scan_metadata(fmt, &stripped, &scanner).findings.is_empty());
            }
        }
    }

    #[test]
    fn jpeg_post_scan_com_is_scanned_and_stripped_across_progressive_scans() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());
        let jpeg = jpeg_with_trailing_com(b"from /home/alice/private.png");
        assert_eq!(validate_image_bytes(&jpeg).unwrap(), ImageFormat::Jpeg);
        let scan = scan_metadata(ImageFormat::Jpeg, &jpeg, &scanner);
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].text, "/home/alice/private.png");

        let stripped = strip_metadata(ImageFormat::Jpeg, &jpeg).unwrap();
        assert_eq!(validate_image_bytes(&stripped).unwrap(), ImageFormat::Jpeg);
        assert!(
            !stripped
                .windows(b"/home/alice/private.png".len())
                .any(|w| w == b"/home/alice/private.png"),
            "private comment survived JPEG metadata strip"
        );
        assert!(scan_metadata(ImageFormat::Jpeg, &stripped, &scanner)
            .findings
            .is_empty());
    }

    #[test]
    fn safe_alt_never_leaks_parent_dirs_or_private_tokens() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());
        let alt = safe_alt_from_path(
            Path::new("/home/alice/private-dir/before-fix.png"),
            &scanner,
        );
        assert_eq!(alt, "before-fix");
        assert!(!alt.contains("/home/alice"));
        let private_name = safe_alt_from_path(Path::new("/home/alice/192.168.1.20.png"), &scanner);
        assert!(!private_name.contains("192.168.1.20"), "{private_name}");
    }
}
