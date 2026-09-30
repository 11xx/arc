use super::common::*;

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// A change with one claim on it, returned as the change id and the claim id.
fn claimed_change(repo: &Repo, slug: &str) -> (String, String) {
    let change_id = begin_no_worktree(repo, slug, &[]);
    repo.arc(&repo.root)
        .args(["claim", slug])
        .assert()
        .success();
    let claim = claim_ids(repo, &change_id)
        .pop()
        .expect("the claim is recorded");
    (change_id, claim)
}

fn claim_ids(repo: &Repo, change_id: &str) -> Vec<String> {
    events_of(repo, change_id, "claim-set")
        .into_iter()
        .map(|event| event["claim_id"].as_str().unwrap().to_string())
        .collect()
}

fn events_of(repo: &Repo, change_id: &str, event_type: &str) -> Vec<serde_json::Value> {
    let mut names: Vec<_> = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|path| serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

fn context(repo: &Repo, args: &[&str]) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.arg("context").args(args);
    command
}

fn read(repo: &Repo, change: &str, claim: &str, record: &str, extra: &[&str]) -> AssertCommand {
    let mut command = context(
        repo,
        &[
            "read",
            "--subject",
            change,
            "--episode",
            claim,
            "--record",
            record,
        ],
    );
    command.args(extra);
    command
}

fn explain_json(repo: &Repo, change: &str) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["explain", change, "--json"]))
}

fn explain_text(repo: &Repo, change: &str) -> String {
    stdout(repo.arc(&repo.root).args(["explain", change]))
}

#[test]
fn a_declaration_may_cite_a_recorded_read() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "cite");
    let seen = digest(b"hello\n");
    read(
        &repo,
        &change,
        &claim,
        "tool-1",
        &["--path", "README.md", "--digest", &seen, "--whole"],
    )
    .assert()
    .success()
    .stdout(predicates::str::contains("read: tool-1"));

    context(
        &repo,
        &[
            "declare",
            "--subject",
            &change,
            "--relies-on",
            "--path",
            "README.md",
            "--citation",
            "tool-1",
        ],
    )
    .assert()
    .success()
    .stdout(predicates::str::contains("relies-on README.md"));

    let before = event_count(&repo, &change);
    context(
        &repo,
        &[
            "declare",
            "--subject",
            &change,
            "--cites",
            "--path",
            "README.md",
            "--citation",
            "tool-unknown",
        ],
    )
    .assert()
    .failure()
    .stderr(predicates::str::contains("unknown-citation"));
    assert_eq!(
        event_count(&repo, &change),
        before,
        "a refusal writes nothing"
    );

    // The declaration is a claim beside the read; it never becomes one.
    let explained = explain_json(&repo, &change);
    let reads = explained["observed_reads"]["rows"].as_array().unwrap();
    assert_eq!(reads.len(), 1, "{explained}");
    let declared: Vec<_> = explained["declared_facts"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["item"] == "declaration")
        .collect();
    assert_eq!(declared.len(), 1, "{explained}");
    assert_eq!(declared[0]["standing"], "declared");
    assert_eq!(declared[0]["relation"], "relies-on");
    assert_eq!(declared[0]["citation"], "tool-1");
}

#[test]
fn tapes_events_become_read_records_with_skips() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "tapes");
    let path = repo.root.join("README.md");
    let path = path.to_str().unwrap();
    let event_id = |n: u8| format!("sha256:{}", format!("{n:02x}").repeat(32));
    let document = serde_json::json!({
        "schema": "tapes-events/9",
        "session": { "id": "session-x" },
        "events": [
            { "ordinal": 1, "event_id": event_id(1), "kind": "tool-call", "name": "Read",
              "read": { "path": path, "lines": { "start": 1, "end": 1 },
                        "sha256": digest(b"1\thello") },
              "pair": { "ordinal": 2 } },
            { "ordinal": 2, "event_id": event_id(2), "kind": "tool-result",
              "pair": { "ordinal": 1 } },
            { "ordinal": 3, "event_id": event_id(3), "kind": "tool-call", "name": "Bash",
              "read": { "path": path, "whole": true, "sha256": digest(b"hello\n") },
              "pair": { "ordinal": 4 } },
            { "ordinal": 4, "event_id": event_id(4), "kind": "tool-result", "status": "completed",
              "pair": { "ordinal": 3 } },
            { "ordinal": 5, "event_id": event_id(5), "kind": "tool-call", "name": "Read",
              "read": { "sha256": digest(b"elsewhere") },
              "pair": { "ordinal": 6 } },
            { "ordinal": 6, "event_id": event_id(6), "kind": "tool-result",
              "pair": { "ordinal": 5 } },
            { "ordinal": 7, "event_id": event_id(7), "kind": "tool-call", "name": "Read",
              "read": { "path": format!("{path}.missing"), "sha256": digest(b"no such file") },
              "pair": { "ordinal": 8 } },
            { "ordinal": 8, "event_id": event_id(8), "kind": "tool-result", "status": "error",
              "pair": { "ordinal": 7 } },
            { "ordinal": 9, "event_id": event_id(9), "kind": "tool-call", "name": "Bash" }
        ]
    });
    let file = repo.home.join("events.json");
    fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
    let from_tapes = |repo: &Repo| {
        stdout(&mut context(
            repo,
            &[
                "read",
                "--subject",
                &change,
                "--episode",
                &claim,
                "--from-tapes",
                file.to_str().unwrap(),
                "--at",
                "HEAD",
            ],
        ))
    };

    let out = from_tapes(&repo);
    let skips: Vec<_> = out
        .lines()
        .filter(|line| line.starts_with("skipped "))
        .collect();
    assert_eq!(skips.len(), 2, "{out}");
    assert!(
        skips.contains(&format!("skipped {}: no path", event_id(5)).as_str()),
        "{out}"
    );
    assert!(
        skips.contains(&format!("skipped {}: the call failed", event_id(7)).as_str()),
        "{out}"
    );
    assert!(out.contains("reads: 2 recorded, 2 skipped"), "{out}");

    let reads = events_of(&repo, &change, "context-read");
    assert_eq!(reads.len(), 2);
    let by_record = |id: String| {
        reads
            .iter()
            .find(|read| read["record"] == id.as_str())
            .unwrap_or_else(|| panic!("no read for {id}"))
            .clone()
    };
    let ranged = by_record(event_id(1));
    assert_eq!(
        ranged["coverage"],
        serde_json::json!({ "kind": "lines", "from": 1, "to": 1 })
    );
    // Line-numbered output digests text no file holds: nothing is inferred.
    assert!(ranged.get("blob").is_none(), "{ranged}");
    assert_eq!(ranged["source"], "tapes-events/9 session session-x");
    let whole = by_record(event_id(3));
    assert_eq!(whole["coverage"], serde_json::json!({ "kind": "whole" }));
    assert_eq!(whole["blob"]["path"], "README.md", "{whole}");
    assert_eq!(whole["blob"]["inference"], "content-matches-revision");

    // A record already held is skipped, not recorded twice.
    let again = from_tapes(&repo);
    assert!(again.contains("reads: 0 recorded, 4 skipped"), "{again}");
    assert!(
        again.contains(&format!("skipped {}: already recorded", event_id(1))),
        "{again}"
    );
    assert_eq!(events_of(&repo, &change, "context-read").len(), 2);
}

#[test]
fn a_tapes_document_of_another_shape_is_refused() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "shape");
    let file = repo.home.join("events.json");
    fs::write(&file, r#"{"schema":"tapes-events/8","events":[]}"#).unwrap();
    context(
        &repo,
        &[
            "read",
            "--subject",
            &change,
            "--episode",
            &claim,
            "--from-tapes",
            file.to_str().unwrap(),
        ],
    )
    .assert()
    .failure()
    .stderr(predicates::str::contains("unsupported-tapes-schema"));
}

#[test]
fn a_read_infers_its_blob_only_when_bytes_match() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "bytes");
    repo.commit(&repo.root, "notes.txt", "one\ntwo\nthree\n", "notes");
    fs::write(repo.root.join("notes.txt"), "one\nTWO\nthree\n").unwrap();
    let edited = digest(b"TWO\n");

    read(
        &repo,
        &change,
        &claim,
        "before-commit",
        &[
            "--path",
            "notes.txt",
            "--digest",
            &edited,
            "--lines",
            "2-2",
            "--at",
            "HEAD",
        ],
    )
    .assert()
    .success()
    .stdout(predicates::str::contains("blob: none"));

    git(&repo.root, &["commit", "-qam", "edit"]);
    let head = repo.head(&repo.root);
    let blob = git_out(&repo.root, &["rev-parse", "HEAD:notes.txt"]);
    read(
        &repo,
        &change,
        &claim,
        "after-commit",
        &[
            "--path",
            repo.root.join("notes.txt").to_str().unwrap(),
            "--digest",
            &edited,
            "--lines",
            "2-2",
            "--at",
            "HEAD",
        ],
    )
    .assert()
    .success();

    let explained = explain_json(&repo, &change);
    let rows = explained["observed_reads"]["rows"].as_array().unwrap();
    let row = |record: &str| {
        rows.iter()
            .find(|row| row["record"] == record)
            .unwrap_or_else(|| panic!("no row for {record}: {explained}"))
    };
    let before = row("before-commit");
    assert_eq!(before["standing"], "recorded");
    assert_eq!(before["digest"], edited.as_str());
    assert!(before.get("blob").is_none(), "{before}");
    assert!(before["compared_at"].is_string(), "{before}");
    let after = row("after-commit");
    assert_eq!(after["blob"]["standing"], "inferred", "{after}");
    assert_eq!(after["blob"]["reason"], "content-matches-revision");
    assert_eq!(after["blob"]["blob"], blob.as_str());
    assert_eq!(after["blob"]["revision"], head.as_str());
    assert_eq!(after["blob"]["path"], "notes.txt");

    // A path the revision does not hold records no blob and is no refusal.
    read(
        &repo,
        &change,
        &claim,
        "absent-path",
        &["--path", "absent.txt", "--digest", &edited, "--at", "HEAD"],
    )
    .assert()
    .success()
    .stdout(predicates::str::contains("blob: none"));
}

#[test]
fn explain_shows_a_read_at_risk_until_pinned() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "risk");
    read(
        &repo,
        &change,
        &claim,
        "tool-1",
        &["--path", "README.md", "--digest", &digest(b"hello\n")],
    )
    .assert()
    .success();

    let text = explain_text(&repo, &change);
    assert!(text.contains("at risk"), "{text}");
    assert!(text.contains("(unknown)"), "{text}");
    let explained = explain_json(&repo, &change);
    assert_eq!(explained["schema"], "arc-explain/1");
    assert_eq!(
        explained["observed_reads"]["rows"][0]["capture"]["at_risk"],
        true
    );

    context(&repo, &["capture", "--record", "tool-1", "--unpinned"])
        .assert()
        .success();
    assert!(explain_text(&repo, &change).contains("at risk: recording reported unpinned"));

    context(&repo, &["capture", "--record", "tool-1", "--pinned"])
        .assert()
        .success();
    let text = explain_text(&repo, &change);
    assert!(!text.contains("at risk"), "{text}");
    assert!(
        text.contains("recording pinned, reported by tester"),
        "{text}"
    );
    let explained = explain_json(&repo, &change);
    let capture = &explained["observed_reads"]["rows"][0]["capture"];
    assert_eq!(capture["at_risk"], false, "{explained}");
    assert_eq!(capture["capture"], "pinned");

    context(&repo, &["capture", "--record", "tool-unknown", "--pinned"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown-record"));
}

#[test]
fn a_duplicate_record_id_is_refused() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "dup");
    let args = ["--path", "README.md", "--digest", &digest(b"hello\n")];
    read(&repo, &change, &claim, "tool-1", &args)
        .assert()
        .success();
    let before = event_count(&repo, &change);
    read(&repo, &change, &claim, "tool-1", &args)
        .assert()
        .failure()
        .stderr(predicates::str::contains("duplicate-record"));
    assert_eq!(
        event_count(&repo, &change),
        before,
        "a refusal writes nothing"
    );
}

#[test]
fn a_read_outside_the_subjects_episodes_is_refused() {
    let repo = Repo::new();
    let (change, _) = claimed_change(&repo, "mine");
    let (_, foreign) = claimed_change(&repo, "theirs");
    let before = event_count(&repo, &change);
    read(
        &repo,
        &change,
        &foreign,
        "tool-1",
        &["--path", "README.md", "--digest", &digest(b"hello\n")],
    )
    .assert()
    .failure()
    .stderr(predicates::str::contains("unknown-episode"));
    context(
        &repo,
        &[
            "read",
            "--subject",
            "nothing-here",
            "--episode",
            &foreign,
            "--record",
            "tool-1",
            "--path",
            "README.md",
            "--digest",
            &digest(b"hello\n"),
        ],
    )
    .assert()
    .failure()
    .stderr(predicates::str::contains("unknown-subject"));
    assert_eq!(event_count(&repo, &change), before);
}

#[test]
fn a_journal_read_names_its_artifact_and_compares_the_body() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "journal");
    let body = "The plan.\n";
    let (dir, file) = journal_artifact(&repo, "relations", "plan", body);
    let stored = fs::read(dir.join(&file)).unwrap();
    let path = dir.join(&file);
    let path = path.to_str().unwrap();
    read(
        &repo,
        &change,
        &claim,
        "whole-plan",
        &["--path", path, "--digest", &digest(&stored), "--whole"],
    )
    .assert()
    .success()
    .stdout(predicates::str::contains(format!(
        "artifact: {file} (body matches)"
    )));
    read(
        &repo,
        &change,
        &claim,
        "one-line",
        &["--path", path, "--digest", &digest(b"x"), "--lines", "1-1"],
    )
    .assert()
    .success()
    .stdout(predicates::str::contains(format!(
        "artifact: {file} (body differs)"
    )));
}

#[test]
fn a_candidates_relations_are_repository_events_its_show_lists() {
    let repo = Repo::new();
    let (change, claim) = claimed_change(&repo, "answer");
    repo.arc(&repo.root)
        .args(["brief", "answer", "--body-file", "-"])
        .write_stdin("Answer the question.\n")
        .assert()
        .success();
    let head = repo.head(&repo.root);
    let registered = stdout(repo.arc(&repo.root).args([
        "candidate",
        "register",
        "--tree",
        &head,
        "--brief",
        &change,
        "--producer",
        "alice",
        "--id",
        "answer-one",
    ]));
    assert!(registered.contains("candidate: answer-one"), "{registered}");
    let before = event_count(&repo, &change);

    read(
        &repo,
        "answer-one",
        &claim,
        "tool-1",
        &[
            "--path",
            "README.md",
            "--digest",
            &digest(b"hello\n"),
            "--whole",
            "--at",
            "HEAD",
        ],
    )
    .assert()
    .success();
    context(
        &repo,
        &[
            "declare",
            "--subject",
            "answer-one",
            "--considers",
            "--path",
            "README.md",
            "--citation",
            "tool-1",
        ],
    )
    .assert()
    .success();
    assert_eq!(
        event_count(&repo, &change),
        before,
        "a candidate's relations stay off the change's ledger"
    );

    let shown =
        json_stdout(
            repo.arc(&repo.root)
                .args(["candidate", "show", "answer-one", "--json"]),
        );
    assert_eq!(shown["schema"], "arc-candidate/1");
    let relations = &shown["candidates"][0]["relations"];
    assert_eq!(relations["reads"][0]["record"], "tool-1", "{shown}");
    assert_eq!(
        relations["reads"][0]["blob"]["inference"],
        "content-matches-revision"
    );
    assert_eq!(relations["reads"][0]["at_risk"], true);
    assert_eq!(relations["declarations"][0]["relation"], "considers");
    assert_eq!(relations["declarations"][0]["citation"], "tool-1");

    // One record on two subjects needs the subject named for a report.
    read(
        &repo,
        &change,
        &claim,
        "tool-1",
        &["--path", "README.md", "--digest", &digest(b"hello\n")],
    )
    .assert()
    .success();
    context(&repo, &["capture", "--record", "tool-1", "--pinned"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ambiguous-record"));
    context(
        &repo,
        &[
            "capture",
            "--record",
            "tool-1",
            "--subject",
            "answer-one",
            "--pinned",
        ],
    )
    .assert()
    .success();
    let shown =
        json_stdout(
            repo.arc(&repo.root)
                .args(["candidate", "show", "answer-one", "--json"]),
        );
    assert_eq!(
        shown["candidates"][0]["relations"]["reads"][0]["at_risk"],
        false
    );
}

#[test]
fn a_read_in_another_projects_journal_names_the_qualified_artifact() {
    let repo = Repo::new();
    let other = Repo::new();
    let (change, claim) = claimed_change(&repo, "foreign");
    let (dir, file) = journal_artifact(&other, "elsewhere", "plan", "Their plan.\n");
    let stored = fs::read(dir.join(&file)).unwrap();
    let path = dir.join(&file);
    read(
        &repo,
        &change,
        &claim,
        "foreign-plan",
        &[
            "--path",
            path.to_str().unwrap(),
            "--digest",
            &digest(&stored),
            "--whole",
        ],
    )
    .assert()
    .success();

    let qualified = format!("{}::{file}", fs::canonicalize(&dir).unwrap().display());
    let reads = events_of(&repo, &change, "context-read");
    assert_eq!(
        reads[0]["artifact"]["file"],
        qualified.as_str(),
        "{}",
        reads[0]
    );
    assert_eq!(
        reads[0]["artifact"]["body_digest"],
        digest(&stored).as_str()
    );

    // The qualified reference a later requirement names is the one declared.
    context(
        &repo,
        &[
            "declare",
            "--subject",
            &change,
            "--relies-on",
            "--artifact",
            &qualified,
            "--citation",
            "foreign-plan",
        ],
    )
    .assert()
    .success();
}
