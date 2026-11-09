use super::common::*;

fn open_files(repo: &Repo) -> Vec<String> {
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    open["open"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["file"].as_str().unwrap().to_string())
        .collect()
}

fn pins(repo: &Repo) -> Vec<serde_json::Value> {
    let listed = json_stdout(repo.arc(&repo.root).args(["journal", "pins", "--json"]));
    assert_eq!(listed["schema"], "arc-journal-pins/1");
    listed["pins"].as_array().unwrap().clone()
}

fn append_evidence(repo: &Repo, file: &str, body: &str) -> String {
    let source = repo.home.join("evidence-body.md");
    fs::write(&source, body).unwrap();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "evidence",
        file,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    let log = journal_event_log(&stdout_dir(repo));
    log.iter()
        .rev()
        .find(|event| event["event"] == "evidence")
        .and_then(|event| event["evidence_id"].as_str())
        .unwrap()
        .to_string()
}

fn stdout_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

/// Claim: a pinned todo is a reference, not work, so `journal open` stops
/// listing it while `catchup` lists it first, and unpinning returns it to the
/// queue. Rejects a pin that leaves the artifact in the open queue.
#[test]
fn pinned_todo_leaves_the_open_queue_until_unpinned() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "render-levels", "todo", "# Render levels\n");
    assert_eq!(open_files(&repo), vec![file.clone()]);

    let pinned = stdout(repo.arc(&repo.root).args([
        "journal",
        "pin",
        &file,
        "--reason",
        "standing reference for compositor work",
    ]));
    assert_eq!(pinned.trim(), format!("pinned: {file}"));
    assert!(open_files(&repo).is_empty(), "a pinned todo left in open");

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    let pinned_at = catchup.find("pinned (1):").expect(&catchup);
    let lanes_at = catchup.find("lanes:").expect(&catchup);
    assert!(
        pinned_at < lanes_at,
        "pins are not listed first:\n{catchup}"
    );
    assert!(catchup.contains("standing reference for compositor work"));
    let journal_catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(journal_catchup.contains(&format!("  {file}  Render levels")));

    let unpinned = stdout(repo.arc(&repo.root).args(["journal", "unpin", &file]));
    assert_eq!(unpinned.trim(), format!("unpinned: {file}"));
    assert_eq!(open_files(&repo), vec![file]);
    assert!(pins(&repo).is_empty());
}

/// Claim: `journal pins` reports the reason, the appended block count and the
/// newest block, for a living document that keeps growing. Rejects a listing
/// that counts nothing appended after the pin or names an older block.
#[test]
fn pins_report_reason_and_appended_blocks() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "research", "note", "# Research notes\n\nBody.\n");
    stdout(repo.arc(&repo.root).args(["journal", "pin", &file]));
    append_evidence(&repo, &file, "First finding.\n");
    let newest = append_evidence(&repo, &file, "Second finding.\n");

    let rows = pins(&repo);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["file"], file.as_str());
    assert_eq!(row["title"], "Research notes");
    assert_eq!(row["kind"], "note");
    assert_eq!(row["reason"], serde_json::Value::Null);
    assert_eq!(row["storage"], "hot");
    assert_eq!(row["blocks"], 2);
    assert_eq!(row["last_block"]["id"], newest.as_str());
    assert_eq!(row["last_block"]["kind"], "Evidence");
    assert_eq!(row["pinned_by"]["actor"], "tester");

    let text = stdout(repo.arc(&repo.root).args(["journal", "pins"]));
    assert!(text.contains("pinned (1):"), "{text}");
    assert!(text.contains(&format!("2 blocks, last {newest}")), "{text}");
}

/// Claim: pinning again with the recorded reason writes nothing, a new
/// reason replaces the old one, and unpinning what is not pinned refuses.
/// Rejects duplicate pin events and a silent unpin of nothing.
#[test]
fn repinning_is_idempotent_and_unpin_needs_a_pin() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "repin", "note", "# Repin\n");
    let unpinned = repo
        .arc(&repo.root)
        .args(["journal", "unpin", &file])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&unpinned.get_output().stderr).to_string();
    assert!(
        stderr.contains(&format!("{file} is not pinned")),
        "{stderr}"
    );

    stdout(
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, "--reason", "one"]),
    );
    let again = stdout(
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, "--reason", "one"]),
    );
    assert_eq!(again.trim(), format!("already pinned: {file}"));
    let count = |dir: &Path| {
        journal_event_log(dir)
            .iter()
            .filter(|event| event["event"] == "pinned")
            .count()
    };
    assert_eq!(count(&dir), 1);

    stdout(
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, "--reason", "two"]),
    );
    assert_eq!(count(&dir), 2);
    assert_eq!(pins(&repo)[0]["reason"], "two");

    repo.arc(&repo.root)
        .args(["journal", "pin", &file, "--reason", "two\nlines"])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

/// Claim: `archive --consumed` keeps a consumed pinned artifact live, and an
/// explicit archive or relocation refuses until it is unpinned. Rejects a
/// sweep that archives a standing reference.
#[test]
fn pinned_artifacts_stay_out_of_archive_sweeps_and_moves() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "finished-plan", "plan", "# Finished plan\n");
    stdout(repo.arc(&repo.root).args(["journal", "pin", &file]));
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "consume", &file, "--outcome", "done"]),
    );

    let sweep = repo
        .arc(&repo.root)
        .args(["journal", "archive", "--consumed"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&sweep.get_output().stderr).to_string();
    assert!(
        stderr.contains(&format!("kept, pinned: {file}")),
        "{stderr}"
    );
    assert!(
        dir.join(&file).is_file(),
        "the sweep archived a pinned plan"
    );
    assert_eq!(pins(&repo)[0]["consumed"], true);

    let refused = repo
        .arc(&repo.root)
        .args(["journal", "archive", &file])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(
        stderr.contains(&format!("{file} is pinned; `arc journal unpin {file}`")),
        "{stderr}"
    );
    let outside = repo.home.join("moved-out");
    let relocated = repo
        .arc(&repo.root)
        .args([
            "journal",
            "relocate",
            &file,
            "--to",
            outside.to_str().unwrap(),
            "--reason",
            "clean room",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&relocated.get_output().stderr).to_string();
    assert!(stderr.contains("is pinned"), "{stderr}");
    assert!(dir.join(&file).is_file());

    stdout(repo.arc(&repo.root).args(["journal", "unpin", &file]));
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "archive", "--consumed"]),
    );
    assert!(!dir.join(&file).exists(), "unpinned plan was not archived");
}

/// Claim: a pin needs the body in hot storage, so an archived artifact
/// refuses with the command that brings it back. Rejects a pin on a body
/// nothing can append to.
#[test]
fn pin_refuses_an_archived_artifact() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "shelved", "note", "# Shelved\n");
    stdout(repo.arc(&repo.root).args(["journal", "archive", &file]));
    let refused = repo
        .arc(&repo.root)
        .args(["journal", "pin", &file])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).to_string();
    assert!(
        stderr.contains(&format!(
            "{file} is archived; `arc journal unarchive {file}`"
        )),
        "{stderr}"
    );
}

/// Claim: an untouched pinned todo is parked, not stale, in `journal filed`.
/// Rejects counting a standing reference as neglected work.
#[test]
fn filed_never_marks_a_pinned_item_stale() {
    let repo = Repo::new();
    let (dir, pinned) = journal_artifact(&repo, "parked-todo", "todo", "# Parked\n");
    let (_, plain) = journal_artifact(&repo, "plain-todo", "todo", "# Plain\n");
    stdout(repo.arc(&repo.root).args(["journal", "pin", &pinned]));
    age_journal_events(&dir, 30 * 86_400);

    let filed = json_stdout(repo.arc(&repo.root).args(["journal", "filed", "--json"]));
    let fate = |file: &str| {
        filed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|session| session["items"].as_array().unwrap().iter())
            .find(|item| item["file"] == file)
            .map(|item| item["fate"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(fate(&plain), "stale");
    assert_eq!(fate(&pinned), "open");
}

/// Claim: `--global` pins in the operator journal and `journal pins
/// --global` lists them, apart from the project's own. Rejects a pin that
/// lands in the project journal.
#[test]
fn global_pins_live_in_the_operator_journal() {
    let repo = Repo::new();
    let printed = stdout(repo.arc(&repo.root).args([
        "journal",
        "--global",
        "note",
        "operator-reference",
        "--title",
        "Operator reference",
    ]));
    let file = Path::new(printed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "--global", "pin", &file]),
    );
    let global = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "--global", "pins", "--json"]),
    );
    assert_eq!(global["pins"][0]["file"], file.as_str());
    assert_eq!(global["pins"][0]["title"], "Operator reference");
    assert!(
        pins(&repo).is_empty(),
        "a global pin reached the project journal"
    );
}
