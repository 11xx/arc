use super::common::*;

fn append(repo: &Repo, verb: &str, file: &str, body: &str, title: Option<&str>) -> String {
    let mut cmd = repo.arc(&repo.root);
    cmd.args(["journal", verb, file, "--body-file", "-"]);
    if let Some(title) = title {
        cmd.args(["--title", title]);
    }
    stdout(cmd.write_stdin(body));
    let dir = PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim());
    journal_event_log(&dir)
        .iter()
        .rev()
        .find(|event| event["event"] == "evidence")
        .and_then(|event| event["evidence_id"].as_str())
        .unwrap()
        .to_string()
}

/// Claim: `append --title` writes the title after the heading's writer and
/// time, records it on the event, and leaves the block addressable by its
/// ID, so `show <id>`, `show --json` and `doctor` all keep working. Rejects a
/// title that breaks the heading grammar readers key on.
#[test]
fn a_titled_block_keeps_its_id_addressable() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "rendering-levels", "note", "# Rendering levels\n");
    let id = append(
        &repo,
        "append",
        &file,
        "Wayland compositors differ in damage tracking.\n",
        Some("Compositor findings"),
    );

    let body = fs::read_to_string(dir.join(&file)).unwrap();
    let heading = body
        .lines()
        .find(|line| line.starts_with(&format!("### Evidence {id} (")))
        .unwrap();
    assert!(heading.ends_with("): Compositor findings"), "{heading}");
    let event = journal_event_log(&dir)
        .into_iter()
        .find(|event| event["evidence_id"] == id.as_str())
        .unwrap();
    assert_eq!(event["title"], "Compositor findings");

    let entry = stdout(repo.arc(&repo.root).args(["journal", "show", &id]));
    assert!(entry.contains("damage tracking"), "{entry}");
    let shown = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", &file, "--json"]),
    );
    assert_eq!(shown["evidence"][0]["id"], id.as_str());
    assert_eq!(shown["evidence"][0]["title"], "Compositor findings");
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

/// Claim: `append` is the `evidence` command under another name; an
/// untitled block is unchanged. Rejects an alias that writes a different
/// record from the command it names.
#[test]
fn append_and_evidence_write_the_same_record() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "alias", "note", "# Alias\n");
    let first = append(&repo, "evidence", &file, "One.\n", None);
    let second = append(&repo, "append", &file, "Two.\n", None);
    let body = fs::read_to_string(dir.join(&file)).unwrap();
    for id in [&first, &second] {
        let heading = body
            .lines()
            .find(|line| line.starts_with(&format!("### Evidence {id} (")))
            .unwrap();
        assert!(heading.ends_with(')'), "{heading}");
    }
    let events: Vec<_> = journal_event_log(&dir)
        .into_iter()
        .filter(|event| event["event"] == "evidence")
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.get("title").is_none()));
}

/// Claim: a block title is corrected through `journal correct`, and every
/// view reads the corrected title. Rejects a title that is permanent once
/// mistyped.
#[test]
fn a_block_title_is_corrected_in_place_of_rewriting() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "typo", "note", "# Typo\n");
    let id = append(&repo, "append", &file, "Body.\n", Some("Compsitor"));
    stdout(repo.arc(&repo.root).args([
        "journal",
        "correct",
        &file,
        "--target",
        &id,
        "--field",
        "title",
        "--value",
        "Compositor",
        "--note",
        "spelling",
    ]));
    let shown = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", &file, "--json"]),
    );
    assert_eq!(shown["evidence"][0]["title"], "Compositor");
    stdout(repo.arc(&repo.root).args(["journal", "pin", &file]));
    let pins = json_stdout(repo.arc(&repo.root).args(["journal", "pins", "--json"]));
    assert_eq!(pins["pins"][0]["last_block"]["title"], "Compositor");
}

/// Claim: a title shares the heading line, so it must be one nonempty line.
/// Rejects a title that would split the heading and orphan the block.
#[test]
fn a_block_title_is_one_line() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "lines", "note", "# Lines\n");
    for title in ["two\nlines", "   "] {
        repo.arc(&repo.root)
            .args([
                "journal",
                "append",
                &file,
                "--body-file",
                "-",
                "--title",
                title,
            ])
            .write_stdin("Body.\n")
            .assert()
            .failure();
    }
    assert!(!fs::read_to_string(dir.join(&file))
        .unwrap()
        .contains("### Evidence"));
}
