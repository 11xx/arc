use crate::common::*;

fn declaring(text: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), text).unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

fn events(repo: &Repo, wt: &Path, change: &str, event_type: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(wt).args(["events", "--change", change]))
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

fn gate<'a>(status: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == name)
        .unwrap()
}

const BYPASSABLE: &str =
    "[gates.docs]\ncommand = \"false\"\nbypass = \"documentation or comments only\"\n";

/// Claim: a gate command exiting 75 within its timeout records `not-admitted`,
/// which blocks integration as a gate that did not run, not as one that
/// failed, and which `--skip-green` never reuses. Rejects recording the exit
/// as a failing test, as arc did before the convention was read.
#[test]
fn exit_75_records_not_admitted_and_blocks_integration_as_unrun() {
    let repo = declaring("[gates.heavy]\ncommand = \"echo queue full; exit 75\"\n");
    let (id, wt, head) = change_with_patchset(&repo, "queued");
    let verified = stdout_any_status(repo.arc(&wt).args(["verify", "--gate", "heavy"]));
    assert!(
        verified.contains("verification: NotAdmitted at"),
        "{verified}"
    );
    assert!(verified.contains("before any check ran"), "{verified}");

    let recorded = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["result"], "not-admitted");
    assert_eq!(recorded[0]["exit_code"], 75);
    assert_eq!(recorded[0]["revision"], head.as_str());

    let status = json_stdout(repo.arc(&wt).args(["status"]));
    let heavy = gate(&status, "heavy");
    assert_eq!(heavy["result"], "not-admitted", "{status}");
    assert_eq!(heavy["green_at_head"], false, "{status}");
    assert_eq!(status["next_action"], "run_gate:heavy", "{status}");

    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    let check = stdout_any_status(repo.arc(&wt).args(["check"]));
    assert!(check.contains("was not admitted"), "{check}");
    assert!(!check.contains("the gate failed"), "{check}");
    repo.arc(&wt).args(["check"]).assert().code(5);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);

    let rerun = stdout_any_status(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(!rerun.contains("skipped"), "{rerun}");
    assert_eq!(events(&repo, &wt, &id, "verification-recorded").len(), 2);
}

/// Claim: a timeout outranks the exit status, so a command that exits 75 and
/// is still holding its output when the timeout elapses timed out. Rejects
/// reading the exit status before the timeout.
#[test]
fn exit_75_after_a_timeout_is_a_timeout() {
    let repo = declaring("[gates.heavy]\ncommand = \"sleep 5 & exit 75\"\ntimeout = \"1s\"\n");
    let (id, wt, _) = change_with_patchset(&repo, "overrun");
    repo.arc(&wt)
        .args(["verify", "--gate", "heavy"])
        .assert()
        .code(1);
    let recorded = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(recorded[0]["result"], "fail", "{recorded:?}");
    assert_eq!(recorded[0]["timed_out"], true, "{recorded:?}");
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "heavy")["timed_out"], true, "{status}");
}

/// Claim: only a gate that declares `bypass` can be bypassed, and only with a
/// stated reason. Rejects a bypass on any gate, or one that records nothing.
#[test]
fn bypass_is_refused_on_an_undeclared_gate_and_without_a_reason() {
    let repo = declaring(&format!("{BYPASSABLE}[gates.build]\ncommand = \"false\"\n"));
    let (id, wt, _) = change_with_patchset(&repo, "refusals");
    repo.arc(&wt)
        .args(["verify", "--gate", "build", "--bypass", "README only"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "gate \"build\" does not declare that it may be bypassed",
        ))
        .stderr(predicates::str::contains("bypass = \"<what qualifies>\""));
    for reason in ["", "   ", "n/a"] {
        repo.arc(&wt)
            .args(["verify", "--gate", "docs", "--bypass", reason])
            .assert()
            .failure()
            .stderr(predicates::str::contains("--bypass must say why"));
    }
    assert!(events(&repo, &wt, &id, "verification-bypassed").is_empty());
}

/// Claim: a declared bypass meets the gate's integration requirement at the
/// head it was judged at, and every surface names it as bypassed with its
/// reason, never as a pass. Rejects a bypass that cannot be integrated on, and
/// one that reads as a pass in `status`, `check`, `show` or `explain`.
#[test]
fn a_declared_bypass_lets_integration_proceed_and_explain_names_it() {
    let repo = declaring(BYPASSABLE);
    let (id, wt, head) = change_with_patchset(&repo, "bypassed");
    let reason = "only README.md changed";
    let bypassed = stdout(
        repo.arc(&wt)
            .args(["verify", "--gate", "docs", "--bypass", reason]),
    );
    assert!(
        bypassed.contains(&format!("verification: bypassed at {head}: {reason}")),
        "{bypassed}"
    );
    let recorded = events(&repo, &wt, &id, "verification-bypassed");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["revision"], head.as_str());
    assert_eq!(
        recorded[0]["tree"],
        git_out(&wt, &["rev-parse", "HEAD^{tree}"]).as_str()
    );
    assert_eq!(recorded[0]["reason"], reason);
    assert_eq!(
        recorded[0]["qualification"],
        "documentation or comments only"
    );
    assert!(recorded[0]["actor"].is_string(), "{recorded:?}");
    for absent in ["command", "exit_code", "duration_ms", "hostname"] {
        assert!(recorded[0].get(absent).is_none(), "{absent}: {recorded:?}");
    }

    let status = json_stdout(repo.arc(&wt).args(["status"]));
    let docs = gate(&status, "docs");
    assert_eq!(docs["result"], "bypassed", "{status}");
    assert_eq!(docs["green_at_head"], true, "{status}");
    assert_eq!(docs["bypass"]["reason"], reason, "{status}");
    let show = stdout(repo.arc(&wt).args(["show"]));
    assert!(show.contains("bypassed at head"), "{show}");
    assert!(
        show.contains(&format!(": {reason} (the gate declares")),
        "{show}"
    );

    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    let check = stdout(repo.arc(&wt).args(["check"]));
    assert!(
        check.contains("ready: every integration gate passes or is bypassed"),
        "{check}"
    );
    assert!(check.contains("`docs` bypassed by"), "{check}");
    let check = json_stdout(repo.arc(&wt).args(["check", "--json"]));
    assert_eq!(check["bypassed"]["docs"]["reason"], reason, "{check}");

    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let explanation = json_stdout(repo.arc(&repo.root).args(["explain", &id, "--json"]));
    let rows = explanation["evaluation"]["rows"].as_array().unwrap();
    let row = rows.iter().find(|row| row["gate"] == "docs").unwrap();
    assert_eq!(row["result"], "bypassed", "{explanation}");
    assert_eq!(row["bypass"]["reason"], reason, "{explanation}");
    assert_eq!(row["evidence_event_id"], recorded[0]["event_id"]);
    assert!(row.get("falsification").is_none(), "{explanation}");
    let text = stdout(repo.arc(&repo.root).args(["explain", &id]));
    assert!(text.contains("gate docs: bypassed by `"), "{text}");
    assert!(text.contains(reason), "{text}");
}

/// Claim: a bypass answers for the head it was judged at and nothing else.
/// `--skip-green` reruns the gate rather than reuse it, a new head owes a run
/// or a new bypass, and a publication guard runs the gate however the change
/// that landed was judged. Rejects a bypass that outlives its head, stands in
/// for a pass, or reaches publication.
#[test]
fn a_bypass_stays_at_its_head_and_is_never_a_pass() {
    let repo = declaring(BYPASSABLE);
    let (id, wt, first) = change_with_patchset(&repo, "bounded");
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();

    let rerun = stdout_any_status(repo.arc(&wt).args(["verify", "--all", "--skip-green"]));
    assert!(!rerun.contains("skipped"), "{rerun}");
    assert!(events(&repo, &wt, &id, "verification-reused").is_empty());
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "docs")["result"], "fail", "{status}");

    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "a judgement does not overrule a run",
        ));

    repo.commit(&wt, "second.txt", "second\n", "test: second");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    let moved = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&moved, "docs")["green_at_head"], false, "{moved}");
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();
    git(&wt, &["commit", "--amend", "-m", "test: reworded"]);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    let reworded = json_stdout(repo.arc(&wt).args(["status"]));
    let docs = gate(&reworded, "docs");
    assert_eq!(docs["result"], "bypassed", "{reworded}");
    assert_eq!(docs["green_at_head"], false, "{reworded}");
    let check = stdout_any_status(repo.arc(&wt).args(["check"]));
    assert!(check.contains("not at this head"), "{check}");
    assert_ne!(repo.head(&wt), first);

    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id, "--debt", "Fixture retains missing review"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["publication", "--target", "master"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("publication gate docs: fail"));
}

/// Claim: a bypass binds the head where the judgement was recorded, even
/// across a same-tree recorded trailer rewrite. Rejects a rewrite moving
/// that judgement to a head nobody bypassed, without running the gate.
#[test]
fn a_same_tree_trailer_rewrite_requires_a_new_bypass() {
    let repo = declaring(BYPASSABLE);
    let (id, wt, original) = change_with_patchset(&repo, "rewritten-bypass");
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check"]).assert().success();
    let tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);

    repo.arc(&wt)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &original,
            "--no-sign",
            "--append",
            "Planned-by: Planner <planner@example.invalid>",
        ])
        .assert()
        .success();
    assert_ne!(repo.head(&wt), original);
    assert_eq!(git_out(&wt, &["rev-parse", "HEAD^{tree}"]), tree);

    let status = json_stdout(repo.arc(&wt).args(["status"]));
    let docs = gate(&status, "docs");
    assert_eq!(docs["result"], "bypassed", "{status}");
    assert_eq!(
        docs["green_at_head"], false,
        "a recorded rewrite must not carry a bypass to its successor: {status}"
    );
    assert_eq!(docs["inherited_from"], original, "{status}");
    repo.arc(&wt)
        .args(["check"])
        .assert()
        .code(5)
        .stdout(predicates::str::contains("not at this head"));
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);
    assert!(events(&repo, &wt, &id, "verification-recorded").is_empty());
    assert_eq!(events(&repo, &wt, &id, "verification-bypassed").len(), 1);

    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();
    repo.arc(&wt).args(["check"]).assert().success();
}

/// Claim: only a run green at the head answers its unresolved failure;
/// not-admitted retries leave bypass refusal and the event ledger intact.
/// Rejects an unrun attempt making a failed gate eligible for bypass.
#[test]
fn not_admitted_retries_do_not_erase_an_unresolved_gate_failure() {
    let repo = declaring(
        r#"[gates.docs]
command = 'exit "$(cat "$HOME/gate-exit")"'
bypass = "documentation or comments only"
"#,
    );
    let (id, wt, _) = change_with_patchset(&repo, "unresolved-failure");
    let outcome = repo.home.join("gate-exit");
    fs::write(&outcome, "0").unwrap();
    repo.arc(&wt)
        .args(["verify", "--gate", "docs"])
        .assert()
        .success();
    fs::write(&outcome, "1").unwrap();
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--rerun"])
        .assert()
        .code(1);
    let failed = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(failed[1]["result"], "fail");

    fs::write(&outcome, "75").unwrap();
    for _ in 0..2 {
        repo.arc(&wt)
            .args(["verify", "--gate", "docs"])
            .assert()
            .code(1)
            .stdout(predicates::str::contains("verification: NotAdmitted at"));
        let before = stdout(repo.arc(&wt).args(["events", "--change", &id]));
        repo.arc(&wt)
            .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "a judgement does not overrule a run",
            ))
            .stderr(predicates::str::contains(
                failed[1]["event_id"].as_str().unwrap(),
            ));
        assert_eq!(
            stdout(repo.arc(&wt).args(["events", "--change", &id])),
            before,
            "refusing the bypass must not append any event"
        );
    }
    assert!(events(&repo, &wt, &id, "verification-bypassed").is_empty());
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "docs")["green_at_head"], false, "{status}");

    fs::write(&outcome, "0").unwrap();
    repo.arc(&wt)
        .args(["verify", "--gate", "docs"])
        .assert()
        .success();
    fs::write(&outcome, "75").unwrap();
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--rerun"])
        .assert()
        .code(1);
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();
}

fn a_non_green_pass_cannot_clear_failure_for_bypass(mutate_during_run: bool) {
    let repo = declaring(
        r#"[gates.docs]
command = 'if test -f "$HOME/move-tree"; then touch fix; fi; test -f fix'
bypass = "documentation or comments only"
"#,
    );
    let (id, wt, head) = change_with_patchset(&repo, "non-green-answer");
    repo.arc(&wt)
        .args(["verify", "--gate", "docs"])
        .assert()
        .code(1);
    let failed = events(&repo, &wt, &id, "verification-recorded");
    assert_eq!(failed[0]["result"], "fail");
    assert_eq!(failed[0]["worktree_dirty"], false);

    if mutate_during_run {
        fs::write(repo.home.join("move-tree"), "move\n").unwrap();
    } else {
        fs::write(wt.join("fix"), "uncommitted fix\n").unwrap();
    }
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--rerun"])
        .assert()
        .success();
    let recorded = events(&repo, &wt, &id, "verification-recorded");
    let passed = &recorded[1];
    assert_eq!(passed["result"], "pass");
    assert_eq!(passed["revision"], head);
    if mutate_during_run {
        assert_eq!(passed["tree_moved"], true, "{passed}");
        fs::remove_file(repo.home.join("move-tree")).unwrap();
    } else {
        assert_eq!(passed["worktree_dirty"], true, "{passed}");
        assert_ne!(passed["tested_tree"], failed[0]["tested_tree"]);
    }
    fs::remove_file(wt.join("fix")).unwrap();
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "docs")["result"], "pass", "{status}");
    assert_eq!(gate(&status, "docs")["green_at_head"], false, "{status}");

    let before = stdout(repo.arc(&wt).args(["events", "--change", &id]));
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "a judgement does not overrule a run",
        ))
        .stderr(predicates::str::contains(
            failed[0]["event_id"].as_str().unwrap(),
        ));
    assert_eq!(
        stdout(repo.arc(&wt).args(["events", "--change", &id])),
        before,
        "a non-green pass must not authorize a bypass or append an event"
    );
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "docs")["green_at_head"], false, "{status}");

    fs::write(wt.join("fix"), "uncommitted fix\n").unwrap();
    repo.arc(&wt)
        .args([
            "verify",
            "--gate",
            "docs",
            "--rerun",
            "--waive-dirty",
            "the uncommitted fixture is the intended tested content",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status"]));
    assert_eq!(gate(&status, "docs")["green_at_head"], true, "{status}");
    fs::remove_file(wt.join("fix")).unwrap();
    repo.arc(&wt)
        .args(["verify", "--gate", "docs", "--bypass", "comment-only edit"])
        .assert()
        .success();
}

/// Claim: a pass on an unwaived dirty tree cannot answer a clean failure at
/// the head. Rejects a bypass that treats uncommitted fixes as committed.
#[test]
fn a_dirty_pass_cannot_clear_a_head_failure_for_bypass() {
    a_non_green_pass_cannot_clear_failure_for_bypass(false);
}

/// Claim: a pass whose tree moved cannot answer a clean failure at the head.
/// Rejects a bypass authorized by evidence for no single reproducible tree.
#[test]
fn a_tree_moved_pass_cannot_clear_a_head_failure_for_bypass() {
    a_non_green_pass_cannot_clear_failure_for_bypass(true);
}
