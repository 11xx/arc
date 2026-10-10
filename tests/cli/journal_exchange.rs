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
    assert_eq!(value["schema"], "arc-journal-bundle/2", "{value}");
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
    let mut source_view =
        json_stdout(
            source
                .arc(&source.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    let mut recipient_view = json_stdout(recipient.arc(&recipient.root).args([
        "journal",
        "discussion",
        &file,
        "--json",
    ]));
    // `age_seconds` is read from the clock, so two reads may straddle a second
    // boundary. Every other field is derived from the exchanged records.
    for view in [&mut source_view, &mut recipient_view] {
        let age = view.as_object_mut().unwrap().remove("age_seconds");
        assert!(age.is_some_and(|age| age.is_u64()), "{view}");
    }
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

fn rewrite_bundle_checksum(bundle: &mut serde_json::Value) {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for artifact in bundle["artifacts"].as_array().unwrap() {
        #[derive(serde::Serialize)]
        struct Artifact<'a> {
            file: &'a serde_json::Value,
            storage: &'a serde_json::Value,
            body: &'a serde_json::Value,
            digest: &'a serde_json::Value,
            events: &'a serde_json::Value,
        }
        let artifact = Artifact {
            file: &artifact["file"],
            storage: &artifact["storage"],
            body: &artifact["body"],
            digest: &artifact["digest"],
            events: &artifact["events"],
        };
        digest.update(serde_json::to_vec(&artifact).unwrap());
        digest.update(b"\n");
    }
    bundle["artifacts_sha256"] = hex::encode(digest.finalize()).into();
}

#[test]
fn a_mixed_bundle_admits_known_artifacts_and_retries_held_back_artifacts() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "origin", "peer");
    let admitted = write_artifact(&source, "admitted", "todo", "# Work\n");
    let held = write_artifact(&source, "held", "decision", "# Future decision\n");
    source
        .arc(&source.root)
        .args(["journal", "evidence", &held, "--body-file", "-"])
        .write_stdin("Future observation.\n")
        .assert()
        .success();
    let dependent = write_artifact(&source, "dependent", "discussion", "# Related work\n");
    source
        .arc(&source.root)
        .args([
            "journal",
            "consume",
            &dependent,
            "--outcome",
            "done",
            "--decision",
            &held,
        ])
        .assert()
        .success();
    let path = bundle_file(&source, "mixed");
    export(&source, &[&admitted, &dependent], &path);
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let artifact = bundle["artifacts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|artifact| artifact["file"] == held)
        .unwrap();
    let event = artifact["events"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap();
    event["event"] = "future-observation".into();
    event["schema"] = "journal-events/2".into();
    event["future_payload"] = serde_json::json!({"observations": ["one", "two"]});
    let mut future = event.clone();
    future["schema"] = "journal-events/99".into();
    artifact["events"].as_array_mut().unwrap().push(future);
    rewrite_bundle_checksum(&mut bundle);
    fs::write(&path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    let dir = journal_dir(&recipient);
    for dry_run in [true, false, false] {
        let result = import(&recipient, &path, dry_run);
        assert!(result.status.success(), "{result:?}");
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(output.contains(&format!("held back: {held}")), "{output}");
        assert!(
            output.contains("future-observation") && output.contains("journal-events/99"),
            "{output}"
        );
        assert!(!output.contains("receipt already recorded"), "{output}");
        for file in [&held, &dependent] {
            assert!(output.contains(&format!("held back: {file}")), "{output}");
            assert!(!dir.join(file).exists());
            let cold = dir.with_file_name(format!(
                "{}-archive",
                dir.file_name().unwrap().to_str().unwrap()
            ));
            assert!(!cold.join(file).exists());
            assert!(events_for(&dir, file).is_empty());
        }
        if dry_run {
            assert!(!dir.join(&admitted).exists());
            assert_eq!(receipts(&dir), 0);
        } else {
            assert!(dir.join(&admitted).exists());
            assert_eq!(events_for(&dir, &admitted).len(), 1);
            assert_eq!(receipts(&dir), 1);
            let receipt = fs::read_dir(dir.join("exchange/imports"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let receipt: serde_json::Value =
                serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
            assert_eq!(receipt["artifacts"].as_array().unwrap().len(), 1);
            assert_eq!(receipt["artifacts"][0]["file"], admitted);
        }
    }
    let before = journal_events_dir(&dir);
    let artifact = bundle["artifacts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|artifact| artifact["file"] == admitted)
        .unwrap();
    artifact["events"][0]["event"] = "future-observation".into();
    rewrite_bundle_checksum(&mut bundle);
    fs::write(&path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    let result = import(&recipient, &path, false);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(journal_events_dir(&dir), before);
    assert_eq!(receipts(&dir), 1);
    assert!(!String::from_utf8_lossy(&result.stdout).contains("receipt: recorded"));
}

#[test]
fn partial_retries_deduplicate_a_delivery_rejected_after_a_local_answer() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "origin", "peer");
    let file = write_artifact(
        &source,
        "shared-question",
        "discussion",
        "# Shared question\n",
    );
    source
        .arc(&source.root)
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
        .write_stdin("Proceed?\n")
        .assert()
        .success();
    let question = events_for(&journal_dir(&source), &file)
        .into_iter()
        .find(|event| event["event"] == "question")
        .unwrap()["question_id"]
        .as_str()
        .unwrap()
        .to_string();
    let initial = bundle_file(&source, "initial-question");
    export(&source, &[&file], &initial);
    assert!(import(&recipient, &initial, false).status.success());
    recipient
        .arc(&recipient.root)
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
        .write_stdin("Proceed.\n")
        .assert()
        .success();
    source
        .arc(&source.root)
        .args([
            "journal",
            "delivered",
            &file,
            "--question",
            &question,
            "--to",
            "person",
        ])
        .assert()
        .success();
    let held = write_artifact(&source, "future-artifact", "todo", "# Future artifact\n");
    let path = bundle_file(&source, "partial-question");
    export(&source, &[&file, &held], &path);
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let artifact = bundle["artifacts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|artifact| artifact["file"] == held)
        .unwrap();
    artifact["events"][0]["event"] = "future-observation".into();
    artifact["events"][0]["schema"] = "journal-events/99".into();
    rewrite_bundle_checksum(&mut bundle);
    fs::write(&path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    let first = import(&recipient, &path, false);
    assert!(first.status.success(), "{first:?}");
    assert!(
        String::from_utf8_lossy(&first.stdout).contains("events: 1 imported"),
        "{first:?}"
    );
    let dir = journal_dir(&recipient);
    let before = journal_events_dir(&dir);
    assert_eq!(
        events_for(&dir, &file)
            .iter()
            .filter(|event| event["event"] == "question-delivered")
            .count(),
        1
    );
    // The physical delivery survives even though question replay rejects it.
    let questions = json_stdout(recipient.arc(&recipient.root).args([
        "journal",
        "discussion",
        &file,
        "--json",
    ]));
    assert_eq!(questions["questions"][0]["delivery"], "answered");
    assert!(questions["questions"][0]["deliveries"]
        .as_array()
        .unwrap()
        .is_empty());
    let receipt = fs::read_dir(dir.join("exchange/imports"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["artifacts"][0]["events"] == 1
        })
        .unwrap();
    let receipt_before: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    for dry_run in [true, false, false] {
        let retry = import(&recipient, &path, dry_run);
        assert!(retry.status.success(), "{retry:?}");
        let output = String::from_utf8_lossy(&retry.stdout);
        assert!(output.contains(&format!("held back: {held}")), "{output}");
        let expected = if dry_run {
            "and 0 event(s)"
        } else {
            "events: 0 imported"
        };
        assert!(output.contains(expected), "{output}");
        assert_eq!(journal_events_dir(&dir), before);
        let receipt_after: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        assert_eq!(receipt_after["artifacts"], receipt_before["artifacts"]);
        assert!(!dir.join(&held).exists());
    }
}

#[test]
fn journal_bundle_guide_and_help_describe_artifact_admission() {
    let repo = Repo::new();
    let normalize = |text: String| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let guide = normalize(stdout(&mut repo.arc(&repo.root)));
    let schemas = guide.split("SCHEMAS").last().unwrap();
    assert!(!schemas.contains("an import refuses a bundle carrying one"));
    assert!(schemas.contains("journal import holds back each artifact carrying an unsupported event and its dependents"), "{schemas}");
    let help = normalize(stdout(
        repo.arc(&repo.root).args(["journal", "import", "--help"]),
    ));
    assert!(help.contains("Artifacts carrying unsupported events are held back as a unit"));
    assert!(help.contains("their dependent artifacts are held back too"));
    for text in [&guide, &help] {
        assert!(
            text.contains(
                "Deduplication includes understood physical log records rejected by state replay"
            ),
            "{text}"
        );
    }
}

#[test]
fn a_partial_receipt_allows_the_remaining_artifact_to_be_admitted() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "origin", "peer");
    let first = write_artifact(&source, "first", "todo", "# First\n");
    let remaining = write_artifact(&source, "remaining", "todo", "# Remaining\n");
    let path = bundle_file(&source, "retry");
    export(&source, &[&first, &remaining], &path);
    assert!(import(&recipient, &path, false).status.success());
    let dir = journal_dir(&recipient);
    let receipt = fs::read_dir(dir.join("exchange/imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    value["artifacts"]
        .as_array_mut()
        .unwrap()
        .retain(|artifact| artifact["file"] == first);
    fs::write(&receipt, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::remove_file(dir.join(&remaining)).unwrap();
    let retained = journal_events_dir(&dir)
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["file"] != remaining
        })
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(dir.join("events.jsonl"), retained).unwrap();

    let result = import(&recipient, &path, false);
    assert!(result.status.success(), "{result:?}");
    assert!(dir.join(&remaining).exists(), "{result:?}");
    assert_eq!(events_for(&dir, &first).len(), 1);
    assert_eq!(events_for(&dir, &remaining).len(), 1);
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert_eq!(receipt["artifacts"].as_array().unwrap().len(), 2);
    assert!(receipt["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|artifact| artifact["events"] == 1));
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

    // An edit conflicts with the held base and refuses every artifact.
    fs::write(
        journal_dir(&source).join(&divergent),
        "# Divergent\n\nEdited.\n",
    )
    .unwrap();
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
    assert!(stderr.contains("first differing block: base"), "{stderr}");
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
    rewrite_bundle_checksum(&mut value);
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

#[test]
fn previous_journal_bundles_import_without_model_provenance() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "origin", "peer");
    let file = write_artifact(&source, "legacy-journal", "note", "# Legacy journal\n");
    let path = bundle_file(&source, "legacy-journal");
    export(&source, &[&file], &path);
    let mut bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    bundle["schema"] = serde_json::json!("arc-journal-bundle/1");
    assert!(bundle["artifacts"][0]["events"][0]
        .get("model_source")
        .is_none());
    rewrite_bundle_checksum(&mut bundle);
    fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    let result = import(&recipient, &path, false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let events = events_for(&journal_dir(&recipient), &file);
    assert!(events
        .iter()
        .all(|event| event.get("model_source").is_none()));
    let current = bundle_file(&recipient, "current-journal");
    export(&recipient, &[&file], &current);
    let bundle: serde_json::Value = serde_json::from_slice(&fs::read(current).unwrap()).unwrap();
    assert_eq!(bundle["schema"], "arc-journal-bundle/2");
}

/// An event without a harness or session travels in a journal bundle and
/// arrives without them.
#[test]
fn a_journal_bundle_carries_events_without_harness_or_session() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "origin", "peer");
    let out = stdout(
        source
            .arc(&source.root)
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .args([
                "journal",
                "note",
                "carried",
                "--kind",
                "note",
                "--body-file",
                "-",
            ])
            .write_stdin("# Carried\n"),
    );
    let file = Path::new(out.lines().last().unwrap().trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let path = bundle_file(&source, "carried");
    export(&source, &[&file], &path);
    let bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(bundle["schema"], "arc-journal-bundle/2", "{bundle}");
    let carried = &bundle["artifacts"][0]["events"][0];
    assert_eq!(carried["schema"], "journal-events/2", "{carried}");
    for key in ["harness", "session"] {
        assert!(carried.get(key).is_none(), "{key}: {carried}");
    }

    let result = import(&recipient, &path, false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let arrived = events_for(&journal_dir(&recipient), &file);
    assert_eq!(arrived.len(), 1, "{arrived:?}");
    for key in ["harness", "session"] {
        assert!(arrived[0].get(key).is_none(), "{key}: {}", arrived[0]);
    }
}

/// A bundle verifies against the events it carries as arc serializes them, so
/// an event recorded under the previous version travels exactly as recorded:
/// the bundle of an older arc and the bundle of this one agree on its bytes.
#[test]
fn a_journal_bundle_relays_a_version_one_event_as_recorded() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "origin", "peer");
    let file = write_artifact(&source, "relayed", "note", "# Relayed\n");
    let events = journal_dir(&source).join("events.jsonl");
    let rewritten: Vec<String> = journal_events_dir(&journal_dir(&source))
        .lines()
        .map(|line| {
            let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
            if event["file"] == file.as_str() {
                event["schema"] = "journal-events/1".into();
                event["harness"] = "unknown".into();
                event["session"] = "unknown".into();
            }
            event.to_string()
        })
        .collect();
    fs::write(&events, rewritten.join("\n") + "\n").unwrap();
    let recorded = events_for(&journal_dir(&source), &file).remove(0);

    let path = bundle_file(&source, "relayed");
    export(&source, &[&file], &path);
    let bundle: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(bundle["artifacts"][0]["events"][0], recorded, "{bundle}");

    let result = import(&recipient, &path, false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(events_for(&journal_dir(&recipient), &file), vec![recorded]);
}

#[test]
fn a_replica_checkpoint_returns_with_provenance_and_is_idempotent() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "return-checkpoint", "plan", "# Plan\n");
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    b.arc(&b.root)
        .args(["claim", &file, "--ttl", "30m"])
        .env("ARC_ACTOR", "executor-b")
        .assert()
        .success();
    b.arc(&b.root)
        .args(["journal", "checkpoint", &file, "--body-file", "-"])
        .env("ARC_ACTOR", "executor-b")
        .write_stdin("Checkpoint on B.\n")
        .assert()
        .success();
    let back = bundle_file(&b, "back");
    export(&b, &[&file], &back);
    let preview = import(&a, &back, true);
    assert!(preview.status.success(), "{preview:?}");
    assert!(
        String::from_utf8_lossy(&preview.stdout).contains("dry-run: would import 1 artifact(s)")
    );
    assert!(!fs::read_to_string(journal_dir(&a).join(&file))
        .unwrap()
        .contains("Checkpoint on B."));
    let result = import(&a, &back, false);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        fs::read(journal_dir(&a).join(&file)).unwrap(),
        fs::read(journal_dir(&b).join(&file)).unwrap()
    );
    let events = events_for(&journal_dir(&a), &file);
    assert_eq!(
        events.iter().find(|e| e["event"] == "checkpoint").unwrap()["actor"],
        "executor-b"
    );
    let body = fs::read(journal_dir(&a).join(&file)).unwrap();
    let before = journal_events_dir(&journal_dir(&a));
    assert!(import(&a, &back, false).status.success());
    assert_eq!(fs::read(journal_dir(&a).join(&file)).unwrap(), body);
    assert_eq!(journal_events_dir(&journal_dir(&a)), before);
    assert_eq!(receipts(&journal_dir(&a)), 1);
}

#[test]
fn concurrent_appends_converge_in_timestamp_and_identity_order() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "concurrent", "discussion", "# Shared\n");
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    for (repo, body) in [
        (&b, "Position: amend\n\nB append.\n"),
        (&a, "Position: for\n\nA append.\n"),
    ] {
        repo.arc(&repo.root)
            .args(["journal", "position", &file, "--body-file", "-"])
            .write_stdin(body)
            .assert()
            .success();
    }
    let from_a = bundle_file(&a, "from-a");
    let from_b = bundle_file(&b, "from-b");
    export(&a, &[&file], &from_a);
    export(&b, &[&file], &from_b);
    let result = import(&a, &from_b, false);
    assert!(result.status.success(), "{result:?}");
    let result = import(&b, &from_a, false);
    assert!(result.status.success(), "{result:?}");
    let body = fs::read_to_string(journal_dir(&a).join(&file)).unwrap();
    assert_eq!(
        body,
        fs::read_to_string(journal_dir(&b).join(&file)).unwrap()
    );
    let mut positions: Vec<_> = events_for(&journal_dir(&a), &file)
        .into_iter()
        .filter(|e| e["event"] == "position")
        .collect();
    positions.sort_by_key(|e| {
        (
            e["ts"].as_str().unwrap().to_string(),
            e["position_id"].as_str().unwrap().to_string(),
        )
    });
    assert!(
        body.find(positions[0]["position_id"].as_str().unwrap())
            .unwrap()
            < body
                .find(positions[1]["position_id"].as_str().unwrap())
                .unwrap()
    );
    let merged = bundle_file(&a, "merged");
    export(&a, &[&file], &merged);
    assert!(import(&b, &merged, false).status.success());
    assert!(import(&a, &from_b, false).status.success());
    assert_eq!(
        body,
        fs::read_to_string(journal_dir(&b).join(&file)).unwrap()
    );
}

#[test]
fn concurrent_evidence_on_a_live_item_converges_between_replicas() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "evidence-exchange", "feature-request", "# Shared\n");
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    for (repo, body) in [(&b, "B found this.\n"), (&a, "A found this.\n")] {
        repo.arc(&repo.root)
            .args(["journal", "evidence", &file, "--body-file", "-"])
            .write_stdin(body)
            .assert()
            .success();
    }
    let from_a = bundle_file(&a, "from-a");
    let from_b = bundle_file(&b, "from-b");
    export(&a, &[&file], &from_a);
    export(&b, &[&file], &from_b);
    let result = import(&a, &from_b, false);
    assert!(result.status.success(), "{result:?}");
    let result = import(&b, &from_a, false);
    assert!(result.status.success(), "{result:?}");
    let body = fs::read_to_string(journal_dir(&a).join(&file)).unwrap();
    assert_eq!(
        body,
        fs::read_to_string(journal_dir(&b).join(&file)).unwrap()
    );
    assert_eq!(body.matches("### Evidence evd-").count(), 2, "{body}");
    assert!(body.contains("A found this.") && body.contains("B found this."));
    let kinds: Vec<_> = events_for(&journal_dir(&a), &file)
        .into_iter()
        .filter(|event| event["event"] == "evidence")
        .collect();
    assert_eq!(kinds.len(), 2);
    assert!(import(&a, &from_b, false).status.success());
    assert_eq!(
        body,
        fs::read_to_string(journal_dir(&a).join(&file)).unwrap()
    );
}

#[test]
fn edited_evidence_blocks_are_refused_like_any_shared_block() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "evidence-edit", "todo", "# Shared\n");
    a.arc(&a.root)
        .args(["journal", "evidence", &file, "--body-file", "-"])
        .write_stdin("Original finding.\n")
        .assert()
        .success();
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    let path = journal_dir(&a).join(&file);
    let body = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        body.replace("Original finding.", "Rewritten finding."),
    )
    .unwrap();
    let modified = bundle_file(&a, "modified");
    export(&a, &[&file], &modified);
    let result = import(&b, &modified, false);
    assert!(!result.status.success(), "{result:?}");
}

#[test]
fn edited_shared_blocks_are_refused_by_identity_and_both_digests() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = discussion_with_records(&a);
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    let path = journal_dir(&a).join(&file);
    let body = fs::read_to_string(&path).unwrap();
    fs::write(&path, body.replace("Move it.", "Edited position.")).unwrap();
    let position_id = events_for(&journal_dir(&a), &file)
        .into_iter()
        .find(|event| event["event"] == "position")
        .unwrap()["position_id"]
        .as_str()
        .unwrap()
        .to_string();
    let modified = bundle_file(&a, "modified");
    export(&a, &[&file], &modified);
    let result = import(&b, &modified, false);
    assert!(!result.status.success(), "{result:?}");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains(&format!("first differing block: {position_id}")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "sha256:{}",
            hex::encode(Sha256::digest(body.as_bytes()))
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "sha256:{}",
            hex::encode(Sha256::digest(fs::read(&path).unwrap()))
        )),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(journal_dir(&b).join(&file)).unwrap(),
        body
    );
    assert_eq!(receipts(&journal_dir(&b)), 1);
}

#[test]
fn typed_amendments_extend_bodies_with_prose_headings_and_fenced_examples() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(
        &a,
        "amendments",
        "discussion",
        "# Shared\n\n### Position examples\n\nAn explanation.\n",
    );
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    b.arc(&b.root)
        .args(["journal", "position", &file, "--body-file", "-"])
        .write_stdin("Position: for\n\n```markdown\n### Position example\n```\n")
        .assert()
        .success();
    let position = events_for(&journal_dir(&b), &file)
        .into_iter()
        .find(|e| e["event"] == "position")
        .unwrap()["position_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.arc(&b.root)
        .args([
            "journal", "correct", &file, "--target", &position, "--field", "stance", "--value",
            "amend",
        ])
        .assert()
        .success();
    b.arc(&b.root)
        .args([
            "journal",
            "retract",
            &file,
            "--target",
            &position,
            "--body-file",
            "-",
        ])
        .write_stdin("Further evidence withdraws the position.\n")
        .assert()
        .success();
    let extended = bundle_file(&b, "extended");
    export(&b, &[&file], &extended);
    let result = import(&a, &extended, false);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        fs::read(journal_dir(&a).join(&file)).unwrap(),
        fs::read(journal_dir(&b).join(&file)).unwrap()
    );
    assert_eq!(
        events_for(&journal_dir(&a), &file),
        events_for(&journal_dir(&b), &file)
    );
    // The older body has never been imported here; retaining the held extension
    // therefore exercises revision comparison rather than the receipt shortcut.
    assert!(import(&a, &initial, false).status.success());
    assert_eq!(
        fs::read(journal_dir(&a).join(&file)).unwrap(),
        fs::read(journal_dir(&b).join(&file)).unwrap()
    );
}

fn assert_event_sets_equal(a: &Repo, b: &Repo, file: &str) {
    let canonical = |repo: &Repo| {
        let mut events: Vec<_> = events_for(&journal_dir(repo), file)
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect();
        events.sort();
        events
    };
    assert_eq!(canonical(a), canonical(b));
}

#[test]
fn every_typed_append_and_reanswered_question_exchanges_in_both_directions() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "all-appends", "discussion", "# Shared\n");
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    a.arc(&a.root)
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
        .write_stdin("Which option?\n")
        .assert()
        .success();
    let question = events_for(&journal_dir(&a), &file)
        .into_iter()
        .find(|e| e["event"] == "question")
        .unwrap()["question_id"]
        .as_str()
        .unwrap()
        .to_string();
    let target = format!("answer:{question}");
    for (index, option) in ["yes", "no"].iter().enumerate() {
        a.arc(&a.root)
            .args([
                "journal",
                "answer",
                &file,
                "--question",
                &question,
                "--option",
                option,
                "--body-file",
                "-",
            ])
            .write_stdin(format!("Answer {index}.\n"))
            .assert()
            .success();
        if index == 0 {
            a.arc(&a.root)
                .args([
                    "journal",
                    "retract",
                    &file,
                    "--target",
                    &target,
                    "--body-file",
                    "-",
                ])
                .write_stdin("Reconsider.\n")
                .assert()
                .success();
        }
    }
    a.arc(&a.root)
        .args(["claim", &file, "--ttl", "30m"])
        .assert()
        .success();
    a.arc(&a.root)
        .args(["journal", "checkpoint", &file, "--body-file", "-"])
        .write_stdin("Work completed.\n")
        .assert()
        .success();
    a.arc(&a.root)
        .args(["release-claim", &file, "--outcome", "paused"])
        .assert()
        .success();
    for repo in [&a, &b] {
        repo.arc(&repo.root)
            .args(["journal", "position", &file, "--body-file", "-"])
            .write_stdin("Position: for\n\nIndependent argument.\n")
            .assert()
            .success();
    }
    let position = events_for(&journal_dir(&a), &file)
        .into_iter()
        .find(|e| e["event"] == "position")
        .unwrap()["position_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.arc(&a.root)
        .args([
            "journal", "correct", &file, "--target", &position, "--field", "stance", "--value",
            "amend",
        ])
        .assert()
        .success();
    a.arc(&a.root)
        .args([
            "journal",
            "consume",
            &file,
            "--outcome",
            "done",
            "--drop-questions",
            "--note",
            "Question retained in the discussion.",
        ])
        .assert()
        .success();
    let from_a = bundle_file(&a, "from-a");
    let from_b = bundle_file(&b, "from-b");
    export(&a, &[&file], &from_a);
    export(&b, &[&file], &from_b);
    for (repo, bundle) in [(&b, &from_a), (&a, &from_b)] {
        let result = import(repo, bundle, false);
        assert!(result.status.success(), "{result:?}");
    }
    assert_eq!(
        fs::read(journal_dir(&a).join(&file)).unwrap(),
        fs::read(journal_dir(&b).join(&file)).unwrap()
    );
    assert_event_sets_equal(&a, &b, &file);
    for kind in [
        "position",
        "question",
        "answer",
        "retraction",
        "correction",
        "checkpoint",
        "consumed",
    ] {
        assert!(
            events_for(&journal_dir(&b), &file)
                .iter()
                .any(|e| e["event"] == kind),
            "missing {kind}"
        );
    }
    let merged = bundle_file(&a, "merged");
    export(&a, &[&file], &merged);
    assert!(import(&b, &merged, false).status.success());
}

#[test]
fn failed_event_append_preserves_bodies_and_retry_converges() {
    use std::os::unix::fs::PermissionsExt;
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "retry-merge", "discussion", "# Shared\n");
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    for (repo, body) in [
        (&b, "Position: for\n\nEarlier incoming.\n"),
        (&a, "Position: amend\n\nLater held.\n"),
    ] {
        repo.arc(&repo.root)
            .args(["journal", "position", &file, "--body-file", "-"])
            .write_stdin(body)
            .assert()
            .success();
    }
    let from_b = bundle_file(&b, "from-b");
    export(&b, &[&file], &from_b);
    let dir = journal_dir(&a);
    let before = fs::read(dir.join(&file)).unwrap();
    let events = fs::read(dir.join("events.jsonl")).unwrap();
    let permissions = fs::metadata(dir.join("events.jsonl"))
        .unwrap()
        .permissions();
    fs::set_permissions(dir.join("events.jsonl"), fs::Permissions::from_mode(0o400)).unwrap();
    let failed = import(&a, &from_b, false);
    fs::set_permissions(dir.join("events.jsonl"), permissions).unwrap();
    assert!(!failed.status.success(), "{failed:?}");
    assert_eq!(fs::read(dir.join(&file)).unwrap(), before);
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), events);
    let retry = import(&a, &from_b, false);
    assert!(retry.status.success(), "{retry:?}");
    let merged = bundle_file(&a, "merged");
    export(&a, &[&file], &merged);
    assert!(import(&b, &merged, false).status.success());
    assert_eq!(
        fs::read(dir.join(&file)).unwrap(),
        fs::read(journal_dir(&b).join(&file)).unwrap()
    );
    assert_event_sets_equal(&a, &b, &file);
    let final_events = fs::read(dir.join("events.jsonl")).unwrap();
    assert!(import(&a, &from_b, false).status.success());
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), final_events);
}

#[test]
fn pending_exchange_recovers_after_body_or_receipt_publication_fails() {
    use std::os::unix::fs::PermissionsExt;
    for failure in ["body", "receipt", "interrupted-log"] {
        let a = Repo::new();
        let b = Repo::new();
        pair(&a, &b, "a", "b");
        let file = write_artifact(&a, "recover-unit", "discussion", "# Shared\n");
        let initial = bundle_file(&a, "initial");
        export(&a, &[&file], &initial);
        assert!(import(&b, &initial, false).status.success());
        for repo in [&b, &a] {
            repo.arc(&repo.root)
                .args(["journal", "position", &file, "--body-file", "-"])
                .write_stdin("Position: for\n\nIndependent append.\n")
                .assert()
                .success();
        }
        let fresh = write_artifact(&b, "zz-recover", "todo", "# Fresh\n");
        let incoming = bundle_file(&b, "incoming");
        export(&b, &[&file, &fresh], &incoming);
        let dir = journal_dir(&a);
        let original = fs::read(dir.join(&file)).unwrap();
        let imports_dir = dir.join("exchange/imports");
        fs::create_dir_all(&imports_dir).unwrap();
        let permissions = fs::metadata(&imports_dir).unwrap().permissions();
        if failure == "body" {
            fs::create_dir(dir.join(&fresh)).unwrap();
        } else {
            fs::set_permissions(&imports_dir, fs::Permissions::from_mode(0o500)).unwrap();
        }
        let failed = import(&a, &incoming, false);
        if failure == "body" {
            fs::remove_dir(dir.join(&fresh)).unwrap();
        } else {
            fs::set_permissions(&imports_dir, permissions).unwrap();
        }
        assert!(!failed.status.success(), "{failed:?}");
        assert!(dir.join("exchange/pending.json").is_file());
        assert_ne!(fs::read(dir.join(&file)).unwrap(), original);
        a.arc(&a.root)
            .args(["journal", "questions", "--json"])
            .assert()
            .success();
        a.arc(&a.root)
            .args(["journal", "position", &file, "--body-file", "-"])
            .write_stdin("Position: against\n\nCannot interleave.\n")
            .assert()
            .failure()
            .stderr(predicates::str::contains("exchange is incomplete"));
        assert!(!import(&a, &incoming, true).status.success());
        if failure == "interrupted-log" {
            let log = fs::read(dir.join("events.jsonl")).unwrap();
            fs::write(dir.join("events.jsonl"), &log[..log.len() - 5]).unwrap();
        }
        let retry = import(&a, &incoming, false);
        assert!(retry.status.success(), "{retry:?}");
        assert!(!dir.join("exchange/pending.json").exists());
        assert!(dir.join(&fresh).is_file());
        let merged = bundle_file(&a, "merged");
        export(&a, &[&file, &fresh], &merged);
        assert!(import(&b, &merged, false).status.success());
        assert_eq!(
            fs::read(dir.join(&file)).unwrap(),
            fs::read(journal_dir(&b).join(&file)).unwrap()
        );
        assert_event_sets_equal(&a, &b, &file);
        assert_event_sets_equal(&a, &b, &fresh);
        let log = fs::read(dir.join("events.jsonl")).unwrap();
        assert!(import(&a, &incoming, false).status.success());
        assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), log);
        assert_eq!(receipts(&dir), 1);
    }
}

fn receipt_failure_fixture() -> (Repo, Repo, String, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = write_artifact(&a, "pending-guard", "discussion", "# Shared\n");
    a.arc(&a.root)
        .args([
            "journal",
            "reattribute",
            &file,
            "--set-actor",
            "earlier-author",
        ])
        .assert()
        .success();
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    b.arc(&b.root)
        .args(["journal", "position", &file, "--body-file", "-"])
        .write_stdin("Position: for\n\nIncoming argument.\n")
        .assert()
        .success();
    let incoming = bundle_file(&b, "incoming");
    export(&b, &[&file], &incoming);
    let dir = journal_dir(&a);
    let imports = dir.join("exchange/imports");
    fs::create_dir_all(&imports).unwrap();
    let permissions = fs::metadata(&imports).unwrap().permissions();
    fs::set_permissions(&imports, fs::Permissions::from_mode(0o500)).unwrap();
    let failed = import(&a, &incoming, false);
    fs::set_permissions(&imports, permissions).unwrap();
    assert!(!failed.status.success(), "{failed:?}");
    assert!(dir.join("exchange/pending.json").is_file());
    (a, b, file, incoming)
}

#[test]
fn pending_exchange_refuses_provenance_repair_until_recovered() {
    assert_pending_writer_refused("reattribute");
}

#[test]
fn pending_exchange_refuses_consume_until_recovered() {
    assert_pending_writer_refused("consume");
}

fn assert_pending_writer_refused(writer: &str) {
    let (a, _b, file, incoming) = receipt_failure_fixture();
    let dir = journal_dir(&a);
    let log = fs::read(dir.join("events.jsonl")).unwrap();
    let body = fs::read(dir.join(&file)).unwrap();
    let pending = fs::read(dir.join("exchange/pending.json")).unwrap();
    let transaction: serde_json::Value = serde_json::from_slice(&pending).unwrap();
    let digest = transaction["receipt"]["bundle_sha256"].as_str().unwrap();
    let args = if writer == "reattribute" {
        vec!["journal", writer, &file, "--set-actor", "repaired-author"]
    } else {
        vec![
            "journal",
            writer,
            &file,
            "--outcome",
            "discarded",
            "--note",
            "Settled",
        ]
    };
    a.arc(&a.root)
        .args(&args)
        .assert()
        .failure()
        .stderr(predicates::str::contains("exchange is incomplete"))
        .stderr(predicates::str::contains("arc journal import <bundle>"))
        .stderr(predicates::str::contains(digest));
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), log);
    assert_eq!(fs::read(dir.join(&file)).unwrap(), body);
    assert_eq!(
        fs::read(dir.join("exchange/pending.json")).unwrap(),
        pending
    );
    a.arc(&a.root)
        .args(["journal", "questions", "--json"])
        .assert()
        .success();
    let retry = import(&a, &incoming, false);
    assert!(retry.status.success(), "{retry:?}");
    assert!(events_for(&dir, &file)
        .iter()
        .any(|event| { event["event"] == "note" && event["actor"] == "earlier-author" }));
    a.arc(&a.root).args(&args).assert().success();
    let settled = fs::read(dir.join("events.jsonl")).unwrap();
    assert!(import(&a, &incoming, false).status.success());
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), settled);
}

#[test]
fn pending_exchange_refuses_body_creation_positions_evidence_and_spool_promotion() {
    let (a, _b, file, incoming) = receipt_failure_fixture();
    let dir = journal_dir(&a);
    a.arc(&a.root)
        .args([
            "journal",
            "note",
            "waiting-spool",
            "--kind",
            "todo",
            "--spool",
            "--body-file",
            "-",
        ])
        .write_stdin("Spooled body\n")
        .assert()
        .success();
    let log = fs::read(dir.join("events.jsonl")).unwrap();
    let body = fs::read(dir.join(&file)).unwrap();
    let mut before: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    before.sort();
    for writer in ["note", "position", "evidence", "spool"] {
        let mut cmd = a.arc(&a.root);
        match writer {
            "note" => {
                cmd.args([
                    "journal",
                    writer,
                    "pending-note",
                    "--kind",
                    "todo",
                    "--body-file",
                    "-",
                ])
                .write_stdin("Must not create a body\n");
            }
            "position" => {
                cmd.args(["journal", writer, &file, "--body-file", "-"])
                    .write_stdin("Position: against\n\nMust not append a body\n");
            }
            "evidence" => {
                cmd.args(["journal", writer, &file, "--body-file", "-"])
                    .write_stdin("Must not append evidence\n");
            }
            _ => {
                cmd.args(["journal", writer, "--promote"]);
            }
        }
        cmd.assert()
            .failure()
            .stderr(predicates::str::contains("arc journal import <bundle>"));
        assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), log);
        assert_eq!(fs::read(dir.join(&file)).unwrap(), body);
        let mut after: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        after.sort();
        assert_eq!(after, before);
    }
    assert!(import(&a, &incoming, false).status.success());
    a.arc(&a.root)
        .args(["journal", "spool", "--promote"])
        .assert()
        .success();
    a.arc(&a.root)
        .args(["journal", "position", &file, "--body-file", "-"])
        .write_stdin("Position: against\n\nSettled exchange\n")
        .assert()
        .success();
}

#[test]
fn pending_exchange_recovery_refuses_unexpected_log_writes() {
    assert_pending_recovery_refused("events.jsonl");
}

#[test]
fn pending_exchange_recovery_refuses_unexpected_body_writes() {
    assert_pending_recovery_refused("body");
}

fn assert_pending_recovery_refused(target: &str) {
    let (a, _b, file, incoming) = receipt_failure_fixture();
    let dir = journal_dir(&a);
    let path = dir.join(if target == "body" { &file } else { target });
    let mut changed = fs::read(&path).unwrap();
    if target == "body" {
        changed.extend_from_slice(b"Unexpected write\n");
    } else {
        changed = String::from_utf8(changed)
            .unwrap()
            .replace("earlier-author", "unexpected-author")
            .into_bytes();
    }
    fs::write(&path, &changed).unwrap();
    let log = fs::read(dir.join("events.jsonl")).unwrap();
    let body = fs::read(dir.join(&file)).unwrap();
    let retry = import(&a, &incoming, false);
    assert!(!retry.status.success(), "{retry:?}");
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("changed after"),
        "{retry:?}"
    );
    assert_eq!(fs::read(dir.join("events.jsonl")).unwrap(), log);
    assert_eq!(fs::read(dir.join(&file)).unwrap(), body);
    assert!(dir.join("exchange/pending.json").is_file());
}

#[test]
fn identical_and_off_menu_reanswers_have_distinct_append_identities() {
    for flag in ["--option", "--other"] {
        let a = Repo::new();
        let b = Repo::new();
        pair(&a, &b, "a", "b");
        let file = write_artifact(&a, "identical-answers", "discussion", "# Shared\n");
        let initial = bundle_file(&a, "initial");
        export(&a, &[&file], &initial);
        assert!(import(&b, &initial, false).status.success());
        a.arc(&a.root)
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
            .write_stdin("Which option?\n")
            .assert()
            .success();
        let question = events_for(&journal_dir(&a), &file)
            .into_iter()
            .find(|e| e["event"] == "question")
            .unwrap()["question_id"]
            .as_str()
            .unwrap()
            .to_string();
        for index in 0..2 {
            a.arc(&a.root)
                .args([
                    "journal",
                    "answer",
                    &file,
                    "--question",
                    &question,
                    flag,
                    "yes",
                    "--body-file",
                    "-",
                ])
                .write_stdin("Same reasoning.\n")
                .assert()
                .success();
            if index == 0 {
                a.arc(&a.root)
                    .args([
                        "journal",
                        "retract",
                        &file,
                        "--target",
                        &format!("answer:{question}"),
                        "--body-file",
                        "-",
                    ])
                    .write_stdin("Reconsider.\n")
                    .assert()
                    .success();
            }
        }
        let answers: Vec<_> = events_for(&journal_dir(&a), &file)
            .into_iter()
            .filter(|e| e["event"] == "answer")
            .collect();
        assert_eq!(answers.len(), 2);
        assert_ne!(answers[0]["digest"], answers[1]["digest"]);
        let incoming = bundle_file(&a, "incoming");
        export(&a, &[&file], &incoming);
        let result = import(&b, &incoming, false);
        assert!(result.status.success(), "{result:?}");
        let back = bundle_file(&b, "back");
        export(&b, &[&file], &back);
        assert!(import(&a, &back, false).status.success());
        assert_eq!(
            fs::read(journal_dir(&a).join(&file)).unwrap(),
            fs::read(journal_dir(&b).join(&file)).unwrap()
        );
        assert_event_sets_equal(&a, &b, &file);
    }
}

#[test]
fn legacy_reanswers_without_block_digests_exchange() {
    let a = Repo::new();
    let b = Repo::new();
    pair(&a, &b, "a", "b");
    let file = discussion_with_records(&a);
    a.arc(&a.root)
        .args(["release-claim", &file, "--outcome", "paused"])
        .assert()
        .success();
    let legacy = || {
        let dir = journal_dir(&a);
        let body = fs::read_to_string(dir.join(&file)).unwrap();
        let body = body
            .split_inclusive('\n')
            .map(|line| {
                if line.starts_with("### Answer ") {
                    let timestamp = line
                        .rsplit_once(", ")
                        .unwrap()
                        .1
                        .trim_end_matches([')', '\n']);
                    let seconds = chrono::DateTime::parse_from_rfc3339(timestamp)
                        .unwrap()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                    line.replace(timestamp, &seconds)
                } else {
                    line.to_string()
                }
            })
            .collect::<String>();
        fs::write(dir.join(&file), body).unwrap();
        let log = journal_events_dir(&dir)
            .lines()
            .map(|line| {
                let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
                if event["file"] == file && event["event"] == "answer" {
                    event.as_object_mut().unwrap().remove("digest");
                }
                serde_json::to_string(&event).unwrap() + "\n"
            })
            .collect::<String>();
        fs::write(dir.join("events.jsonl"), log).unwrap();
    };
    legacy();
    let initial = bundle_file(&a, "initial");
    export(&a, &[&file], &initial);
    assert!(import(&b, &initial, false).status.success());
    let question = events_for(&journal_dir(&a), &file)
        .into_iter()
        .find(|e| e["event"] == "question")
        .unwrap()["question_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.arc(&a.root)
        .args([
            "journal",
            "retract",
            &file,
            "--target",
            &format!("answer:{question}"),
            "--body-file",
            "-",
        ])
        .write_stdin("Reconsider.\n")
        .assert()
        .success();
    thread::sleep(Duration::from_millis(1100));
    a.arc(&a.root)
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
        .write_stdin("Different reasoning.\n")
        .assert()
        .success();
    legacy();
    let incoming = bundle_file(&a, "incoming");
    export(&a, &[&file], &incoming);
    let result = import(&b, &incoming, false);
    assert!(result.status.success(), "{result:?}");
    b.arc(&b.root)
        .args(["journal", "position", &file, "--body-file", "-"])
        .write_stdin("Position: amend\n\nFurther argument.\n")
        .assert()
        .success();
    let back = bundle_file(&b, "back");
    export(&b, &[&file], &back);
    let result = import(&a, &back, false);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        fs::read(journal_dir(&a).join(&file)).unwrap(),
        fs::read(journal_dir(&b).join(&file)).unwrap()
    );
    assert_event_sets_equal(&a, &b, &file);
}
