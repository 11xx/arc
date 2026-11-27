use super::common::*;

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

fn zlib_stored(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let len = payload.len() as u16;
    out.push(0x01);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&adler32(payload).to_be_bytes());
    out
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(12 + data.len());
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    chunk.extend_from_slice(&crc32(&chunk[4..]).to_be_bytes());
    chunk
}

/// Build a genuine, decoder-valid 1x1 RGB PNG for the given pixel color.
fn sample_png(tag: u8) -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let ihdr = [0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0];
    png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    // Scanline: filter byte 0 + RGB pixel.
    let raw_scanline = [0u8, tag, tag.wrapping_mul(3), tag.wrapping_mul(7)];
    let idat = zlib_stored(&raw_scanline);
    png.extend_from_slice(&png_chunk(b"IDAT", &idat));
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    png
}

fn png_with_extra_chunk(tag: u8, kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let base = sample_png(tag);
    let iend_start = base.len() - 12;
    let mut out = base[..iend_start].to_vec();
    out.extend_from_slice(&png_chunk(kind, data));
    out.extend_from_slice(&base[iend_start..]);
    out
}

fn png_with_text_chunk(text: &str) -> Vec<u8> {
    let payload = format!("Comment\0{text}");
    png_with_extra_chunk(5, b"tEXt", payload.as_bytes())
}

fn png_with_ztxt_chunk(text: &str) -> Vec<u8> {
    let mut payload = b"Comment\0\x00".to_vec();
    payload.extend_from_slice(&zlib_stored(text.as_bytes()));
    png_with_extra_chunk(6, b"zTXt", &payload)
}

fn png_with_compressed_itxt_chunk(text: &str) -> Vec<u8> {
    let mut payload = b"Comment\0\x01\x00en\0\0".to_vec();
    payload.extend_from_slice(&zlib_stored(text.as_bytes()));
    png_with_extra_chunk(7, b"iTXt", &payload)
}

/// Genuine decoder-valid 1x1 baseline JPEG (Pillow-verified).
const VALID_JPEG: &[u8] = &[
    255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 255, 219, 0, 67, 0, 8,
    6, 6, 7, 6, 5, 8, 7, 7, 7, 9, 9, 8, 10, 12, 20, 13, 12, 11, 11, 12, 25, 18, 19, 15, 20, 29, 26,
    31, 30, 29, 26, 28, 28, 32, 36, 46, 39, 32, 34, 44, 35, 28, 28, 40, 55, 41, 44, 48, 49, 52, 52,
    52, 31, 39, 57, 61, 56, 50, 60, 46, 51, 52, 50, 255, 219, 0, 67, 1, 9, 9, 9, 12, 11, 12, 24,
    13, 13, 24, 50, 33, 28, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 255, 192, 0, 17, 8, 0, 1, 0, 1, 3, 1, 34, 0, 2, 17, 1, 3,
    17, 1, 255, 196, 0, 31, 0, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6,
    7, 8, 9, 10, 11, 255, 196, 0, 181, 16, 0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125, 1, 2,
    3, 0, 4, 17, 5, 18, 33, 49, 65, 6, 19, 81, 97, 7, 34, 113, 20, 50, 129, 145, 161, 8, 35, 66,
    177, 193, 21, 82, 209, 240, 36, 51, 98, 114, 130, 9, 10, 22, 23, 24, 25, 26, 37, 38, 39, 40,
    41, 42, 52, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89,
    90, 99, 100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 131, 132,
    133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165,
    166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198,
    199, 200, 201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 225, 226, 227, 228, 229, 230,
    231, 232, 233, 234, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250, 255, 196, 0, 31, 1, 0, 3,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 255, 196, 0,
    181, 17, 0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119, 0, 1, 2, 3, 17, 4, 5, 33, 49, 6, 18,
    65, 81, 7, 97, 113, 19, 34, 50, 129, 8, 20, 66, 145, 161, 177, 193, 9, 35, 51, 82, 240, 21, 98,
    114, 209, 10, 22, 36, 52, 225, 37, 241, 23, 24, 25, 26, 38, 39, 40, 41, 42, 53, 54, 55, 56, 57,
    58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88, 89, 90, 99, 100, 101, 102, 103,
    104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 130, 131, 132, 133, 134, 135, 136, 137,
    138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164, 165, 166, 167, 168, 169, 170,
    178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197, 198, 199, 200, 201, 202, 210,
    211, 212, 213, 214, 215, 216, 217, 218, 226, 227, 228, 229, 230, 231, 232, 233, 234, 242, 243,
    244, 245, 246, 247, 248, 249, 250, 255, 218, 0, 12, 3, 1, 0, 2, 17, 3, 17, 0, 63, 0, 241, 26,
    40, 162, 182, 50, 63, 255, 217,
];

/// Genuine decoder-valid 1x1 multi-scan progressive JPEG (Pillow-verified).
const VALID_PROGRESSIVE_JPEG: &[u8] = &[
    255, 216, 255, 224, 0, 16, 74, 70, 73, 70, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 255, 219, 0, 67, 0, 8,
    6, 6, 7, 6, 5, 8, 7, 7, 7, 9, 9, 8, 10, 12, 20, 13, 12, 11, 11, 12, 25, 18, 19, 15, 20, 29, 26,
    31, 30, 29, 26, 28, 28, 32, 36, 46, 39, 32, 34, 44, 35, 28, 28, 40, 55, 41, 44, 48, 49, 52, 52,
    52, 31, 39, 57, 61, 56, 50, 60, 46, 51, 52, 50, 255, 219, 0, 67, 1, 9, 9, 9, 12, 11, 12, 24,
    13, 13, 24, 50, 33, 28, 33, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50, 50,
    50, 50, 50, 50, 50, 50, 50, 50, 50, 255, 194, 0, 17, 8, 0, 1, 0, 1, 3, 1, 34, 0, 2, 17, 1, 3,
    17, 1, 255, 196, 0, 21, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 255, 196, 0,
    20, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 255, 218, 0, 12, 3, 1, 0, 2, 16, 3,
    16, 0, 0, 1, 136, 12, 127, 255, 196, 0, 20, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 255, 218, 0, 8, 1, 1, 0, 1, 5, 2, 127, 255, 196, 0, 20, 17, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 3, 1, 1, 63, 1, 127, 255, 196, 0, 20, 17, 1, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 2, 1, 1, 63, 1, 127, 255, 196, 0, 20,
    16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 1, 0, 6, 63, 2, 127,
    255, 196, 0, 20, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 1,
    0, 1, 63, 33, 127, 255, 218, 0, 12, 3, 1, 0, 2, 0, 3, 0, 0, 0, 16, 7, 255, 196, 0, 20, 17, 1,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 3, 1, 1, 63, 16, 127, 255,
    196, 0, 20, 17, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218, 0, 8, 1, 2, 1, 1,
    63, 16, 127, 255, 196, 0, 20, 16, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 218,
    0, 8, 1, 1, 0, 1, 63, 16, 127, 255, 217,
];

/// Genuine decoder-valid 1x1 lossless WebP (Pillow-verified).
const VALID_WEBP: &[u8] = &[
    82, 73, 70, 70, 30, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 76, 17, 0, 0, 0, 47, 0, 0, 0, 0, 7,
    80, 138, 42, 212, 163, 255, 129, 136, 232, 127, 0, 0,
];

/// Genuine decoder-valid 1x1 GIF87a (Pillow-verified).
const VALID_GIF: &[u8] = &[
    71, 73, 70, 56, 55, 97, 1, 0, 1, 0, 129, 0, 0, 10, 20, 30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 44, 0, 0,
    0, 0, 1, 0, 1, 0, 0, 8, 4, 0, 1, 4, 4, 0, 59,
];

fn sample_jpeg() -> Vec<u8> {
    VALID_JPEG.to_vec()
}

fn progressive_jpeg_with_post_scan_comment(comment: &[u8]) -> Vec<u8> {
    let eoi = VALID_PROGRESSIVE_JPEG.len() - 2;
    let mut out = VALID_PROGRESSIVE_JPEG[..eoi].to_vec();
    out.push(0xFF);
    out.push(0xFE);
    out.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(comment);
    out.extend_from_slice(&VALID_PROGRESSIVE_JPEG[eoi..]);
    out
}

fn sample_webp() -> Vec<u8> {
    VALID_WEBP.to_vec()
}

fn sample_gif() -> Vec<u8> {
    VALID_GIF.to_vec()
}

fn decode_b64_data_uri(html: &str, prefix: &str) -> Vec<u8> {
    let start = html
        .find(prefix)
        .unwrap_or_else(|| panic!("missing {prefix} in {html}"))
        + prefix.len();
    let end = html[start..].find('"').unwrap() + start;
    let b64 = &html[start..end];
    let val = |b: u8| -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("invalid b64 char {b}"),
        }
    };
    let mut out = Vec::new();
    for chunk in b64.as_bytes().chunks_exact(4) {
        let v0 = val(chunk[0]);
        let v1 = val(chunk[1]);
        out.push((v0 << 2) | (v1 >> 4));
        if chunk[2] != b'=' {
            let v2 = val(chunk[2]);
            out.push((v1 << 4) | (v2 >> 2));
            if chunk[3] != b'=' {
                let v3 = val(chunk[3]);
                out.push((v2 << 6) | v3);
            }
        }
    }
    out
}

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

fn cold_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir", "--archive"])).trim())
}

#[test]
fn attach_at_filing_and_append_deduplicates_and_hides_source_paths() {
    let repo = Repo::new();
    let secret_dir = repo.home.join("private-host-folder");
    fs::create_dir_all(&secret_dir).unwrap();
    let img_png = secret_dir.join("whiteboard.png");
    let img_jpg = secret_dir.join("after-fix.jpg");
    let png_bytes = sample_png(1);
    let jpg_bytes = sample_jpeg();
    fs::write(&img_png, &png_bytes).unwrap();
    fs::write(&img_jpg, &jpg_bytes).unwrap();

    let png_hash = hex::encode(Sha256::digest(&png_bytes));
    let jpg_hash = hex::encode(Sha256::digest(&jpg_bytes));
    let png_rel = format!("attachments/{png_hash}.png");
    let jpg_rel = format!("attachments/{jpg_hash}.jpg");

    let filed = repo
        .arc(&repo.root)
        .args([
            "journal",
            "todo",
            "ui-review",
            "--title",
            "UI review",
            "--attach",
            img_png.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(filed.status.success(), "{filed:?}");
    let stderr = String::from_utf8_lossy(&filed.stderr);
    assert!(stderr.contains(&format!("attached: {png_rel}")), "{stderr}");
    let file = Path::new(String::from_utf8_lossy(&filed.stdout).trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Append with --attach alone (no --body-file required) and deduplicated PNG + new JPEG.
    let appended = repo
        .arc(&repo.root)
        .args([
            "journal",
            "append",
            &file,
            "--title",
            "Follow-up shots",
            "--attach",
            img_png.to_str().unwrap(),
            "--attach",
            img_jpg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(appended.status.success(), "{appended:?}");

    let hot = journal_dir(&repo);
    assert_eq!(
        fs::read(hot.join(&png_rel)).unwrap(),
        png_bytes,
        "PNG blob must match content hash"
    );
    assert_eq!(
        fs::read(hot.join(&jpg_rel)).unwrap(),
        jpg_bytes,
        "JPEG blob must match content hash"
    );
    let att_entries: Vec<_> = fs::read_dir(hot.join("attachments"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        att_entries.len(),
        2,
        "identical PNG must deduplicate to a single blob"
    );

    // Privacy: neither the markdown body nor events.jsonl may leak the source directory path.
    let body = fs::read_to_string(hot.join(&file)).unwrap();
    assert!(
        body.contains(&format!("![whiteboard]({png_rel})")),
        "{body}"
    );
    assert!(body.contains(&format!("![after-fix]({jpg_rel})")), "{body}");
    assert!(
        !body.contains("private-host-folder"),
        "body leaked source host path: {body}"
    );
    let events_text = fs::read_to_string(hot.join("events.jsonl")).unwrap();
    assert!(
        !events_text.contains("private-host-folder"),
        "events.jsonl leaked source host path: {events_text}"
    );

    let shown = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", &file, "--json"]),
    );
    let atts = shown["attachments"].as_array().unwrap();
    assert_eq!(atts.len(), 2, "{shown}");
    assert!(atts.iter().all(|a| a["status"] == "available"), "{shown}");

    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

#[test]
fn rejects_svg_html_non_image_magic_only_empty_and_oversized_attachments() {
    let repo = Repo::new();
    let dir = repo.home.join("bad-inputs");
    fs::create_dir_all(&dir).unwrap();

    let svg = dir.join("diagram.svg");
    fs::write(
        &svg,
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
    )
    .unwrap();
    let disguised_svg = dir.join("sneaky.png");
    fs::write(
        &disguised_svg,
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
    )
    .unwrap();
    let html = dir.join("page.jpg");
    fs::write(&html, b"<!doctype html><script>alert(1)</script>").unwrap();
    let plain = dir.join("text.png");
    fs::write(&plain, b"not an image at all").unwrap();
    let empty = dir.join("empty.png");
    fs::write(&empty, b"").unwrap();
    let sig_only_png = dir.join("sig-only.png");
    fs::write(&sig_only_png, b"\x89PNG\r\n\x1a\n").unwrap();
    let truncated_png = dir.join("trunc.png");
    let valid = sample_png(9);
    fs::write(&truncated_png, &valid[..valid.len() - 4]).unwrap();
    let sig_only_jpg = dir.join("sig-only.jpg");
    fs::write(
        &sig_only_jpg,
        [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02, 0xFF, 0xD9],
    )
    .unwrap();
    let sig_only_webp = dir.join("sig-only.webp");
    fs::write(&sig_only_webp, b"RIFF\x10\x00\x00\x00WEBPVP8 ").unwrap();
    let sig_only_gif = dir.join("sig-only.gif");
    fs::write(&sig_only_gif, b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
    let huge = dir.join("huge.png");
    let mut huge_bytes = sample_png(9);
    huge_bytes.resize(5 * 1024 * 1024 + 1, 0);
    fs::write(&huge, &huge_bytes).unwrap();

    for bad in [
        &svg,
        &disguised_svg,
        &html,
        &plain,
        &empty,
        &sig_only_png,
        &truncated_png,
        &sig_only_jpg,
        &sig_only_webp,
        &sig_only_gif,
        &huge,
        &dir,
    ] {
        let out = repo
            .arc(&repo.root)
            .args([
                "journal",
                "note",
                "rejected",
                "--title",
                "Rejected",
                "--attach",
                bad.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "expected refusal for {}, got success",
            bad.display()
        );
    }

    let hot = journal_dir(&repo);
    assert!(
        !hot.join("attachments").exists(),
        "rejected attachments must leave no blobs on disk"
    );
}

#[test]
fn render_embeds_data_uris_under_csp_and_enforces_metadata_privacy() {
    let repo = Repo::new();
    let dir = repo.home.join("imgs");
    fs::create_dir_all(&dir).unwrap();
    let png_path = dir.join("shot.png");
    let webp_path = dir.join("shot.webp");
    let gif_path = dir.join("shot.gif");
    fs::write(&png_path, sample_png(2)).unwrap();
    fs::write(&webp_path, sample_webp()).unwrap();
    fs::write(&gif_path, sample_gif()).unwrap();

    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "visuals",
                "--body-file",
                "-",
                "--attach",
                png_path.to_str().unwrap(),
                "--attach",
                webp_path.to_str().unwrap(),
                "--attach",
                gif_path.to_str().unwrap(),
            ])
            .write_stdin(
                "# Visuals\n\n![evil](data:image/svg+xml;base64,PHN2Zz4=)\n\n![escape](attachments/../secret.png)\n",
            ),
    );
    let file = Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    let rendered = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "render",
        &file,
        "--fragment",
        "--json",
    ]));
    let html = rendered["pages"][0]["html"].as_str().unwrap();
    assert!(html.contains("src=\"data:image/png;base64,"), "{html}");
    assert!(html.contains("src=\"data:image/webp;base64,"), "{html}");
    assert!(html.contains("src=\"data:image/gif;base64,"), "{html}");
    assert!(!html.contains("data:image/svg+xml"), "{html}");
    assert!(!html.contains("secret.png"), "{html}");
    assert!(!html.to_ascii_lowercase().contains("<script"), "{html}");

    // Metadata privacy: PNG tEXt, zTXt, compressed iTXt, and post-scan progressive JPEG COM
    // carrying private paths/addresses must refuse by default and be fully stripped under --strip-private.
    let cases: [(&str, Vec<u8>, &str); 4] = [
        (
            "leaky-text.png",
            png_with_text_chunk("saved from /home/alice/notes.png via 192.168.1.44"),
            "data:image/png;base64,",
        ),
        (
            "leaky-ztxt.png",
            png_with_ztxt_chunk("compressed /home/alice/private.png"),
            "data:image/png;base64,",
        ),
        (
            "leaky-itxt.png",
            png_with_compressed_itxt_chunk("itxt /home/alice/private.png"),
            "data:image/png;base64,",
        ),
        (
            "leaky-prog.jpg",
            progressive_jpeg_with_post_scan_comment(b"tail /home/alice/private.png"),
            "data:image/jpeg;base64,",
        ),
    ];

    for (idx, (filename, bytes, data_prefix)) in cases.iter().enumerate() {
        let img_path = dir.join(filename);
        fs::write(&img_path, bytes).unwrap();
        let topic = format!("leaky-{idx}");
        let leaky_out = stdout(repo.arc(&repo.root).args([
            "journal",
            "note",
            &topic,
            "--title",
            "Leaky image",
            "--attach",
            img_path.to_str().unwrap(),
        ]));
        let leaky_file = Path::new(leaky_out.trim())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let refused = repo
            .arc(&repo.root)
            .args(["journal", "render", &leaky_file])
            .output()
            .unwrap();
        assert!(
            !refused.status.success(),
            "must refuse private metadata in {filename}"
        );
        let refused_err = String::from_utf8_lossy(&refused.stderr);
        assert!(
            refused_err.contains("/home/alice/"),
            "{filename}: {refused_err}"
        );

        let stripped_html = stdout(repo.arc(&repo.root).args([
            "journal",
            "render",
            &leaky_file,
            "--strip-private",
        ]));
        let decoded = decode_b64_data_uri(&stripped_html, data_prefix);
        assert!(
            !decoded
                .windows(b"/home/alice/".len())
                .any(|w| w == b"/home/alice/"),
            "{filename}: stripped data URI still contains private path bytes"
        );
        assert!(
            !decoded.windows(b"zTXt".len()).any(|w| w == b"zTXt")
                && !decoded.windows(b"iTXt".len()).any(|w| w == b"iTXt")
                && !decoded.windows(b"tEXt".len()).any(|w| w == b"tEXt"),
            "{filename}: stripped PNG still contains text metadata chunk"
        );

        let kept_doc = json_stdout(repo.arc(&repo.root).args([
            "journal",
            "render",
            &leaky_file,
            "--keep-private",
            "--json",
        ]));
        assert!(
            kept_doc["pages"][0]["private_stretches"].as_u64().unwrap() >= 1,
            "{filename}: {kept_doc}"
        );
    }

    // Corrupt compressed zTXt chunk cannot be inspected -> default render refuses, --strip-private succeeds.
    let bad_ztxt_path = dir.join("bad-ztxt.png");
    fs::write(
        &bad_ztxt_path,
        png_with_extra_chunk(8, b"zTXt", b"Comment\0\x00not-valid-zlib"),
    )
    .unwrap();
    let bad_ztxt_file = Path::new(
        stdout(repo.arc(&repo.root).args([
            "journal",
            "note",
            "bad-ztxt",
            "--title",
            "Unreadable zTXt",
            "--attach",
            bad_ztxt_path.to_str().unwrap(),
        ]))
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_string_lossy()
    .to_string();
    repo.arc(&repo.root)
        .args(["journal", "render", &bad_ztxt_file])
        .assert()
        .failure();
    assert!(stdout(repo.arc(&repo.root).args([
        "journal",
        "render",
        &bad_ztxt_file,
        "--strip-private",
    ]))
    .contains("data:image/png;base64,"));
}

#[test]
fn missing_corrupt_symlink_and_orphaned_attachments_degrade_and_diagnose() {
    let repo = Repo::new();
    let png = repo.home.join("diag.png");
    let png_bytes = sample_png(3);
    fs::write(&png, &png_bytes).unwrap();
    let hash = hex::encode(Sha256::digest(&png_bytes));
    let rel = format!("attachments/{hash}.png");

    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "diag",
        "--title",
        "Diagnostics",
        "--attach",
        png.to_str().unwrap(),
    ]));
    let file = Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let hot = journal_dir(&repo);
    let blob_path = hot.join(&rel);

    let sha_present = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "render", &file, "--json"]),
    )["pages"][0]["sha256"]
        .as_str()
        .unwrap()
        .to_string();

    // Missing attachment: safe degradation, no host path leak, digest changes, doctor flags problem.
    fs::remove_file(&blob_path).unwrap();
    let missing_out = repo
        .arc(&repo.root)
        .args(["journal", "render", &file, "--json"])
        .output()
        .unwrap();
    assert!(missing_out.status.success(), "{missing_out:?}");
    let missing_doc: serde_json::Value = serde_json::from_slice(&missing_out.stdout).unwrap();
    let missing_html = missing_doc["pages"][0]["html"].as_str().unwrap();
    assert!(
        missing_html.contains("attachment-missing"),
        "{missing_html}"
    );
    assert!(missing_html.contains(&rel), "{missing_html}");
    assert!(
        !missing_html.contains(&hot.display().to_string()),
        "missing notice leaked host journal path: {missing_html}"
    );
    assert_ne!(
        missing_doc["pages"][0]["sha256"].as_str().unwrap(),
        sha_present,
        "render digest must change when a referenced attachment disappears"
    );
    let doc_missing = repo
        .arc(&repo.root)
        .args(["journal", "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(doc_missing.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&doc_missing.stdout).contains("missing-attachment"),
        "{}",
        String::from_utf8_lossy(&doc_missing.stdout)
    );

    // Corrupt attachment: hash mismatch degrades safely and doctor reports corrupt-attachment.
    fs::write(&blob_path, sample_png(99)).unwrap();
    let corrupt_out = repo
        .arc(&repo.root)
        .args(["journal", "render", &file])
        .output()
        .unwrap();
    assert!(corrupt_out.status.success(), "{corrupt_out:?}");
    let corrupt_html = String::from_utf8_lossy(&corrupt_out.stdout);
    assert!(
        corrupt_html.contains("corrupt attachment"),
        "{corrupt_html}"
    );
    assert!(!corrupt_html.contains("data:image/png"), "{corrupt_html}");
    let doc_corrupt = repo
        .arc(&repo.root)
        .args(["journal", "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(doc_corrupt.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&doc_corrupt.stdout).contains("corrupt-attachment"));

    // Symlink attachment: refused on read and flagged by doctor.
    fs::remove_file(&blob_path).unwrap();
    std::os::unix::fs::symlink(&png, &blob_path).unwrap();
    let sym_out = repo
        .arc(&repo.root)
        .args(["journal", "render", &file])
        .output()
        .unwrap();
    assert!(sym_out.status.success(), "{sym_out:?}");
    let sym_html = String::from_utf8_lossy(&sym_out.stdout);
    assert!(sym_html.contains("corrupt attachment"), "{sym_html}");
    assert!(!sym_html.contains("data:image/png"), "{sym_html}");
    let doc_sym = repo
        .arc(&repo.root)
        .args(["journal", "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(doc_sym.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&doc_sym.stdout).contains("symlink-attachment"));

    // Restore valid blob and add an unreferenced valid orphan -> doctor exits 0 with advice.
    fs::remove_file(&blob_path).unwrap();
    fs::write(&blob_path, &png_bytes).unwrap();
    let orphan_bytes = sample_png(42);
    let orphan_hash = hex::encode(Sha256::digest(&orphan_bytes));
    fs::write(
        hot.join("attachments").join(format!("{orphan_hash}.png")),
        &orphan_bytes,
    )
    .unwrap();
    let doc_orphan = repo
        .arc(&repo.root)
        .args(["journal", "doctor", "--json"])
        .output()
        .unwrap();
    assert!(doc_orphan.status.success(), "{doc_orphan:?}");
    let report: serde_json::Value = serde_json::from_slice(&doc_orphan.stdout).unwrap();
    assert!(
        report["problems"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert!(
        report["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "orphaned-attachment"),
        "{report}"
    );
}

#[test]
fn attachments_travel_through_archive_unarchive_and_relocate_preserving_shared_blobs() {
    let repo = Repo::new();
    let shared = repo.home.join("shared.png");
    let exclusive = repo.home.join("exclusive.png");
    let shared_bytes = sample_png(10);
    let exclusive_bytes = sample_png(20);
    fs::write(&shared, &shared_bytes).unwrap();
    fs::write(&exclusive, &exclusive_bytes).unwrap();
    let shared_rel = format!(
        "attachments/{}.png",
        hex::encode(Sha256::digest(&shared_bytes))
    );
    let exclusive_rel = format!(
        "attachments/{}.png",
        hex::encode(Sha256::digest(&exclusive_bytes))
    );

    let file_a = Path::new(
        stdout(repo.arc(&repo.root).args([
            "journal",
            "todo",
            "alpha",
            "--title",
            "Alpha",
            "--attach",
            shared.to_str().unwrap(),
            "--attach",
            exclusive.to_str().unwrap(),
        ]))
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_string_lossy()
    .to_string();

    let file_b = Path::new(
        stdout(repo.arc(&repo.root).args([
            "journal",
            "todo",
            "beta",
            "--title",
            "Beta",
            "--attach",
            shared.to_str().unwrap(),
        ]))
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_string_lossy()
    .to_string();

    let hot = journal_dir(&repo);
    let cold = cold_dir(&repo);

    // Consume and archive Alpha: both blobs copy to cold; exclusive leaves hot; shared stays hot.
    stdout(repo.arc(&repo.root).args(["journal", "consume", &file_a]));
    stdout(repo.arc(&repo.root).args(["journal", "archive", &file_a]));
    assert!(cold.join(&shared_rel).is_file());
    assert!(cold.join(&exclusive_rel).is_file());
    assert!(!hot.join(&exclusive_rel).exists());
    assert!(hot.join(&shared_rel).is_file());
    assert!(
        stdout(repo.arc(&repo.root).args(["journal", "render", &file_a]))
            .contains("data:image/png;base64,")
    );
    assert!(
        stdout(repo.arc(&repo.root).args(["journal", "render", &file_b]))
            .contains("data:image/png;base64,")
    );
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();

    // Unarchive Alpha: exclusive returns to hot; cold loses both blobs.
    stdout(repo.arc(&repo.root).args(["journal", "unarchive", &file_a]));
    assert!(hot.join(&exclusive_rel).is_file());
    assert!(hot.join(&shared_rel).is_file());
    assert!(!cold.join(&exclusive_rel).exists());
    assert!(!cold.join(&shared_rel).exists());
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();

    // Relocate Alpha outside arc: destination receives both blobs; exclusive leaves hot; shared stays hot for Beta.
    let dest = repo.home.join("relocated-out");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "relocate",
        &file_a,
        "--to",
        dest.to_str().unwrap(),
        "--reason",
        "move off journal",
    ]));
    assert!(dest.join(&file_a).is_file());
    assert_eq!(fs::read(dest.join(&shared_rel)).unwrap(), shared_bytes);
    assert_eq!(
        fs::read(dest.join(&exclusive_rel)).unwrap(),
        exclusive_bytes
    );
    assert!(!hot.join(&exclusive_rel).exists());
    assert!(hot.join(&shared_rel).is_file());
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

#[test]
fn relocating_archived_artifact_preserves_blob_for_live_cross_tier_reference() {
    let repo = Repo::new();
    let img = repo.home.join("cross-tier.png");
    let bytes = sample_png(33);
    fs::write(&img, &bytes).unwrap();
    let rel = format!("attachments/{}.png", hex::encode(Sha256::digest(&bytes)));

    // Archive a todo so the blob exists ONLY in cold storage.
    let archived_todo = Path::new(
        stdout(repo.arc(&repo.root).args([
            "journal",
            "todo",
            "cold-owner",
            "--title",
            "Cold owner",
            "--attach",
            img.to_str().unwrap(),
        ]))
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_string_lossy()
    .to_string();
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "consume", &archived_todo]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "archive", &archived_todo]),
    );

    let hot = journal_dir(&repo);
    let cold = cold_dir(&repo);
    assert!(!hot.join(&rel).exists());
    assert!(cold.join(&rel).is_file());

    // File a live note referencing the same canonical Markdown attachment path.
    let live_note = Path::new(
        stdout(
            repo.arc(&repo.root)
                .args(["journal", "note", "live-ref", "--body-file", "-"])
                .write_stdin(format!("# Live ref\n\n![reused]({rel})\n")),
        )
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_string_lossy()
    .to_string();

    // Relocate ONLY the archived todo. The live note must keep a verified hot copy.
    let dest = repo.home.join("relocated-cold");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "relocate",
        &archived_todo,
        "--to",
        dest.to_str().unwrap(),
        "--reason",
        "move archived todo",
    ]));

    assert_eq!(fs::read(dest.join(&rel)).unwrap(), bytes);
    assert_eq!(
        fs::read(hot.join(&rel)).unwrap(),
        bytes,
        "relocating archived todo must preserve a verified copy in hot for live note"
    );
    assert!(!cold.join(&rel).exists());
    let rendered = stdout(repo.arc(&repo.root).args(["journal", "render", &live_note]));
    assert!(rendered.contains("data:image/png;base64,"), "{rendered}");
    assert!(
        !rendered.contains("class=\"attachment-missing\""),
        "{rendered}"
    );
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

#[test]
fn attachments_round_trip_through_exchange_spool_and_global_promote() {
    let source = Repo::new();
    let recipient = Repo::new();

    // Pair source and recipient replicas under one logical project.
    super::journal_exchange::pair(&source, &recipient, "alpha", "beta");

    let img = source.home.join("diagram.png");
    let img_bytes = sample_png(77);
    fs::write(&img, &img_bytes).unwrap();
    let rel = format!(
        "attachments/{}.png",
        hex::encode(Sha256::digest(&img_bytes))
    );

    // 1. Spool + promote carries attachment into source journal.
    stdout(source.arc(&source.root).args([
        "journal",
        "memory",
        "spooled-rule",
        "--title",
        "Spooled rule",
        "--attach",
        img.to_str().unwrap(),
        "--spool",
    ]));
    stdout(
        source
            .arc(&source.root)
            .args(["journal", "spool", "--promote"]),
    );
    let list = json_stdout(source.arc(&source.root).args(["journal", "list", "--json"]));
    let mem_file = list["artifacts"][0]["file"].as_str().unwrap().to_string();
    assert!(journal_dir(&source).join(&rel).is_file());

    // 2. Export + import carries attachment to paired recipient replica.
    let bundle_path = source.home.join("exchange.json");
    stdout(source.arc(&source.root).args([
        "journal",
        "export",
        &mem_file,
        "--output",
        bundle_path.to_str().unwrap(),
    ]));
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    assert_eq!(
        bundle["attachments"].as_array().unwrap().len(),
        1,
        "{bundle}"
    );

    // Tampered attachment payload must refuse import.
    let mut tampered = bundle.clone();
    tampered["attachments"][0]["data"] = serde_json::json!("AAAA");
    let tampered_path = source.home.join("tampered.json");
    fs::write(&tampered_path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    recipient
        .arc(&recipient.root)
        .args(["journal", "import", tampered_path.to_str().unwrap()])
        .assert()
        .failure();

    // Bundle dropping a referenced attachment must also refuse import.
    let mut dropped = bundle.clone();
    dropped["attachments"] = serde_json::json!([]);
    let dropped_path = source.home.join("dropped.json");
    fs::write(&dropped_path, serde_json::to_vec(&dropped).unwrap()).unwrap();
    recipient
        .arc(&recipient.root)
        .args(["journal", "import", dropped_path.to_str().unwrap()])
        .assert()
        .failure();

    stdout(recipient.arc(&recipient.root).args([
        "journal",
        "import",
        bundle_path.to_str().unwrap(),
    ]));
    assert_eq!(
        fs::read(journal_dir(&recipient).join(&rel)).unwrap(),
        img_bytes
    );
    assert!(stdout(
        recipient
            .arc(&recipient.root)
            .args(["journal", "render", &mem_file])
    )
    .contains("data:image/png;base64,"));
    recipient
        .arc(&recipient.root)
        .args(["journal", "doctor"])
        .assert()
        .success();

    // 3. Global memory promotion copies attachment into operator global journal.
    let promoted = stdout(
        source
            .arc(&source.root)
            .args(["journal", "promote", &mem_file, "--global"]),
    );
    let global_file = Path::new(promoted.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let global_dir = PathBuf::from(
        stdout(
            source
                .arc(&source.root)
                .args(["journal", "--global", "dir"]),
        )
        .trim(),
    );
    assert_eq!(fs::read(global_dir.join(&rel)).unwrap(), img_bytes);
    assert!(stdout(
        source
            .arc(&source.root)
            .args(["journal", "--global", "render", &global_file,])
    )
    .contains("data:image/png;base64,"));
}

#[test]
fn corrupt_destination_attachment_refuses_import_atomically_without_blocking_writes() {
    let source = Repo::new();
    let recipient = Repo::new();
    super::journal_exchange::pair(&source, &recipient, "alpha", "beta");

    let img_a = source.home.join("first.png");
    let img_b = source.home.join("second.png");
    let bytes_a = sample_png(11);
    let bytes_b = sample_png(22);
    fs::write(&img_a, &bytes_a).unwrap();
    fs::write(&img_b, &bytes_b).unwrap();
    let rel_a = format!("attachments/{}.png", hex::encode(Sha256::digest(&bytes_a)));
    let rel_b = format!("attachments/{}.png", hex::encode(Sha256::digest(&bytes_b)));
    // Sort so we corrupt whichever attachment is installed second in sorted order.
    let (first_rel, second_rel) = if rel_a < rel_b {
        (rel_a, rel_b)
    } else {
        (rel_b, rel_a)
    };

    let note_file = Path::new(
        stdout(source.arc(&source.root).args([
            "journal",
            "note",
            "two-imgs",
            "--title",
            "Two images",
            "--attach",
            img_a.to_str().unwrap(),
            "--attach",
            img_b.to_str().unwrap(),
        ]))
        .trim(),
    )
    .file_name()
    .unwrap()
    .to_string_lossy()
    .to_string();

    let bundle_path = source.home.join("two.json");
    stdout(source.arc(&source.root).args([
        "journal",
        "export",
        &note_file,
        "--output",
        bundle_path.to_str().unwrap(),
    ]));

    // Pre-populate a corrupt file at the second attachment's destination in recipient.
    let recipient_hot = journal_dir(&recipient);
    fs::create_dir_all(recipient_hot.join("attachments")).unwrap();
    fs::write(recipient_hot.join(&second_rel), b"corrupt-bytes").unwrap();

    // Import must refuse atomically: first blob is NOT installed, no exchange/pending.json left behind.
    recipient
        .arc(&recipient.root)
        .args(["journal", "import", bundle_path.to_str().unwrap()])
        .assert()
        .failure();
    assert!(
        !recipient_hot.join(&first_rel).exists(),
        "refused import must not install the first attachment"
    );
    assert!(
        !recipient_hot.join("exchange/pending.json").exists(),
        "refused import must not leave a pending exchange transaction"
    );
    assert!(
        !recipient_hot.join(&note_file).exists(),
        "refused import must not write the artifact body"
    );

    // An unrelated write on recipient must succeed immediately (journal is not wedged).
    recipient
        .arc(&recipient.root)
        .args([
            "journal",
            "memory",
            "unrelated",
            "--title",
            "Still writable",
            "--body-file",
            "-",
        ])
        .write_stdin("Journal remains writable.\n")
        .assert()
        .success();
}
