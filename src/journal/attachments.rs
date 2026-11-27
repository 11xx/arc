//! Content-addressed raster image attachments stored beside journal artifacts.
//!
//! Blobs live under `<hot>/attachments/<sha256>.<ext>` and
//! `<cold>/attachments/<sha256>.<ext>` and are referenced in Markdown by the
//! relative path `attachments/<sha256>.<ext>`. Only PNG, JPEG, WebP, and GIF
//! payloads verified by magic bytes up to [`MAX_ATTACHMENT_BYTES`] are
//! accepted; SVG, HTML, and other active formats are refused. Directory and
//! blob opens inside the journal never follow symbolic links and verify both
//! format and SHA-256 digest on every read and import.

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

    /// Sniff an allowed raster image format strictly from magic bytes.
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

    /// Validate metadata, base64 encoding, size bounds, magic bytes, and SHA-256.
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
        if ImageFormat::detect(&decoded) != Some(id.format) {
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
/// larger than [`MAX_ATTACHMENT_BYTES`], and non-raster/active formats (such as
/// SVG or HTML) are refused.
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
    ImageFormat::detect(bytes).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported attachment format: magic bytes do not match PNG, JPEG, WebP, or GIF"
        )
    })
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
    if ImageFormat::detect(&bytes) != Some(id.format) {
        return ReadOutcome::Corrupt(format!(
            "magic bytes do not match {}",
            id.format.media_type()
        ));
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
/// `store_dir`, excluding `skip_files`.
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
        if parse_artifact_name(&name).is_none() || skip_files.contains(name.as_str()) {
            continue;
        }
        if let Ok(body) = std::fs::read_to_string(entry.path()) {
            used.extend(referenced_for_artifact(&body, events, &name));
        }
    }
    Ok(used)
}

/// Copy every attachment referenced by `filename` (body + events) from
/// `source_dir` into `dest_dir`, verifying hash and format.
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
/// leaves `store_dir`. Blobs still referenced by any remaining artifact in
/// `store_dir` are preserved.
pub(crate) fn prune_unreferenced_from_store(
    store_dir: &Path,
    events: &[JournalEvent],
    candidates: &[AttachmentId],
    leaving_files: &BTreeSet<&str>,
) -> Result<()> {
    if candidates.is_empty() {
        return Ok(());
    }
    let still_used = attachments_used_in_store(store_dir, events, leaving_files)?;
    let Ok(root_fd) = open_dir_nofollow(store_dir) else {
        return Ok(());
    };
    let Ok(att_fd) = open_subdir_nofollow(&root_fd, ATTACHMENTS_DIR) else {
        return Ok(());
    };
    let mut removed_any = false;
    for id in candidates {
        if still_used.contains(id) {
            continue;
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
            ReadOutcome::Available { bytes, .. } => {
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

/// Scan textual metadata chunks embedded inside a validated raster image for
/// private paths, addresses, or hostnames using `scanner`.
pub(super) fn scan_metadata(format: ImageFormat, bytes: &[u8], scanner: &Scanner) -> Vec<Finding> {
    let mut findings = Vec::new();
    for text in extract_metadata_strings(format, bytes) {
        findings.extend(scanner.findings(&text));
    }
    findings
}

/// Remove textual/EXIF/XMP/comment metadata chunks from a validated raster
/// image so rendered `data:` URIs never carry private metadata under
/// `--strip-private`.
pub(crate) fn strip_metadata(format: ImageFormat, bytes: &[u8]) -> Result<Vec<u8>> {
    match format {
        ImageFormat::Png => strip_png_metadata(bytes),
        ImageFormat::Jpeg => strip_jpeg_metadata(bytes),
        ImageFormat::Webp => strip_webp_metadata(bytes),
        ImageFormat::Gif => strip_gif_metadata(bytes),
    }
}

fn extract_metadata_strings(format: ImageFormat, bytes: &[u8]) -> Vec<String> {
    match format {
        ImageFormat::Png => extract_png_metadata(bytes),
        ImageFormat::Jpeg => extract_jpeg_metadata(bytes),
        ImageFormat::Webp => extract_webp_metadata(bytes),
        ImageFormat::Gif => extract_gif_metadata(bytes),
    }
}

fn printable_runs(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for &b in data {
        if matches!(b, 0x20..=0x7E | b'\t' | b'\n' | b'\r') {
            cur.push(b);
        } else {
            if cur.len() >= 4 {
                if let Ok(s) = String::from_utf8(std::mem::take(&mut cur)) {
                    out.push(s);
                }
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 4 {
        if let Ok(s) = String::from_utf8(cur) {
            out.push(s);
        }
    }
    out
}

fn extract_png_metadata(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
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
        if matches!(kind, b"tEXt" | b"iTXt" | b"eXIf") {
            out.extend(printable_runs(data));
        }
        pos = chunk_end;
    }
    out
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

fn extract_jpeg_metadata(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 2;
    while pos + 2 <= bytes.len() {
        if bytes[pos] != 0xFF {
            break;
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if matches!(marker, 0xD0..=0xD8 | 0x01) {
            continue;
        }
        if pos + 2 > bytes.len() {
            break;
        }
        let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        if len < 2 || pos + len > bytes.len() {
            break;
        }
        let payload = &bytes[pos + 2..pos + len];
        if marker == 0xFE || (0xE1..=0xEF).contains(&marker) {
            out.extend(printable_runs(payload));
        }
        pos += len;
    }
    out
}

fn strip_jpeg_metadata(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        bail!("truncated JPEG header");
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..2]);
    let mut pos = 2;
    while pos < bytes.len() {
        if bytes[pos] != 0xFF {
            bail!("malformed JPEG marker stream");
        }
        let seg_start = pos;
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;
        if marker == 0xDA {
            // SOS and entropy-coded scan through EOI stay untouched.
            out.extend_from_slice(&bytes[seg_start..]);
            return Ok(out);
        }
        if marker == 0xD9 {
            out.extend_from_slice(&bytes[seg_start..pos]);
            return Ok(out);
        }
        if matches!(marker, 0xD0..=0xD8 | 0x01) {
            out.extend_from_slice(&bytes[seg_start..pos]);
            continue;
        }
        if pos + 2 > bytes.len() {
            bail!("truncated JPEG segment length");
        }
        let len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        if len < 2 || pos + len > bytes.len() {
            bail!("invalid JPEG segment length");
        }
        let seg_end = pos + len;
        if marker != 0xFE && !(0xE1..=0xEF).contains(&marker) {
            out.extend_from_slice(&bytes[seg_start..seg_end]);
        }
        pos = seg_end;
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
            out.extend(printable_runs(&bytes[data_start..data_start + size]));
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
                    out.extend(printable_runs(&payload));
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

    fn sample_png() -> Vec<u8> {
        // Minimal valid 1x1 PNG (signature + IHDR + IDAT + IEND).
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        // IHDR
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        // IEND
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]);
        png
    }

    fn png_with_text(text: &str) -> Vec<u8> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        let payload = format!("Comment\0{text}");
        png.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        png.extend_from_slice(b"tEXt");
        png.extend_from_slice(payload.as_bytes());
        png.extend_from_slice(&[0, 0, 0, 0]);
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]);
        png
    }

    #[test]
    fn magic_bytes_accept_raster_and_reject_svg_html_and_text() {
        assert_eq!(
            validate_image_bytes(&sample_png()).unwrap(),
            ImageFormat::Png
        );
        assert_eq!(
            validate_image_bytes(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]).unwrap(),
            ImageFormat::Jpeg
        );
        assert_eq!(
            validate_image_bytes(b"RIFF\x10\x00\x00\x00WEBPVP8 ").unwrap(),
            ImageFormat::Webp
        );
        assert_eq!(
            validate_image_bytes(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap(),
            ImageFormat::Gif
        );
        for bad in [
            b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>".as_slice(),
            b"<?xml version='1.0'?><svg></svg>".as_slice(),
            b"<!doctype html><html><body>hi</body></html>".as_slice(),
            b"plain text".as_slice(),
            b"".as_slice(),
        ] {
            assert!(validate_image_bytes(bad).is_err());
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
    fn png_metadata_scan_and_strip() {
        let scanner = Scanner::new(Vec::<PathBuf>::new());
        let png = png_with_text("captured at /home/alice/secret.png on 192.168.1.9");
        let findings = scan_metadata(ImageFormat::Png, &png, &scanner);
        assert_eq!(findings.len(), 2, "{findings:?}");
        let stripped = strip_metadata(ImageFormat::Png, &png).unwrap();
        assert_eq!(ImageFormat::detect(&stripped), Some(ImageFormat::Png));
        assert!(scan_metadata(ImageFormat::Png, &stripped, &scanner).is_empty());
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
