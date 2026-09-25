use super::common::*;

/// Two independent stores, paired in one logical project.
fn pair(source: &Repo, recipient: &Repo, source_name: &str, recipient_name: &str) {
    let recipient_id = repository_id(recipient);
    source
        .arc(&source.root)
        .args(["replica", "init", source_name])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args([
            "replica",
            "pair",
            recipient_name,
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
}

fn repository_id(repo: &Repo) -> String {
    let identity = json_stdout(repo.arc(&repo.root).args(["replica", "id", "--json"]));
    identity["repository_id"].as_str().unwrap().to_string()
}

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

fn journal_events_dir(dir: &Path) -> String {
    fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default()
}

fn events_for(dir: &Path, file: &str) -> Vec<serde_json::Value> {
    journal_events_dir(dir)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["file"] == file)
        .collect()
}

fn bundle_file(repo: &Repo, name: &str) -> PathBuf {
    repo.home.join(format!("{name}.json"))
}

fn export(repo: &Repo, files: &[&str], path: &Path) -> String {
    let mut cmd = repo.arc(&repo.root);
    cmd.args(["journal", "export"]);
    cmd.args(files);
    cmd.args(["--output", path.to_str().unwrap()]);
    stdout(&mut cmd)
}

fn import(repo: &Repo, path: &Path, dry_run: bool) -> std::process::Output {
    let mut cmd = repo.arc(&repo.root);
    cmd.args(["journal", "import", path.to_str().unwrap()]);
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.output().unwrap()
}

fn receipts(dir: &Path) -> usize {
    fs::read_dir(dir.join("exchange/imports"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// An artifact written through the CLI, named by the file the command reports.
fn write_artifact(repo: &Repo, topic: &str, kind: &str, body: &str) -> String {
    let out = stdout(
        repo.arc(&repo.root)
            .args(["journal", "note", topic, "--kind", kind, "--body-file", "-"])
            .write_stdin(body),
    );
    let path = out.lines().last().unwrap().trim();
    Path::new(path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

/// A discussion argued and settled the way `journal` records each step.
fn discussion_with_records(repo: &Repo) -> String {
    let file = write_artifact(
        repo,
        "exchange-probe",
        "discussion",
        "# Exchange probe\n\nA discussion about moving artifacts.\n",
    );
    repo.arc(&repo.root)
        .args(["journal", "position", &file, "--body-file", "-"])
        .write_stdin("Position: for\n\nMove it.\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "journal",
            "question",
            &file,
            "--placement",
            "closing",
            "--option",
            "yes",
            "--option",
            "no",
            "--body-file",
            "-",
        ])
        .write_stdin("Should the exchange be selective?\n")
        .assert()
        .success();
    let questions = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "questions", "--json"]),
    );
    let question = questions["questions"][0]["question"]
        .as_str()
        .unwrap()
        .to_string();
    repo.arc(&repo.root)
        .args([
            "journal",
            "answer",
            &file,
            "--question",
            &question,
            "--option",
            "yes",
            "--body-file",
            "-",
        ])
        .write_stdin("Yes, selective.\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["claim", &file, "--ttl", "30m"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "checkpoint", &file, "--body-file", "-"])
        .write_stdin("Where the work stands.\n")
        .assert()
        .success();
    file
}

#[test]
fn a_discussion_round_trips_with_its_bodies_events_and_provenance() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let file = discussion_with_records(&source);
    let source_dir = journal_dir(&source);
    let recipient_dir = journal_dir(&recipient);

    let bundle = bundle_file(&source, "journal-bundle");
    let reported = export(&source, &[&file], &bundle);
    assert!(
        reported.contains("source replica: workstation"),
        "{reported}"
    );
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(value["schema"], "arc-journal-bundle/1", "{value}");
    assert_eq!(value["source_replica"]["name"], "workstation");
    assert_eq!(value["artifacts"].as_array().unwrap().len(), 1);
    let artifact = &value["artifacts"][0];
    assert_eq!(artifact["file"], file.as_str());
    assert_eq!(artifact["storage"], "hot");
    assert!(artifact["digest"].as_str().unwrap().starts_with("sha256:"));
    let kinds = artifact["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "note",
            "position",
            "question",
            "answer",
            "claim-set",
            "checkpoint"
        ],
        "{value}"
    );
    assert!(
        !serde_json::to_string(&value)
            .unwrap()
            .contains(source.root.to_str().unwrap()),
        "a journal bundle carries no journal directory path: {value}"
    );

    let preview = import(&recipient, &bundle, true);
    assert!(preview.status.success(), "{preview:?}");
    let preview = String::from_utf8_lossy(&preview.stdout);
    assert!(
        preview.contains("dry-run: would import 1 artifact(s) and 6 event(s)"),
        "{preview}"
    );
    assert!(
        preview.contains("receipt: would record source replica and bundle digest"),
        "{preview}"
    );
    assert!(!recipient_dir.join(&file).exists());
    assert!(!recipient_dir.join("events.jsonl").exists());
    assert_eq!(receipts(&recipient_dir), 0);

    let imported = import(&recipient, &bundle, false);
    assert!(imported.status.success(), "{imported:?}");
    assert_eq!(
        fs::read(recipient_dir.join(&file)).unwrap(),
        fs::read(source_dir.join(&file)).unwrap()
    );
    let recorded = events_for(&recipient_dir, &file);
    assert_eq!(recorded.len(), 6, "{recorded:?}");
    assert_eq!(recorded, events_for(&source_dir, &file));
    for event in &recorded {
        assert_eq!(event["harness"], "test", "{event}");
        assert_eq!(event["session"], "session-a", "{event}");
        assert_eq!(event["actor"], "tester", "{event}");
    }
    let source_view =
        json_stdout(
            source
                .arc(&source.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    let recipient_view = json_stdout(recipient.arc(&recipient.root).args([
        "journal",
        "discussion",
        &file,
        "--json",
    ]));
    assert_eq!(recipient_view, source_view, "{recipient_view}");

    assert_eq!(receipts(&recipient_dir), 1);
    let receipt_path = fs::read_dir(recipient_dir.join("exchange/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["schema"], "arc-journal-exchange-import/1");
    assert_eq!(receipt["source_replica"]["name"], "workstation");
    assert_eq!(receipt["imported_by"]["name"], "agent-host");
    assert_eq!(receipt["artifacts"][0]["file"], file.as_str());

    recipient
        .arc(&recipient.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));

    // Back the other way: the receiving replica exports what it holds, and the
    // originating replica already has every byte of it.
    let back = bundle_file(&recipient, "journal-bundle-back");
    export(&recipient, &[&file], &back);
    let before = journal_events_dir(&source_dir);
    let returned = import(&source, &back, false);
    assert!(returned.status.success(), "{returned:?}");
    assert!(String::from_utf8_lossy(&returned.stdout).contains("events: 0 imported"));
    assert_eq!(journal_events_dir(&source_dir), before);
}

#[test]
fn importing_the_same_bundle_twice_changes_nothing() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let file = write_artifact(&source, "idempotence", "todo", "# Idempotence\n");
    let bundle = bundle_file(&source, "idempotence");
    export(&source, &[&file], &bundle);
    import(&recipient, &bundle, false);
    let recipient_dir = journal_dir(&recipient);
    let after_first = journal_events_dir(&recipient_dir);

    let second = import(&recipient, &bundle, false);
    assert!(second.status.success(), "{second:?}");
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("0 imported (receipt already recorded)"),
        "{second:?}"
    );
    assert_eq!(journal_events_dir(&recipient_dir), after_first);
    assert_eq!(receipts(&recipient_dir), 1);
}

#[test]
fn a_divergent_local_revision_refuses_the_whole_import() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let divergent = write_artifact(
        &source,
        "divergent",
        "discussion",
        "# Divergent\n\nFirst.\n",
    );
    let bundle = bundle_file(&source, "divergent-first");
    export(&source, &[&divergent], &bundle);
    import(&recipient, &bundle, false);
    let recipient_dir = journal_dir(&recipient);
    let recipient_body = fs::read(recipient_dir.join(&divergent)).unwrap();
    let recipient_events = journal_events_dir(&recipient_dir);

    // The origin extends the artifact and also sends a second, new one. The
    // conflict refuses both: nothing partial lands.
    source
        .arc(&source.root)
        .args(["journal", "position", &divergent, "--body-file", "-"])
        .write_stdin("Position: against\n\nChanged my mind.\n")
        .assert()
        .success();
    let fresh = write_artifact(&source, "divergent-fresh", "todo", "# Fresh\n");
    let bundle = bundle_file(&source, "divergent-second");
    export(&source, &[&divergent, &fresh], &bundle);

    let refused = import(&recipient, &bundle, false);
    assert!(!refused.status.success(), "{refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains(&divergent), "{stderr}");
    assert!(stderr.contains("present locally with digest"), "{stderr}");
    assert_eq!(
        fs::read(recipient_dir.join(&divergent)).unwrap(),
        recipient_body
    );
    assert!(!recipient_dir.join(&fresh).exists());
    assert_eq!(journal_events_dir(&recipient_dir), recipient_events);
    assert_eq!(receipts(&recipient_dir), 1);
}

#[test]
fn a_referenced_artifact_travels_in_the_closure_or_the_export_refuses() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let decision = write_artifact(&source, "closure-decision", "decision", "# Decided\n");
    let discussion = write_artifact(&source, "closure-discussion", "discussion", "# Argued\n");
    source
        .arc(&source.root)
        .args([
            "journal",
            "consume",
            &discussion,
            "--outcome",
            "done",
            "--decision",
            &decision,
        ])
        .assert()
        .success();

    let bundle = bundle_file(&source, "closure");
    export(&source, &[&discussion], &bundle);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let files = value["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| artifact["file"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(files.contains(&decision), "{value}");
    assert!(files.contains(&discussion), "{value}");
    import(&recipient, &bundle, false);
    assert!(journal_dir(&recipient).join(&decision).is_file());

    // A reference whose artifact is gone refuses the export by name, and
    // writes no file.
    let source_dir = journal_dir(&source);
    fs::remove_file(source_dir.join(&decision)).unwrap();
    let missing = bundle_file(&source, "closure-missing");
    source
        .arc(&source.root)
        .args([
            "journal",
            "export",
            &discussion,
            "--output",
            missing.to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(decision.as_str()))
        .stderr(predicates::str::contains("export nothing"));
    assert!(!missing.exists());
}

#[test]
fn an_archived_artifact_lands_in_the_receiving_cold_archive() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let file = write_artifact(&source, "cold-travel", "discussion", "# Shelved\n");
    source
        .arc(&source.root)
        .args([
            "journal",
            "archive",
            &file,
            "--unresolved",
            "--note",
            "waiting",
        ])
        .assert()
        .success();
    let bundle = bundle_file(&source, "cold-travel");
    export(&source, &[&file], &bundle);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(value["artifacts"][0]["storage"], "cold", "{value}");

    import(&recipient, &bundle, false);
    let recipient_dir = journal_dir(&recipient);
    let cold = recipient_dir.with_file_name(format!(
        "{}-archive",
        recipient_dir.file_name().unwrap().to_str().unwrap()
    ));
    assert!(cold.join(&file).is_file());
    assert!(!recipient_dir.join(&file).exists());
    let shown = stdout(
        recipient
            .arc(&recipient.root)
            .args(["journal", "show", &file]),
    );
    assert!(shown.contains("# Shelved"), "{shown}");
    recipient
        .arc(&recipient.root)
        .args(["journal", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));
}

#[test]
fn journal_exchange_requires_replicas_paired_with_each_other() {
    let source = Repo::new();
    let recipient = Repo::new();
    let file = write_artifact(&source, "unpaired", "todo", "# Unpaired\n");
    let bundle = bundle_file(&source, "unpaired");
    source
        .arc(&source.root)
        .args([
            "journal",
            "export",
            &file,
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("replica init"));
    assert!(!bundle.exists());

    pair(&source, &recipient, "workstation", "agent-host");
    export(&source, &[&file], &bundle);

    // A store that recorded no replica of its own cannot adopt artifacts from
    // an unrelated pair's bundle.
    let stranger = Repo::new();
    let stranger_bundle = bundle_file(&source, "stranger");
    fs::copy(&bundle, &stranger_bundle).unwrap();
    stranger
        .arc(&stranger.root)
        .args(["journal", "import", stranger_bundle.to_str().unwrap()])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("replica init"));

    // A bundle whose source is not a paired replica of the receiver refuses,
    // even when the artifacts themselves are intact.
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    value["source_replica"]["name"] = serde_json::json!("stranger");
    value["source_replica"]["repository_id"] = serde_json::json!("01STRANGER0000000000000000");
    fs::write(&bundle, json_file_bytes(&value)).unwrap();
    recipient
        .arc(&recipient.root)
        .args(["journal", "import", bundle.to_str().unwrap()])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("not paired"));
}

#[test]
fn a_bundle_from_another_logical_project_is_refused() {
    let source = Repo::new();
    let peer = Repo::new();
    let other_source = Repo::new();
    let other_peer = Repo::new();
    pair(&source, &peer, "workstation", "agent-host");
    pair(&other_source, &other_peer, "second", "second-peer");
    let file = write_artifact(&source, "project-scope", "todo", "# Scoped\n");
    let bundle = bundle_file(&source, "project-scope");
    export(&source, &[&file], &bundle);

    other_peer
        .arc(&other_peer.root)
        .args(["journal", "import", bundle.to_str().unwrap()])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("belongs to logical project"));
    assert!(!journal_dir(&other_peer).join(&file).exists());
}

#[test]
fn a_live_remote_claim_contested_locally_refuses_the_import() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let file = write_artifact(&source, "claim-contest", "discussion", "# Contested\n");
    let bundle = bundle_file(&source, "claim-contest");
    export(&source, &[&file], &bundle);
    import(&recipient, &bundle, false);
    let recipient_dir = journal_dir(&recipient);

    // Both replicas hold the artifact and both have a live claim on it. The
    // origin's claim arrives on an unchanged body, so it is a claim bringing
    // new events, not a revision conflict.
    source
        .arc(&source.root)
        .args(["claim", &file, "--ttl", "30m"])
        .assert()
        .success();
    recipient
        .arc(&recipient.root)
        .args(["claim", &file, "--ttl", "30m"])
        .assert()
        .success();
    export(&source, &[&file], &bundle);
    let before = journal_events_dir(&recipient_dir);
    let refused = import(&recipient, &bundle, true);
    assert!(!refused.status.success(), "{refused:?}");
    let text = String::from_utf8_lossy(&refused.stdout);
    assert!(text.contains("claim contest on artifact"), "{text}");
    assert!(text.contains("workstation"), "{text}");
    assert!(text.contains("agent-host"), "{text}");
    assert_eq!(journal_events_dir(&recipient_dir), before);
    assert_eq!(receipts(&recipient_dir), 1);
}

#[test]
fn exchange_takes_artifact_names_not_paths() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "workstation", "agent-host");
    let file = write_artifact(&source, "name-only", "todo", "# Name only\n");
    let bundle = bundle_file(&source, "name-only");
    source
        .arc(&source.root)
        .args([
            "journal",
            "export",
            "../escape.md",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .failure();
    export(&source, &[&file], &bundle);
    recipient
        .arc(&recipient.root)
        .args(["journal", "import", bundle.to_str().unwrap()])
        .assert()
        .success();
}
