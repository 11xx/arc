use crate::common::*;

/// Synthetic text standing for a passage that must leave the journal.
const SECRET: &str = "SYNTHETIC-SECRET-1234";

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

fn cold_dir(repo: &Repo) -> PathBuf {
    let hot = journal_dir(repo);
    hot.with_file_name(format!(
        "{}-archive",
        hot.file_name().unwrap().to_string_lossy()
    ))
}

fn write_artifact(repo: &Repo, topic: &str, kind: &str, body: &str) -> String {
    let out = stdout(
        repo.arc(&repo.root)
            .args(["journal", "note", topic, "--kind", kind, "--body-file", "-"])
            .write_stdin(body),
    );
    Path::new(out.lines().last().unwrap().trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn add_evidence(repo: &Repo, file: &str, body: &str) {
    repo.arc(&repo.root)
        .args(["journal", "evidence", file, "--body-file", "-"])
        .write_stdin(body)
        .assert()
        .success();
}

fn events(repo: &Repo) -> Vec<serde_json::Value> {
    fs::read_to_string(journal_dir(repo).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn relocate(repo: &Repo, args: &[&str]) -> std::process::Output {
    repo.arc(&repo.root)
        .args(["journal", "relocate"])
        .args(args)
        .output()
        .unwrap()
}

fn text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Every file arc keeps for this repository's journal, hot and cold, with
/// its contents, so a test can say no copy of a passage stayed behind.
fn journal_bytes(repo: &Repo) -> String {
    let mut all = String::new();
    for dir in [journal_dir(repo), cold_dir(repo)] {
        let Ok(walk) = fs::read_dir(&dir) else {
            continue;
        };
        let mut stack: Vec<PathBuf> = walk.map(|entry| entry.unwrap().path()).collect();
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(
                    fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            } else {
                all.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    all
}

/// Claim: relocation moves hot and archived bodies byte-for-byte to the
/// destination, leaves no copy anywhere in the journal, and records a
/// tombstone that every reader reports. Rejects: a relocation that keeps a
/// copy arc can read, or whose artifact reads as merely missing.
#[test]
fn relocation_moves_bodies_out_of_every_store_and_leaves_a_tombstone() {
    let repo = Repo::new();
    let todo = write_artifact(
        &repo,
        "exposed",
        "todo",
        &format!("# Exposed\n\n{SECRET}\n"),
    );
    add_evidence(&repo, &todo, &format!("more {SECRET}\n"));
    let note = write_artifact(
        &repo,
        "shelved",
        "note",
        &format!("# Shelved\n\n{SECRET}\n"),
    );
    repo.arc(&repo.root)
        .args(["journal", "archive", &note])
        .assert()
        .success();
    let hot_body = fs::read(journal_dir(&repo).join(&todo)).unwrap();
    let cold_body = fs::read(cold_dir(&repo).join(&note)).unwrap();
    let outside = repo.home.join("outside/private");

    let dry = relocate(
        &repo,
        &[
            &todo,
            &note,
            "--to",
            outside.to_str().unwrap(),
            "--reason",
            "clean room",
            "--dry-run",
        ],
    );
    assert!(dry.status.success(), "{}", text(&dry));
    assert!(!outside.exists(), "a dry run created the destination");
    assert!(journal_dir(&repo).join(&todo).is_file());

    let moved = relocate(
        &repo,
        &[
            &todo,
            &note,
            "--to",
            outside.to_str().unwrap(),
            "--reason",
            "clean room",
        ],
    );
    assert!(moved.status.success(), "{}", text(&moved));
    assert_eq!(fs::read(outside.join(&todo)).unwrap(), hot_body);
    assert_eq!(fs::read(outside.join(&note)).unwrap(), cold_body);
    assert!(
        !journal_bytes(&repo).contains(SECRET),
        "a copy of the relocated text stayed in the journal"
    );

    let tombstone = events(&repo)
        .into_iter()
        .find(|event| event["event"] == "relocated" && event["file"] == todo.as_str())
        .expect("no tombstone");
    assert_eq!(tombstone["note"], "clean room");
    assert_eq!(
        tombstone["storage_destination"],
        outside.join(&todo).display().to_string()
    );
    assert!(tombstone["storage_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    let shown = repo
        .arc(&repo.root)
        .args(["journal", "show", &note])
        .output()
        .unwrap();
    assert!(!shown.status.success());
    assert!(
        text(&shown).contains("was relocated out of this journal"),
        "{}",
        text(&shown)
    );
    let archived =
        stdout(
            repo.arc(&repo.root)
                .args(["journal", "inventory", "--archived", "--json"]),
        );
    assert!(!archived.contains(&note), "{archived}");
    let filed = json_stdout(repo.arc(&repo.root).args(["journal", "filed", "--json"]));
    let fates: Vec<&str> = filed["sessions"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["fate"].as_str().unwrap())
        .collect();
    assert_eq!(fates, ["relocated", "relocated"], "{filed}");
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));
}

/// Claim: an open claim, unanswered questions, an unfinished storage move,
/// and a destination holding a different body each refuse the relocation
/// before anything moves, and the matching acknowledgement lets it proceed.
/// Rejects: a relocation that strands live ownership, an open decision, or a
/// half-finished move without anybody accepting it.
#[test]
fn relocation_refuses_live_state_until_acknowledged() {
    let repo = Repo::new();
    let outside = repo.home.join("outside");
    let to = outside.to_str().unwrap();
    let claimed = write_artifact(&repo, "claimed", "todo", "# Claimed\n");
    let free = write_artifact(&repo, "free", "todo", "# Free\n");
    repo.arc(&repo.root)
        .args(["claim", &claimed])
        .assert()
        .success();
    let claim_id = events(&repo)
        .into_iter()
        .find(|event| event["event"] == "claim-set")
        .unwrap()["claim_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Every file is checked first: the free todo does not move either.
    let refused = relocate(&repo, &[&free, &claimed, "--to", to, "--reason", "r"]);
    assert!(!refused.status.success());
    assert!(text(&refused).contains("open claim"), "{}", text(&refused));
    assert!(journal_dir(&repo).join(&free).is_file());
    assert!(!outside.join(&free).exists());
    let moved = relocate(
        &repo,
        &[
            &claimed,
            "--to",
            to,
            "--reason",
            "r",
            "--acknowledge-claim",
            &claim_id,
        ],
    );
    assert!(moved.status.success(), "{}", text(&moved));
    let catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(catchup.contains("artifact claims:\n  (none)"), "{catchup}");

    let discussion = write_artifact(&repo, "undecided", "discussion", "# Undecided\n");
    repo.arc(&repo.root)
        .args([
            "journal",
            "question",
            &discussion,
            "--placement",
            "opening",
            "--option",
            "a",
            "--option",
            "b",
            "--body-file",
            "-",
        ])
        .write_stdin("Which one?\n")
        .assert()
        .success();
    let refused = relocate(&repo, &[&discussion, "--to", to, "--reason", "r"]);
    assert!(
        text(&refused).contains("unanswered question"),
        "{}",
        text(&refused)
    );
    assert!(relocate(
        &repo,
        &[&discussion, "--to", to, "--reason", "r", "--drop-questions"]
    )
    .status
    .success());

    // An archive interrupted after its intent: the move never completed.
    let note = write_artifact(&repo, "half-moved", "note", "# Half\n");
    repo.arc(&repo.root)
        .args(["journal", "archive", &note])
        .assert()
        .success();
    let log = journal_dir(&repo).join("events.jsonl");
    let kept: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|line| !(line.contains("\"storage-completed\"") && line.contains(&note)))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&log, kept.concat()).unwrap();
    let refused = relocate(&repo, &[&note, "--to", to, "--reason", "r"]);
    assert!(text(&refused).contains("unfinished"), "{}", text(&refused));
    assert!(cold_dir(&repo).join(&note).is_file());
    let moved = relocate(
        &repo,
        &[
            &note,
            "--to",
            to,
            "--reason",
            "r",
            "--acknowledge-transition",
        ],
    );
    assert!(moved.status.success(), "{}", text(&moved));
    assert!(!cold_dir(&repo).join(&note).exists());

    // A destination that already holds the filename with other bytes.
    fs::write(outside.join(&free), "someone else's file\n").unwrap();
    let refused = relocate(&repo, &[&free, "--to", to, "--reason", "r"]);
    assert!(
        text(&refused).contains("different body"),
        "{}",
        text(&refused)
    );
    assert!(journal_dir(&repo).join(&free).is_file());

    // A destination arc reads is refused outright.
    let inside = journal_dir(&repo).join("kept");
    let refused = relocate(
        &repo,
        &[&free, "--to", inside.to_str().unwrap(), "--reason", "r"],
    );
    assert!(
        text(&refused).contains("which arc reads"),
        "{}",
        text(&refused)
    );
}

/// Claim: links to a relocated artifact say so — the successor's
/// `supersedes` in `filed`, and the source's `superseded by` in the workspace
/// inventory. Rejects: links that read as pointing at nothing.
#[test]
fn links_to_a_relocated_artifact_render_as_relocated() {
    let repo = Repo::new();
    let outside = repo.home.join("outside");
    let source = write_artifact(&repo, "first", "todo", "# First\n");
    repo.arc(&repo.root)
        .args([
            "journal",
            "transition",
            &source,
            "--to",
            "plan",
            "--body-file",
            "-",
            "--reason",
            "planned",
        ])
        .write_stdin("# First plan\n")
        .assert()
        .success();
    let successor = events(&repo)
        .into_iter()
        .find(|event| event["event"] == "transition")
        .unwrap()["file"]
        .as_str()
        .unwrap()
        .to_string();

    let moved = relocate(
        &repo,
        &[&source, "--to", outside.to_str().unwrap(), "--reason", "r"],
    );
    assert!(moved.status.success(), "{}", text(&moved));
    let filed = stdout(repo.arc(&repo.root).args(["journal", "filed"]));
    assert!(
        filed.contains(&format!("supersedes: {source} (relocated)")),
        "{filed}"
    );

    let other = Repo::new();
    let source = write_artifact(&other, "second", "todo", "# Second\n");
    other
        .arc(&other.root)
        .args([
            "journal",
            "transition",
            &source,
            "--to",
            "plan",
            "--body-file",
            "-",
            "--reason",
            "planned",
        ])
        .write_stdin("# Second plan\n")
        .assert()
        .success();
    let successor_other = events(&other)
        .into_iter()
        .find(|event| event["event"] == "transition")
        .unwrap()["file"]
        .as_str()
        .unwrap()
        .to_string();
    let moved = relocate(
        &other,
        &[
            &successor_other,
            "--to",
            other.home.join("outside").to_str().unwrap(),
            "--reason",
            "r",
        ],
    );
    assert!(moved.status.success(), "{}", text(&moved));
    let inventory =
        stdout(
            other
                .arc(&other.root)
                .args(["workspace", "inventory", "--storage", "all"]),
        );
    assert!(
        inventory.contains(&format!("superseded by {successor_other} (relocated)")),
        "{inventory}"
    );
    assert_ne!(successor, successor_other);
    for repo in [&repo, &other] {
        let doctor = stdout_any_status(repo.arc(&repo.root).args(["journal", "doctor"]));
        assert!(doctor.contains("problems:\n  (none)"), "{doctor}");
    }
}

/// Claim: a rerun finishes a relocation interrupted after its tombstone, and
/// doctor names the leftover body until then. Rejects: a tombstone that
/// silently coexists with the body it says left.
#[test]
fn a_rerun_finishes_an_interrupted_relocation() {
    let repo = Repo::new();
    let outside = repo.home.join("outside");
    let file = write_artifact(&repo, "leftover", "note", "# Leftover\n");
    let body = fs::read(journal_dir(&repo).join(&file)).unwrap();
    assert!(relocate(
        &repo,
        &[&file, "--to", outside.to_str().unwrap(), "--reason", "r"]
    )
    .status
    .success());
    fs::write(journal_dir(&repo).join(&file), &body).unwrap();
    let doctor = stdout_any_status(repo.arc(&repo.root).args(["journal", "doctor"]));
    assert!(doctor.contains("relocated-body-present"), "{doctor}");

    let finished = relocate(
        &repo,
        &[&file, "--to", outside.to_str().unwrap(), "--reason", "r"],
    );
    assert!(finished.status.success(), "{}", text(&finished));
    assert!(!journal_dir(&repo).join(&file).exists());
    assert_eq!(
        events(&repo)
            .iter()
            .filter(|event| event["event"] == "relocated")
            .count(),
        1
    );
}

/// Claim: redaction rewrites the base and appended entries of a hot or
/// archived body, records digests and the reason, and never writes the
/// removed text anywhere in the journal, refusals included. Rejects: a
/// redaction that leaks what it removes or breaks an appended entry.
#[test]
fn redaction_rewrites_hot_and_archived_bodies_and_records_only_digests() {
    let repo = Repo::new();
    let file = write_artifact(
        &repo,
        "leaky",
        "todo",
        &format!("# Leaky\n\n{SECRET} here\n"),
    );
    add_evidence(&repo, &file, &format!("and {SECRET} again\n"));

    let missing = repo
        .arc(&repo.root)
        .args([
            "journal",
            "redact",
            &file,
            "--replace",
            "NOT-PRESENT-TEXT",
            "--with",
            "x",
        ])
        .args(["--reason", "r"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        !text(&missing).contains("NOT-PRESENT-TEXT"),
        "{}",
        text(&missing)
    );
    let heading = repo
        .arc(&repo.root)
        .args([
            "journal",
            "redact",
            &file,
            "--replace",
            "### Evidence",
            "--with",
            "### Gone",
        ])
        .args(["--reason", "r"])
        .output()
        .unwrap();
    assert!(!heading.status.success());
    assert!(text(&heading).contains("heading"), "{}", text(&heading));

    let before = fs::read_to_string(journal_dir(&repo).join(&file)).unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "redact",
            &file,
            "--replace",
            SECRET,
            "--with",
            "[removed]",
        ])
        .args(["--reason", "third-party identifier"])
        .assert()
        .success();
    let after = fs::read_to_string(journal_dir(&repo).join(&file)).unwrap();
    assert_eq!(after, before.replace(SECRET, "[removed]"));
    assert!(!journal_bytes(&repo).contains(SECRET));
    let redaction = events(&repo)
        .into_iter()
        .find(|event| event["event"] == "redacted")
        .unwrap();
    assert_eq!(redaction["note"], "third-party identifier");
    let sections: Vec<&str> = redaction["redaction"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|section| section["id"].as_str().unwrap())
        .collect();
    assert_eq!(sections.len(), 2, "{redaction}");
    assert_eq!(sections[0], "base");
    assert!(sections[1].starts_with("evd-"), "{redaction}");
    let shown = stdout(repo.arc(&repo.root).args(["journal", "show", &file]));
    assert!(shown.contains("and [removed] again"), "{shown}");
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));

    let note = write_artifact(&repo, "cold", "note", "# Cold\n\nold wording\n");
    repo.arc(&repo.root)
        .args(["journal", "archive", &note])
        .assert()
        .success();
    let replacement = repo.home.join("replacement.md");
    fs::write(&replacement, "# Cold\n\nnew wording\n").unwrap();
    repo.arc(&repo.root)
        .args(["journal", "redact", &note, "--body-file"])
        .arg(&replacement)
        .args(["--reason", "reworded"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(cold_dir(&repo).join(&note)).unwrap(),
        "# Cold\n\nnew wording\n"
    );
}

/// Two independent replicas of one logical project.
fn paired() -> (Repo, Repo) {
    let source = Repo::new();
    let recipient = Repo::new();
    let recipient_id = json_stdout(
        recipient
            .arc(&recipient.root)
            .args(["replica", "id", "--json"]),
    )["repository_id"]
        .as_str()
        .unwrap()
        .to_string();
    source
        .arc(&source.root)
        .args(["replica", "init", "workstation"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args([
            "replica",
            "pair",
            "agent-host",
            "--repository-id",
            &recipient_id,
        ])
        .assert()
        .success();
    let pairing = source.home.join("pairing.json");
    source
        .arc(&source.root)
        .args(["replica", "export", "--output", pairing.to_str().unwrap()])
        .assert()
        .success();
    recipient
        .arc(&recipient.root)
        .args(["replica", "import", pairing.to_str().unwrap()])
        .assert()
        .success();
    (source, recipient)
}

fn export(repo: &Repo, file: &str, name: &str) -> PathBuf {
    let bundle = repo.home.join(format!("{name}.json"));
    repo.arc(&repo.root)
        .args(["journal", "export", file, "--output"])
        .arg(&bundle)
        .assert()
        .success();
    bundle
}

fn import(repo: &Repo, bundle: &Path) -> std::process::Output {
    repo.arc(&repo.root)
        .args(["journal", "import"])
        .arg(bundle)
        .output()
        .unwrap()
}

/// Claim: import takes a redacted body as the current revision whichever
/// order the bundles arrive in, so a bundle written before the redaction
/// never restores the removed text. Rejects: an import that refuses the
/// redacted body as an edit of the base, or brings the text back.
#[test]
fn exchange_carries_a_redaction_and_never_restores_removed_text() {
    for stale_first in [true, false] {
        let (source, recipient) = paired();
        let file = write_artifact(
            &source,
            "shared",
            "todo",
            &format!("# Shared\n\n{SECRET}\n"),
        );
        add_evidence(&source, &file, &format!("note {SECRET}\n"));
        let stale = export(&source, &file, "stale");
        source
            .arc(&source.root)
            .args([
                "journal",
                "redact",
                &file,
                "--replace",
                SECRET,
                "--with",
                "[removed]",
            ])
            .args(["--reason", "r"])
            .assert()
            .success();
        let redacted = export(&source, &file, "redacted");
        let order = if stale_first {
            [&stale, &redacted]
        } else {
            [&redacted, &stale]
        };
        for bundle in order {
            let imported = import(&recipient, bundle);
            assert!(
                imported.status.success(),
                "stale first {stale_first}: {}",
                text(&imported)
            );
        }
        let body = fs::read_to_string(journal_dir(&recipient).join(&file)).unwrap();
        assert!(!body.contains(SECRET), "stale first {stale_first}: {body}");
        assert_eq!(
            body,
            fs::read_to_string(journal_dir(&source).join(&file)).unwrap()
        );
        recipient
            .arc(&recipient.root)
            .args(["journal", "doctor"])
            .assert()
            .success();
    }
}

/// Claim: exchange cannot carry a relocated artifact: export refuses it,
/// and an import holds back a peer's copy of one with what references it.
/// Rejects: an exchange that reinstalls a body the journal relocated.
#[test]
fn exchange_neither_carries_nor_restores_a_relocated_artifact() {
    let (source, recipient) = paired();
    let file = write_artifact(
        &source,
        "shared",
        "note",
        &format!("# Shared\n\n{SECRET}\n"),
    );
    let bundle = export(&source, &file, "shared");
    import(&recipient, &bundle);
    let moved = relocate(
        &recipient,
        &[
            &file,
            "--to",
            recipient.home.join("outside").to_str().unwrap(),
            "--reason",
            "r",
        ],
    );
    assert!(moved.status.success(), "{}", text(&moved));

    let refused = recipient
        .arc(&recipient.root)
        .args(["journal", "export", &file, "--output"])
        .arg(recipient.home.join("again.json"))
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("was relocated"),
        "{}",
        text(&refused)
    );

    source
        .arc(&source.root)
        .args(["journal", "evidence", &file, "--body-file", "-"])
        .write_stdin("later\n")
        .assert()
        .success();
    let newer = export(&source, &file, "newer");
    let imported = import(&recipient, &newer);
    assert!(imported.status.success(), "{}", text(&imported));
    assert!(text(&imported).contains("held back"), "{}", text(&imported));
    assert!(!journal_dir(&recipient).join(&file).exists());
    assert!(!journal_bytes(&recipient).contains(SECRET));
}
