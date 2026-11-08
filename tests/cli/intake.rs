//! Intake items as journal sources.
//!
//! An intake source is identified by the intake and the item alone, so the
//! same item filed under the same key by any session is one item, while the
//! session that writes stays recorded as the writer.

use super::common::*;

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

fn journal_events(repo: &Repo) -> Vec<serde_json::Value> {
    match fs::read_to_string(journal_dir(repo).join("events.jsonl")) {
        Ok(text) => text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("{error}"),
    }
}

fn todos(repo: &Repo) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(journal_dir(repo))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
                .filter(|name| name.contains("-todo"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn file_of(printed: &str) -> String {
    Path::new(printed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

/// File a todo as `session` from `spec`, keyed by `key`, and return what the
/// write printed.
fn file_as(repo: &Repo, session: &str, spec: &str, key: &str) -> String {
    stdout(repo.arc(&repo.root).env("ARC_SESSION", session).args([
        "journal",
        "todo",
        "triaged",
        "--title",
        "Triaged",
        "--source",
        spec,
        "--item-key",
        key,
    ]))
}

#[test]
fn an_intake_item_filed_by_two_sessions_is_one_item() {
    let repo = Repo::new();
    let first = file_as(&repo, "triage-one", "intake=tray item=T1", "T1");
    let file = file_of(&first);
    let before = journal_events(&repo).len();

    let second = file_as(&repo, "triage-two", "intake=tray item=T1", "T1");
    assert_eq!(second, format!("existing: {file} [open]\n"));
    assert_eq!(journal_events(&repo).len(), before, "the repeat wrote");
    assert_eq!(todos(&repo), vec![file.clone()]);

    // The item is the source; the first session stays the writer.
    let event = journal_events(&repo)
        .into_iter()
        .find(|event| event["file"] == file.as_str())
        .unwrap();
    assert_eq!(event["session"], "triage-one", "{event}");
    assert_eq!(
        event["source"],
        serde_json::json!({"intake": "tray", "item": "T1"}),
        "{event}"
    );
    assert_eq!(event["item_key"], "T1", "{event}");
}

#[test]
fn another_item_or_intake_under_the_same_key_is_another_item() {
    let repo = Repo::new();
    let mut written = Vec::new();
    for spec in [
        "intake=tray item=T1",
        "intake=tray item=T2",
        "intake=inbox item=T1",
        "harness=claude session=T1 ts=2026-09-03T00:00:00Z",
    ] {
        let printed = file_as(&repo, "triage", spec, "K");
        assert!(!printed.starts_with("existing:"), "{spec}: {printed}");
        written.push(file_of(&printed));
    }
    let mut distinct = written.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "{written:?}");
    assert_eq!(todos(&repo).len(), 4);
}

#[test]
fn a_malformed_intake_source_is_refused_before_any_write() {
    let repo = Repo::new();
    let too_long = format!("intake=tray item={}", "x".repeat(129));
    let long_name = format!("intake={} item=T1", "a".repeat(33));
    let specs = [
        "intake=tray item=T1 harness=claude",
        "intake=tray item=T1 session=abc",
        "harness=claude session=abc ts=2026-09-03T00:00:00Z item=T1",
        "intake=tray",
        "item=T1",
        "intake=tray item=T1 bogus=1",
        "intake= item=T1",
        "intake=tray item=",
        "intake=tray item=T1 intake=inbox",
        "intake=tray item=T1 item=T2",
        "intake=Tray item=T1",
        "intake=1tray item=T1",
        "intake=tr_ay item=T1",
        &long_name,
        &too_long,
    ];
    for spec in specs {
        repo.arc(&repo.root)
            .args([
                "journal",
                "todo",
                "refused",
                "--title",
                "T",
                "--source",
                spec,
                "--item-key",
                "K",
            ])
            .assert()
            .failure();
    }
    assert!(journal_events(&repo).is_empty(), "a refused write recorded");
    assert!(todos(&repo).is_empty(), "a refused write filed");

    // source-attach shares the grammar and refuses before appending.
    let file = file_of(&file_as(&repo, "triage", "intake=tray item=T1", "T1"));
    let before = journal_events(&repo).len();
    for spec in specs {
        repo.arc(&repo.root)
            .args(["journal", "source-attach", &file, "--source", spec])
            .assert()
            .failure();
    }
    assert_eq!(
        journal_events(&repo).len(),
        before,
        "a refused attach wrote"
    );

    // The bound is inclusive.
    let longest = format!("intake=tray item={}", "x".repeat(128));
    let printed = file_as(&repo, "triage", &longest, "K");
    assert!(!printed.starts_with("existing:"), "{printed}");
}

#[test]
fn journal_source_answers_for_an_intake_item() {
    let repo = Repo::new();
    let file = file_of(&file_as(&repo, "triage", "intake=tray item=T1", "T1"));
    let other = file_of(&file_as(&repo, "triage", "intake=tray item=T2", "T2"));
    repo.arc(&repo.root)
        .args([
            "journal",
            "source-attach",
            &other,
            "--source",
            "intake=tray item=T1",
        ])
        .assert()
        .success()
        .stdout(format!("attached: {other}\n"));

    let report = json_stdout(repo.arc(&repo.root).args([
        "journal", "source", "--intake", "tray", "--item", "T1", "--json",
    ]));
    assert_eq!(report["schema"], "journal-source/2", "{report}");
    assert_eq!(report["intake"], "tray", "{report}");
    assert_eq!(report["item"], "T1", "{report}");
    assert!(report.get("harness").is_none(), "{report}");
    let items = report["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{report}");
    assert_eq!(items[0]["item_key"], "T1", "{report}");
    assert_eq!(items[0]["disposition"], "open", "{report}");
    assert_eq!(items[0]["file"], file.as_str(), "{report}");
    assert_eq!(items[0]["events"][0]["source"]["item"], "T1", "{report}");
    assert!(items[1].get("item_key").is_none(), "{report}");
    assert_eq!(items[1]["file"], other.as_str(), "{report}");
    assert_eq!(items[1]["events"][0]["event"], "source-attach", "{report}");

    let text = stdout(
        repo.arc(&repo.root)
            .args(["journal", "source", "--intake", "tray", "--item", "T1"]),
    );
    assert!(text.starts_with(&format!("T1 [open] {file}\n")), "{text}");
    assert!(text.contains(" note intake=tray item=T1\n"), "{text}");
    assert!(
        text.contains(&format!("(no item key) [open] {other}\n")),
        "{text}"
    );

    let none = stdout(
        repo.arc(&repo.root)
            .args(["journal", "source", "--intake", "tray", "--item", "T9"]),
    );
    assert_eq!(none, "no items recorded for intake=tray item=T9\n");

    // A session and an intake item are never the same origin.
    let session = json_stdout(repo.arc(&repo.root).args([
        "journal",
        "source",
        "--harness",
        "tray",
        "--session",
        "T1",
        "--json",
    ]));
    assert!(session["items"].as_array().unwrap().is_empty(), "{session}");
    for args in [
        &["--intake", "tray", "--item", "T1", "--harness", "tray"][..],
        &["--intake", "tray"][..],
    ] {
        repo.arc(&repo.root)
            .args(["journal", "source"])
            .args(args)
            .assert()
            .failure();
    }
    let filed =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "filed", "--session", "T1", "--json"]),
        );
    assert!(filed["sessions"].as_array().unwrap().is_empty(), "{filed}");
    let filed = json_stdout(repo.arc(&repo.root).args(["journal", "filed", "--json"]));
    assert_eq!(filed["sessions"][0]["session"], "triage", "{filed}");
    assert_eq!(
        filed["sessions"][0]["items"][0]["sources"][0],
        serde_json::json!({"intake": "tray", "item": "T1", "item_key": "T1"}),
        "{filed}"
    );
    let filed = stdout(repo.arc(&repo.root).args(["journal", "filed"]));
    assert!(
        filed.contains("    source: intake=tray item=T1 item-key=T1\n"),
        "{filed}"
    );
}
