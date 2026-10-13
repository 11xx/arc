use crate::common::*;

/// A second project under the fixture's home, with one todo and one
/// discussion filed in its journal, then deleted: the registration a probe
/// repository leaves behind. Returns its anchor and journal directory.
fn vanished_probe(repo: &Repo) -> (PathBuf, PathBuf) {
    let probe = repo.home.join("probe");
    fs::create_dir_all(&probe).unwrap();
    git(&probe, &["init", "-b", "master"]);
    let body = repo.home.join("probe-body.md");
    fs::write(&body, "left behind\n").unwrap();
    for (topic, kind) in [("stranded", "todo"), ("undecided", "discussion")] {
        repo.arc(&probe)
            .args(["journal", "note", topic, "--kind", kind, "--body-file"])
            .arg(&body)
            .assert()
            .success();
    }
    let journal = PathBuf::from(stdout(repo.arc(&probe).args(["journal", "dir"])).trim());
    let anchor = fs::canonicalize(&probe).unwrap();
    fs::remove_dir_all(&probe).unwrap();
    (anchor, journal)
}

fn artifact(journal: &Path, kind: &str) -> String {
    fs::read_dir(journal)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .find(|name| name.ends_with(&format!("-{kind}.md")))
        .unwrap_or_else(|| panic!("no {kind} in {}", journal.display()))
}

fn bindings(journal: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(journal.join("bindings.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn retire(repo: &Repo, journal: &Path, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let output = repo
        .arc(&repo.root)
        .args(["workspace", "retire"])
        .arg(journal)
        .args(["--reason", "probe repository deleted", "--json"])
        .args(extra)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), value)
}

fn report(repo: &Repo, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let output = repo
        .arc(&repo.root)
        .args(["workspace", "report", "--global", "--json"])
        .args(extra)
        .output()
        .unwrap();
    (
        output.status.code(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

/// Claim: retirement shows what the registration holds and refuses, writing
/// nothing, while unresolved artifacts remain — in a dry run and for real.
/// Rejects: a retirement that strands an open todo or an undecided discussion
/// out of every view without anybody choosing to shelve it.
#[test]
fn retirement_refuses_while_unresolved_artifacts_remain() {
    let repo = Repo::new();
    let (anchor, journal) = vanished_probe(&repo);
    let todo = artifact(&journal, "todo");
    let discussion = artifact(&journal, "discussion");
    let before = fs::read(journal.join("bindings.jsonl")).unwrap();

    for extra in [&["--dry-run"][..], &[][..]] {
        let (code, value) = retire(&repo, &journal, extra);
        assert_eq!(code, Some(1), "{value}");
        assert_eq!(value["schema"], "arc-workspace-retirement/1");
        assert_eq!(value["anchor"], anchor.display().to_string());
        assert_eq!(value["anchor_exists"], false);
        let held: Vec<&str> = value["holdings"]["unresolved"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["file"].as_str().unwrap())
            .collect();
        assert!(held.contains(&todo.as_str()), "{value}");
        assert!(held.contains(&discussion.as_str()), "{value}");
        assert!(
            value["refused"]
                .as_str()
                .unwrap()
                .contains("2 unresolved artifacts"),
            "{value}"
        );
        assert!(value["retirement"].is_null(), "{value}");
    }
    assert_eq!(fs::read(journal.join("bindings.jsonl")).unwrap(), before);
    assert!(journal.join(&todo).is_file());

    // Still a failed observation: nothing was retired.
    let (code, value) = report(&repo, &[]);
    assert_eq!(code, Some(16), "{value}");
    assert_eq!(value["collection"]["retired"], 0, "{value}");
}

/// Claim: `--archive-unresolved` shelves each unresolved artifact in the cold
/// archive with the reason, records the retirement, and leaves the history
/// readable from the journal directory. Rejects: a retirement that deletes or
/// hides journal history, or records no reason.
#[test]
fn retirement_archives_unresolved_artifacts_and_keeps_history_readable() {
    let repo = Repo::new();
    let (anchor, journal) = vanished_probe(&repo);
    let todo = artifact(&journal, "todo");

    let (code, value) = retire(&repo, &journal, &["--dry-run", "--archive-unresolved"]);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["dry_run"], true);
    assert!(journal.join(&todo).is_file(), "a dry run moved {todo}");
    assert_eq!(bindings(&journal).last().unwrap()["event"], "bound");

    let (code, value) = retire(&repo, &journal, &["--archive-unresolved"]);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["archived"].as_array().unwrap().len(), 2, "{value}");
    assert_eq!(value["retirement"]["reason"], "probe repository deleted");
    assert_eq!(value["retirement"]["anchor"], anchor.display().to_string());

    let last = bindings(&journal).pop().unwrap();
    assert_eq!(last["event"], "retired");
    assert_eq!(last["reason"], "probe repository deleted");
    assert_eq!(last["anchor"], anchor.display().to_string());

    // The body moved to the cold archive, and reads back from it.
    assert!(!journal.join(&todo).exists());
    let cold = journal.with_file_name(format!(
        "{}-archive",
        journal.file_name().unwrap().to_string_lossy()
    ));
    assert!(cold.join(&todo).is_file());
    let read = |args: &[&str]| {
        stdout(
            repo.arc(&repo.root)
                .env("ARC_JOURNAL_DIR", &journal)
                .args(["journal"])
                .args(args),
        )
    };
    let shown = read(&["show", &todo]);
    assert!(shown.contains("left behind"), "{shown}");
    let events = read(&["events"]);
    let archived = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["event"] == "storage-completed" && event["file"] == todo.as_str())
        .unwrap_or_else(|| panic!("no archive event for {todo}: {events}"));
    assert_eq!(archived["outcome"], "unresolved", "{archived}");

    let (code, value) = retire(&repo, &journal, &[]);
    assert_eq!(code, Some(1), "{value}");
    assert!(
        value["refused"]
            .as_str()
            .unwrap()
            .contains("already retired"),
        "{value}"
    );
}

/// Claim: the workspace views stop observing a retired registration — no
/// failed collection, no exit 16, no unreachable-anchor attention — count it
/// as retired, and list it under `--retired`. Rejects: a report that keeps
/// failing on an anchor the operator deliberately retired.
///
/// The retired binding is written as the retirement writes it, so the
/// readers are tested apart from the command.
#[test]
fn workspace_views_stop_failing_on_a_retired_missing_anchor() {
    let repo = Repo::new();
    let journal = repo.home.join(".local/ai/journals/-gone-away-project");
    fs::create_dir_all(&journal).unwrap();
    fs::write(
        journal.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-01T00:00:00Z\",\
         \"event\":\"bound\",\"anchor\":\"/gone/away/project\"}\n\
         {\"schema\":\"journal-binding/1\",\"ts\":\"2026-01-02T00:00:00Z\",\
         \"event\":\"retired\",\"anchor\":\"/gone/away/project\",\
         \"reason\":\"probe deleted\"}\n",
    )
    .unwrap();

    let (code, value) = report(&repo, &[]);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["collection"]["failed"], 0, "{value}");
    assert_eq!(value["collection"]["retired"], 1, "{value}");
    assert!(value.get("retired").is_none(), "{value}");
    assert!(
        value["attention"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| !entry["rule"].as_str().unwrap().starts_with("unreachable")),
        "{value}"
    );

    let (code, value) = report(&repo, &["--retired"]);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["retired"][0]["slug"], "-gone-away-project", "{value}");
    assert_eq!(value["retired"][0]["reason"], "probe deleted", "{value}");

    let mut backlog = repo.arc(&repo.root);
    backlog.args(["workspace", "backlog", "--json", "--retired"]);
    let backlog = json_stdout(&mut backlog);
    assert_eq!(backlog["unreachable"].as_array().unwrap().len(), 0);
    assert_eq!(backlog["retired"][0]["anchor"], "/gone/away/project");

    repo.arc(&repo.root)
        .args(["workspace", "inventory", "--json"])
        .assert()
        .code(0);
    let doctor = stdout_any_status(
        repo.arc(&repo.root)
            .env("ARC_JOURNAL_DIR", &journal)
            .args(["journal", "doctor", "--json"]),
    );
    assert!(doctor.contains("\"problems\""), "{doctor}");
    assert!(!doctor.contains("orphaned-journal"), "{doctor}");
}

/// Claim: retirement is reversible, by an explicit restore or by a journal
/// write from a checkout at the anchor, and either is recorded. Rejects: a
/// retirement that cannot be undone, or one that silently outlives the
/// project's return.
#[test]
fn restore_or_a_write_from_the_anchor_ends_a_retirement() {
    let repo = Repo::new();
    let (anchor, journal) = vanished_probe(&repo);
    let (code, value) = retire(&repo, &journal, &["--archive-unresolved"]);
    assert_eq!(code, Some(0), "{value}");

    let restored = stdout(
        repo.arc(&repo.root)
            .args(["workspace", "restore"])
            .arg(&anchor)
            .args(["--reason", "needed after all", "--json"]),
    );
    let restored: serde_json::Value = serde_json::from_str(&restored).unwrap();
    assert!(restored["retirement"].is_null(), "{restored}");
    let last = bindings(&journal).pop().unwrap();
    assert_eq!(last["event"], "restored");
    assert_eq!(last["reason"], "needed after all");
    let (code, value) = report(&repo, &[]);
    assert_eq!(
        code,
        Some(16),
        "a restored missing anchor is unreachable again: {value}"
    );

    repo.arc(&repo.root)
        .args(["workspace", "restore"])
        .arg(&journal)
        .args(["--reason", "again"])
        .assert()
        .code(1);

    // Retired again, then the project comes back and writes to its journal.
    let (code, value) = retire(&repo, &journal, &[]);
    assert_eq!(code, Some(0), "{value}");
    fs::create_dir_all(&anchor).unwrap();
    git(&anchor, &["init", "-b", "master"]);
    repo.arc(&anchor)
        .args(["journal", "log", "back", "the probe returned"])
        .assert()
        .success();
    let last = bindings(&journal).pop().unwrap();
    assert_eq!(last["event"], "bound", "{last}");
    assert_eq!(last["anchor"], anchor.display().to_string());
    let (code, value) = report(&repo, &[]);
    assert_eq!(code, Some(0), "{value}");
    assert_eq!(value["collection"]["retired"], 0, "{value}");
}

/// Claim: an artifact filed after retirement's first look, but before its
/// record, refuses the retirement instead of being stranded. The test holds
/// the journal's events lock so the retirement waits between its check and
/// its record, files a todo there, then lets it go.
/// Rejects: a check-then-write race that retires a journal holding new work.
#[test]
fn an_artifact_filed_during_retirement_refuses_it() {
    let repo = Repo::new();
    let (_, journal) = vanished_probe(&repo);
    let (code, value) = retire(&repo, &journal, &["--archive-unresolved"]);
    assert_eq!(code, Some(0), "{value}");
    repo.arc(&repo.root)
        .args(["workspace", "restore"])
        .arg(&journal)
        .args(["--reason", "retry"])
        .assert()
        .success();

    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(journal.join(".locks/events.lock"))
        .unwrap();
    lock.lock().unwrap();
    let path = journal.display().to_string();
    let mut child = spawn_arc_keeping_stderr(
        &repo,
        &repo.root,
        &["workspace", "retire", &path, "--reason", "racing"],
        "session-a",
    );
    thread::sleep(Duration::from_millis(400));
    fs::write(journal.join("20260101T000000Z-late-todo.md"), "# Late\n").unwrap();
    lock.unlock().unwrap();
    let status = wait_for_exit(&mut child);
    let stderr = child_stderr(&mut child);
    assert!(!status.success(), "{stderr}");
    assert!(
        stderr.contains("the journal changed while it was being retired"),
        "{stderr}"
    );
    assert_eq!(bindings(&journal).pop().unwrap()["event"], "restored");
}
