use super::common::*;

fn sample_png(tag: u8) -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, tag]);
    png.extend_from_slice(&0u32.to_be_bytes());
    png.extend_from_slice(b"IEND");
    png.extend_from_slice(&[0xAE, 0x42, 0x60, 0x82]);
    png
}

fn png_with_text_chunk(text: &str) -> Vec<u8> {
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

fn sample_jpeg() -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x02, 0xFF, 0xD9]
}

fn sample_webp() -> Vec<u8> {
    let mut buf = b"RIFF".to_vec();
    buf.extend_from_slice(&12u32.to_le_bytes());
    buf.extend_from_slice(b"WEBPVP8 ");
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf
}

fn sample_gif() -> Vec<u8> {
    let mut buf = b"GIF89a".to_vec();
    buf.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0, 0x3B]);
    buf
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
fn rejects_svg_html_non_image_empty_and_oversized_attachments() {
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
    let huge = dir.join("huge.png");
    let mut huge_bytes = sample_png(9);
    huge_bytes.resize(5 * 1024 * 1024 + 1, 0);
    fs::write(&huge, &huge_bytes).unwrap();

    for bad in [&svg, &disguised_svg, &html, &plain, &empty, &huge, &dir] {
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

    // Metadata privacy: PNG tEXt chunk with local path & private IP must be
    // refused by default, stripped under --strip-private, and kept under --keep-private.
    let leaky_png = dir.join("leaky.png");
    fs::write(
        &leaky_png,
        png_with_text_chunk("saved from /home/alice/notes.png via 192.168.1.44"),
    )
    .unwrap();
    let leaky_out = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        "leaky",
        "--title",
        "Leaky image",
        "--attach",
        leaky_png.to_str().unwrap(),
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
        "must refuse private image metadata"
    );
    let refused_err = String::from_utf8_lossy(&refused.stderr);
    assert!(
        refused_err.contains("/home/alice/notes.png"),
        "{refused_err}"
    );
    assert!(refused_err.contains("192.168.1.44"), "{refused_err}");

    let stripped =
        stdout(
            repo.arc(&repo.root)
                .args(["journal", "render", &leaky_file, "--strip-private"]),
        );
    assert!(stripped.contains("data:image/png;base64,"), "{stripped}");
    // Extract base64 payload and confirm metadata strings were stripped from PNG bytes.
    let b64_start =
        stripped.find("data:image/png;base64,").unwrap() + "data:image/png;base64,".len();
    let b64_end = stripped[b64_start..].find('"').unwrap() + b64_start;
    let b64 = &stripped[b64_start..b64_end];
    let stripped_doc = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "render",
        &leaky_file,
        "--keep-private",
        "--json",
    ]));
    assert_eq!(stripped_doc["pages"][0]["private_stretches"], 2);
    assert!(
        !stripped_doc["pages"][0]["html"]
            .as_str()
            .unwrap()
            .contains(b64),
        "stripped PNG bytes must differ from unstripped bytes"
    );
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
