use crate::common::*;
use serde_json::Value;

fn declaration(gate: u64, total: u64) -> String {
    format!("[budget]\ntotal='{total}ms'\n[selection]\ninert=['.arc/']\n[gates.unit]\ncommand='true'\nbudget='{gate}ms'\npaths=['src/']\n")
}
fn setup(slug: &str, new: u64, lines: usize) -> (Repo, String, PathBuf) {
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, 20000));
    let (id, wt, _) = change_with_patchset(&repo, slug);
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(&wt, "src/module.rs", &lines_text(lines), "test: add module");
    if new != 10000 {
        repo.commit(
            &wt,
            ".arc/gates.toml",
            &declaration(new, 20000),
            "test: declare budget",
        );
    }
    snapshot(&repo, &wt);
    (repo, id, wt)
}
fn lines_text(lines: usize) -> String {
    (0..lines).map(|i| format!("value_{i}\n")).collect()
}
fn snapshot(repo: &Repo, wt: &Path) {
    repo.arc(wt).args(["snapshot"]).assert().success();
}
fn approve(repo: &Repo, wt: &Path) {
    repo.arc(wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
}
fn verify(repo: &Repo, wt: &Path, ms: u64, code: i32) {
    repo.arc(wt)
        .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(code);
}
fn raise(repo: &Repo, wt: &Path) -> AssertCommand {
    let mut cmd = repo.arc(wt);
    cmd.args([
        "gates",
        "raise",
        "unit",
        "--reason",
        "Measured and optimized the added coverage",
    ]);
    cmd
}
fn shown(repo: &Repo, wt: &Path, id: &str) -> Value {
    json_stdout(repo.arc(wt).args(["show", id, "--json"]))
}
fn refusal(command: &mut AssertCommand, expected: &str) {
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "unexpected acceptance: {text}");
    assert!(text.contains(expected), "missing {expected:?}: {text}");
}

fn unrecorded(path: &str) {
    for (new_gate, new_total) in [(10400, 20000), (10000, 20400)] {
        let (repo, id, wt) = setup("unrecorded", new_gate, 100);
        if new_total != 20000 {
            repo.commit(
                &wt,
                ".arc/gates.toml",
                &declaration(new_gate, new_total),
                "test: total declaration",
            );
        }
        snapshot(&repo, &wt);
        approve(&repo, &wt);
        verify(&repo, &wt, 9000, 0);
        let before = repo.head(&repo.root);
        match path {
            "integrate" => refusal(
                repo.arc(&repo.root).args(["integrate", &id]),
                "requires an applicable accepted raise record",
            ),
            "land" => {
                let (other, _, _) = change_with_patchset(&repo, "other-member");
                refusal(
                    repo.arc(&repo.root).args([
                        "land",
                        &id,
                        &other,
                        "--debt",
                        "combined review owed",
                    ]),
                    "requires an applicable accepted raise record",
                );
            }
            _ => {
                for cached in [false, true] {
                    let branch = git_out(&wt, &["branch", "--show-current"]);
                    let mut cmd = repo.arc(&wt);
                    cmd.args(["publication", "--target", &branch, "--change", &id])
                        .env("ARC_TEST_COMMAND_RUN_MS", "9000");
                    if cached {
                        cmd.arg("--skip-green");
                    }
                    refusal(&mut cmd, "requires an applicable accepted raise record");
                }
            }
        }
        assert_eq!(repo.head(&repo.root), before);
    }
}

/// Claim: integrate refuses higher gate or total declarations without records even
/// when runs fit the old budget. Rejects runtime greenness installing a looser bound.
#[test]
fn raise_unrecorded_green_integrate() {
    unrecorded("integrate");
}

/// Claim: landing refuses higher gate or total declarations without records even
/// when runs fit the old budget. Rejects suppressed member blockers installing a looser bound.
#[test]
fn raise_unrecorded_green_land() {
    unrecorded("land");
}

/// Claim: fresh and cached publication refuse higher gate or total declarations without
/// records while runs fit the old budget. Rejects the publication branch authorizing itself.
#[test]
fn raise_unrecorded_green_publication() {
    unrecorded("publication");
}

/// Claim: exact size allowance and a direct over-budget pass establish a raise,
/// but its appended evidence needs a fresh review subject. Rejects a prior verdict
/// covering a later raise set and requiring historical exceptions in later changes.
#[test]
fn raise_boundary_binds_review_and_becomes_the_next_targets_budget() {
    let (repo, id, wt) = setup("boundary", 10400, 100);
    approve(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    raise(&repo, &wt).assert().success();
    let st = shown(&repo, &wt, &id);
    let r = &st["budget_raises"][0]["raise"]["increases"][0];
    assert_eq!(r["added"], 100);
    assert_eq!(r["allowance_ms"], 400);
    assert_eq!(r["old_ms"], 10000);
    assert_eq!(r["new_ms"], 10400);
    assert_eq!(st["gate_status"][0]["budget"]["budget_ms"], 10400);
    refusal(repo.arc(&wt).args(["check", &id]), "fresh snapshot");
    snapshot(&repo, &wt);
    refusal(repo.arc(&repo.root).args(["integrate", &id]), "approval");
    assert!(stdout(repo.arc(&wt).args(["show", &id])).contains("Budget review checklist"));
    let before = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args([
            "integrate",
            &id,
            "--debt",
            "Independent review remains owed",
        ])
        .assert()
        .success();
    assert_ne!(repo.head(&repo.root), before);
    let historic = shown(&repo, &repo.root, &id);
    assert_eq!(historic["budget_raises"], st["budget_raises"]);
    assert!(historic["closure"]["authorization"]["verdict_event_id"].is_null());
    assert!(historic["closure"]["authorization"]["audit_debt_event_id"].is_string());
    let (next, next_wt, _) = change_with_patchset(&repo, "next-budget");
    verify(&repo, &next_wt, 10300, 0);
    let status = json_stdout(repo.arc(&next_wt).args(["status", &next]));
    assert_eq!(status["gates"][0]["budget"]["budget_ms"], 10400);
    assert!(status["budget_raises"].as_array().unwrap().is_empty());
}

/// Claim: one millisecond beyond the trusted size allowance, self-loosened rules,
/// repeated requests and an operator ceiling cannot enlarge entitlement. Rejects
/// head-controlled multipliers, compounded raises and a local ceiling override.
#[test]
fn raise_allowance_and_operator_ceiling_are_absolute() {
    for mode in ["one-over", "self-rules"] {
        let (repo, _, wt) = setup(mode, 10401, 100);
        if mode == "self-rules" {
            let text = declaration(10401, 20000)
                .replace("[budget]", "[budget]\nraise='1000%'\nraise_per=1");
            repo.commit(&wt, ".arc/gates.toml", &text, "test: self-loosen allowance");
            snapshot(&repo, &wt);
        }
        verify(&repo, &wt, 10300, 1);
        for _ in 0..2 {
            refusal(
                &mut raise(&repo, &wt),
                "exceeds allowance 0.4s (100 eligible lines)",
            );
        }
    }
}

/// Claim: only the newest genuine clean execution at this head justifies a raise.
/// Rejects missing or unneeded runs, newer failures, reuse, attestation, bypass,
/// wrong declaration/environment and unknown local provenance.
#[test]
fn raise_needs_the_newest_direct_clean_over_budget_execution() {
    fn next_id(id: &str) -> String {
        let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let mut bytes = id.as_bytes().to_vec();
        for byte in bytes.iter_mut().rev() {
            let n = alphabet.iter().position(|x| x == byte).unwrap();
            *byte = alphabet[(n + 1) % 32];
            if n < 31 {
                break;
            }
        }
        String::from_utf8(bytes).unwrap()
    }
    let mut accepted = Vec::new();
    for mode in [
        "missing",
        "unneeded",
        "failure",
        "reuse",
        "attestation",
        "wrong-declaration",
        "wrong-environment",
        "unknown-provenance",
        "bypass",
    ] {
        let (repo, id, wt) = if mode == "wrong-environment" {
            let repo = Repo::new();
            let probe = "environment='printf fixture'\n";
            repo.redeclare_gates(&format!("{}{probe}", declaration(10000, 20000)));
            let (id, wt, _) = change_with_patchset(&repo, mode);
            fs::create_dir_all(wt.join("src")).unwrap();
            repo.commit(&wt, "src/module.rs", &lines_text(100), "test: module");
            repo.commit(
                &wt,
                ".arc/gates.toml",
                &format!("{}{probe}", declaration(10400, 20000)),
                "test: declaration",
            );
            snapshot(&repo, &wt);
            (repo, id, wt)
        } else {
            setup(mode, 10400, 100)
        };
        match mode {
            "missing" => {}
            "unneeded" => verify(&repo, &wt, 9000, 0),
            "reuse" => {
                verify(&repo, &wt, 10300, 1);
                git(
                    &wt,
                    &["commit", "--allow-empty", "-m", "test: changed commit"],
                );
                snapshot(&repo, &wt);
                repo.arc(&wt).args(["verify", "--all"]).assert().code(1);
            }
            "attestation" => {
                let head = repo.head(&wt);
                repo.arc(&wt)
                    .args([
                        "verify",
                        "--gate",
                        "unit",
                        "--attest",
                        "--result",
                        "pass",
                        "--tested-revision",
                        &head,
                        "--execution-host",
                        "fixture",
                        "--runner",
                        "fixture",
                    ])
                    .assert()
                    .code(1);
            }
            _ => {
                verify(&repo, &wt, 10300, 1);
                let mut files = fs::read_dir(event_dir(&repo, &id))
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect::<Vec<_>>();
                files.sort();
                let path = files
                    .into_iter()
                    .rev()
                    .find(|p| {
                        let v: Value = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
                        v["event_type"] == "verification-recorded"
                    })
                    .unwrap();
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                match mode {
                    "failure" => {
                        let next = next_id(value["event_id"].as_str().unwrap());
                        value["event_id"] = next.clone().into();
                        value["run_id"] = Value::Null;
                        value["result"] = "fail".into();
                        value["exit_code"] = 1.into();
                        fs::write(
                            path.parent().unwrap().join(format!("{next}.json")),
                            serde_json::to_vec(&value).unwrap(),
                        )
                        .unwrap();
                    }
                    "wrong-declaration" => {
                        value["declaration"] = format!("sha256:{}", "0".repeat(64)).into();
                        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                    }
                    "wrong-environment" => {
                        value["environment"]["identity"] =
                            format!("sha256:{}", "0".repeat(64)).into();
                        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                    }
                    "unknown-provenance" => {
                        value["worktree_dirty"] = Value::Null;
                        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                    }
                    _ => {
                        value["event_type"] = "verification-bypassed".into();
                        value["reason"] = "fixture bypass".into();
                        value["qualification"] = "fixture qualification".into();
                        fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                    }
                }
            }
        }
        let output = raise(&repo, &wt).output().unwrap();
        if output.status.success() {
            accepted.push(mode);
        } else {
            let text = String::from_utf8_lossy(&output.stderr);
            let expected = if mode == "attestation" {
                "observed run_ms"
            } else if mode == "wrong-environment" {
                "environment does not match"
            } else {
                "direct execution"
            };
            assert!(text.contains(expected), "{mode}: {text}");
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid proof accepted for {accepted:?}"
    );
}

/// Claim: moves, copies, whitespace-only edits, binaries and trusted generated/vendor
/// paths earn no allowance. Rejects diff settings or padding expanding size credit.
fn padding(mode: &str) {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join("src")).unwrap();
    repo.commit(
        &repo.root,
        "src/original.rs",
        &lines_text(600),
        "test: existing content",
    );
    if mode == "binary-attributes" {
        repo.commit(
            &repo.root,
            ".gitattributes",
            "*.dat binary\n",
            "test: binary classification",
        );
    }
    repo.redeclare_gates(&declaration(10000, 20000));
    repo.declare_gates_locally("[budget]\nexclude=['src/generated/','src/vendor/']\n");
    let (id, wt, _) = change_with_patchset(&repo, mode);
    match mode {
        "move" => {
            git(&wt, &["mv", "src/original.rs", "src/moved.rs"]);
        }
        "copy" => {
            fs::copy(wt.join("src/original.rs"), wt.join("src/copy.rs")).unwrap();
        }
        "whitespace" => {
            fs::write(
                wt.join("src/original.rs"),
                lines_text(600).replace('\n', " \n\n\n"),
            )
            .unwrap();
        }
        "binary-attributes" => {
            fs::write(
                wt.join("src/opaque.dat"),
                lines_text(600).replace("value", "opaque"),
            )
            .unwrap();
        }
        "binary" => {
            fs::write(wt.join("src/binary.bin"), vec![0; 10000]).unwrap();
        }
        _ => {
            fs::create_dir_all(wt.join(format!("src/{mode}"))).unwrap();
            fs::write(wt.join(format!("src/{mode}/padded.rs")), lines_text(600)).unwrap();
        }
    }
    fs::write(wt.join(".arc/gates.toml"), declaration(10001, 20000)).unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "test: attempted padding"]);
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10001, 1);
    for setting in ["false", "copies"] {
        git(&wt, &["config", "diff.renames", setting]);
        refusal(
            &mut raise(&repo, &wt),
            "exceeds allowance 0s (0 eligible lines)",
        );
    }
    assert!(shown(&repo, &wt, &id)["budget_raises"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// Claim: move padding earns no allowance. Rejects counting excluded or
/// unchanged content as growth under either configured rename setting.
#[test]
fn raise_padding_move() {
    padding("move");
}

/// Claim: copy padding earns no allowance. Rejects counting excluded or
/// unchanged content as growth under either configured rename setting.
#[test]
fn raise_padding_copy() {
    padding("copy");
}

/// Claim: whitespace padding earns no allowance. Rejects counting excluded or
/// unchanged content as growth under either configured rename setting.
#[test]
fn raise_padding_whitespace() {
    padding("whitespace");
}

/// Claim: binary padding earns no allowance. Rejects counting excluded or
/// unchanged content as growth under either configured rename setting.
#[test]
fn raise_padding_binary() {
    padding("binary");
}

/// Claim: generated padding earns no allowance. Rejects counting excluded or
/// unchanged content as growth under either configured rename setting.
#[test]
fn raise_padding_generated() {
    padding("generated");
}

/// Claim: vendor padding earns no allowance. Rejects counting excluded or
/// unchanged content as growth under either configured rename setting.
#[test]
fn raise_padding_vendor() {
    padding("vendor");
}

/// Claim: removing counted growth, changing the exact head or moving the target
/// invalidates a raise while history stays readable. Rejects rewrite projection,
/// rebase or squash carrying an immutable entitlement onto another patchset.
#[test]
fn raise_head_and_destination_changes_leave_only_historical_evidence() {
    for mode in ["amend", "same-tree", "target"] {
        let (repo, id, wt) = setup(mode, 10400, 100);
        verify(&repo, &wt, 10300, 1);
        raise(&repo, &wt).assert().success();
        snapshot(&repo, &wt);
        let record = shown(&repo, &wt, &id)["budget_raises"].clone();
        match mode {
            "amend" => {
                fs::write(wt.join("src/module.rs"), lines_text(5)).unwrap();
                git(&wt, &["add", "."]);
                git(&wt, &["commit", "--amend", "--no-edit"]);
            }
            "same-tree" => {
                git(
                    &wt,
                    &["commit", "--allow-empty", "-m", "test: another patchset"],
                );
            }
            _ => {
                repo.commit(
                    &repo.root,
                    "upstream.rs",
                    "upstream\n",
                    "test: target moves",
                );
            }
        }
        let st = shown(&repo, &wt, &id);
        assert_eq!(st["budget_raises"], record);
        assert_eq!(
            st["budget_raises"][0]["raise"]["increases"][0]["added"],
            100
        );
        assert_eq!(
            st["budget_raises"][0]["raise"]["head"],
            record[0]["raise"]["head"]
        );
        assert_eq!(st["gate_status"][0]["budget"]["budget_ms"], 10000);
        refusal(
            repo.arc(&wt).args(["check", &id]),
            "requires an applicable accepted raise record",
        );
    }
}

/// Claim: the integration total can rise explicitly for a new gate without an
/// existing gate exceeding its budget. Rejects implicit total increases, invented
/// total execution evidence and a total whose declaration sum does not show need.
#[test]
fn raise_total_only_uses_the_declaration_sum() {
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, 10000));
    let (id, wt, _) = change_with_patchset(&repo, "new-gate");
    fs::create_dir_all(wt.join("src")).unwrap();
    let text = format!(
        "{}[gates.extra]\ncommand='true'\nbudget='400ms'\n",
        declaration(10000, 10400)
    );
    repo.commit(&wt, "src/added.rs", &lines_text(100), "test: new coverage");
    repo.commit(&wt, ".arc/gates.toml", &text, "test: new gate");
    snapshot(&repo, &wt);
    verify(&repo, &wt, 1, 0);
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "--total",
            "--reason",
            "Added an independently useful gate",
        ])
        .assert()
        .success();
    snapshot(&repo, &wt);
    approve(&repo, &wt);
    let st = shown(&repo, &wt, &id);
    let total = &st["budget_raises"][0]["raise"]["increases"][0];
    assert_eq!(total["declaration_sum_ms"], 10400);
    assert!(total["execution"].is_null());
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
}

/// Claim: exported raises preserve their execution and counting evidence but
/// receiver policy controls applicability. Rejects transported acceptance overriding
/// a stricter receiver ceiling or losing the original record during replay.
#[test]
fn raise_import_keeps_facts_and_rejudges_receiver_constraints() {
    let (repo, id, wt) = setup("transport-raise", 10400, 100);
    verify(&repo, &wt, 10300, 1);
    raise(&repo, &wt).assert().success();
    snapshot(&repo, &wt);
    let before = shown(&repo, &wt, &id)["budget_raises"].clone();
    let bundle = repo.home.join("raise.json");
    repo.arc(&wt)
        .args(["export", &id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let receiver = Repo::new();
    deliver_git_objects(&repo, &receiver);
    git(
        &receiver.root,
        &[
            "update-ref",
            "refs/heads/arc/transport-raise",
            &repo.head(&wt),
        ],
    );
    git(&receiver.root, &["reset", "--hard", &repo.head(&repo.root)]);
    receiver
        .arc(&receiver.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let state = shown(&receiver, &receiver.root, &id);
    assert_eq!(state["budget_raises"], before);
    receiver.declare_gates_locally("[budget]\ntotal='19s'\n");
    let status = json_stdout(receiver.arc(&receiver.root).args(["status", &id]));
    assert_eq!(status["budget_raises"][0]["applicable"], false);
    assert!(status["budget_raises"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("policy basis changed"));
}

/// Claim: budgeted projects without an operator ceiling expose that repeated raises
/// are unbounded in doctor and gate reports. Rejects silently implying a cumulative bound.
#[test]
fn raise_without_operator_ceiling_is_reported() {
    let (repo, _, wt) = setup("unbounded", 10000, 100);
    assert!(stdout(repo.arc(&wt).args(["gates"])).contains("nothing bounds repeated raises"));
    let output = repo.arc(&wt).args(["doctor"]).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("nothing bounds repeated raises"), "{text}");
}

fn register(repo: &Repo, id: &str, wt: &Path, name: &str) -> String {
    repo.declare_gates_locally("[candidates]\nevaluation_reuse='never'\n");
    repo.arc(wt)
        .args(["brief", id, "--body-file", "-"])
        .write_stdin("Evaluate the candidate coverage")
        .assert()
        .success();
    let tree = git_out(wt, &["rev-parse", "HEAD^{tree}"]);
    repo.arc(wt)
        .args([
            "candidate",
            "register",
            "--tree",
            &tree,
            "--brief",
            id,
            "--producer",
            "tester",
            "--id",
            name,
        ])
        .assert()
        .success();
    tree
}

/// Claim: candidate verification and selection refuse a higher unrecorded declaration
/// before consuming an otherwise fast pass. Rejects the candidate tree self-authorizing
/// a higher budget independently of the destination change.
#[test]
fn raise_unrecorded_green_candidate_selection() {
    let (repo, id, wt) = setup("candidate-increase", 10400, 100);
    register(&repo, &id, &wt, "raised-candidate");
    refusal(
        repo.arc(&wt).env("ARC_TEST_COMMAND_RUN_MS", "9000").args([
            "candidate",
            "verify",
            "raised-candidate",
        ]),
        "requires an applicable accepted raise record",
    );
    let target = repo.head(&repo.root);
    refusal(
        repo.arc(&wt).args([
            "candidate",
            "select",
            "--chosen",
            "raised-candidate",
            "--into",
            &id,
            "--target",
            &target,
            "--rationale",
            "matching coverage",
        ]),
        "requires an applicable accepted raise record",
    );
}

/// Claim: candidate promotion rechecks a raise even when the selected execution fits
/// the old budget, including recovery. Rejects a selected record authorizing an effect after its supporting
/// raise has become unavailable, without either branch moving.
#[test]
fn raise_unrecorded_green_candidate_promotion() {
    let (repo, id, wt) = setup("promote-increase", 10400, 100);
    verify(&repo, &wt, 10300, 1);
    raise(&repo, &wt).assert().success();
    snapshot(&repo, &wt);
    register(&repo, &id, &wt, "raised-candidate");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .args(["candidate", "verify", "raised-candidate"])
        .assert()
        .success();
    let events = fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    let evaluation = events
        .iter()
        .map(|p| serde_json::from_slice::<Value>(&fs::read(p).unwrap()).unwrap())
        .find(|e| e["event_type"] == "candidate-verified")
        .unwrap()["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    let target = repo.head(&repo.root);
    let head = repo.head(&wt);
    let release = repo.home.join("promotion-release");
    let mut child = repo.spawn_arc_piped_with_env(
        &wt,
        &[
            "candidate",
            "select",
            "--chosen",
            "raised-candidate",
            "--into",
            &id,
            "--target",
            &target,
            "--evaluation",
            &evaluation,
            "--rationale",
            "matching coverage",
        ],
        &[("ARC_SELECT_PAUSE", &release)],
    );
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let selection = loop {
        let mut line = String::new();
        if output.read_line(&mut line).unwrap() == 0 {
            let result = child.wait_with_output().unwrap();
            panic!(
                "selection was not recorded: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        if let Some(id) = line.trim().strip_prefix("selection: ") {
            break id.to_string();
        }
    };
    for entry in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = entry.unwrap().path();
        let event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "gate-budget-raised" {
            fs::remove_file(path).unwrap();
        }
    }
    fs::write(release, "").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let text = String::from_utf8_lossy(&result.stderr);
    assert!(
        text.contains("requires an applicable accepted raise record"),
        "{text}"
    );
    assert_eq!(repo.head(&repo.root), target);
    assert_eq!(repo.head(&wt), head);
    refusal(
        repo.arc(&wt).args(["candidate", "promote", &selection]),
        "requires an applicable accepted raise record",
    );
    let recovery_ref = format!("refs/arc/candidate-promotion/raised-candidate/{selection}");
    git(&wt, &["update-ref", &recovery_ref, &head]);
    refusal(
        repo.arc(&wt).args(["candidate", "promote", &selection]),
        "requires an applicable accepted raise record",
    );
}

/// Claim: a record's need is frozen at issuance while newer genuine executions
/// retain ordinary authority. Rejects invalidating raises on forced reruns or letting
/// the cited pass mask a newer failed gate.
#[test]
fn raise_survives_a_newer_execution_but_never_masks_its_failure() {
    let (repo, id, wt) = setup("newer-execution", 10400, 100);
    verify(&repo, &wt, 10300, 1);
    raise(&repo, &wt).assert().success();
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10200, 0);
    let status = json_stdout(repo.arc(&wt).args(["status", &id]));
    assert_eq!(status["budget_raises"][0]["applicable"], true);
    assert_eq!(status["gates"][0]["green_at_head"], true);
    verify(&repo, &wt, 10401, 1);
    let status = json_stdout(repo.arc(&wt).args(["status", &id]));
    assert_eq!(status["budget_raises"][0]["applicable"], true);
    assert_eq!(status["gates"][0]["green_at_head"], false);
}

/// Claim: a snapshot binds its recorded raise set even when an imported record
/// sorts before it. Rejects reconstructing an old review subject from subsequently
/// available history and crediting an approval with a raise it never covered.
#[test]
fn raise_late_earlier_record_cannot_retroactively_change_a_review_subject() {
    let (repo, id, wt) = setup("late-raise", 10400, 100);
    verify(&repo, &wt, 10300, 1);
    repo.arc(&wt)
        .args(["brief", &id, "--body-file", "-"])
        .write_stdin("Assess added coverage")
        .assert()
        .success();
    snapshot(&repo, &wt);
    approve(&repo, &wt);
    raise(&repo, &wt).assert().success();
    for entry in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] != "gate-budget-raised" {
            continue;
        }
        let mut next = event["raise"]["increases"][0]["execution"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        let alphabet = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        for byte in next.iter_mut().rev() {
            let n = alphabet.iter().position(|x| x == byte).unwrap();
            *byte = alphabet[(n + 1) % 32];
            if n < 31 {
                break;
            }
        }
        let next = String::from_utf8(next).unwrap();
        event["event_id"] = next.clone().into();
        fs::remove_file(&path).unwrap();
        fs::write(
            path.parent().unwrap().join(format!("{next}.json")),
            serde_json::to_vec(&event).unwrap(),
        )
        .unwrap();
    }
    refusal(repo.arc(&wt).args(["check", &id]), "fresh snapshot");
}

/// Claim: simultaneous gate and total raises preserve the independent operator
/// ceiling. Rejects a record overriding the final minimum with its proposed total.
#[test]
fn raise_operator_ceiling_remains_binding() {
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, 10000));
    repo.declare_gates_locally("[budget]\ntotal='10399ms'\n");
    let (_, wt, _) = change_with_patchset(&repo, "ceiling");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(&wt, "src/module.rs", &lines_text(100), "test: module");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &declaration(10400, 10400),
        "test: simultaneous raises",
    );
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    refusal(raise(&repo, &wt).arg("--total"), "operator total ceiling");
}

/// Claim: raising a gate cannot break the integration total, and a higher total
/// requires an explicit atomic request. Rejects a gate record implicitly authorizing
/// its neighboring total declaration or ignoring the effective declaration sum.
#[test]
fn raise_gate_and_total_transitions_are_explicit_and_atomic() {
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, 10000));
    let (id, wt, _) = change_with_patchset(&repo, "both-raises");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(&wt, "src/module.rs", &lines_text(100), "test: module");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &declaration(10400, 10000),
        "test: larger gate",
    );
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    refusal(&mut raise(&repo, &wt), "integration sum");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &declaration(10400, 10400),
        "test: larger total",
    );
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    refusal(&mut raise(&repo, &wt), "explicit --total");
    raise(&repo, &wt).arg("--total").assert().success();
    snapshot(&repo, &wt);
    approve(&repo, &wt);
    let st = shown(&repo, &wt, &id);
    assert_eq!(
        st["budget_raises"][0]["raise"]["increases"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
}

/// Claim: modified renames earn only net growth, overlapping paths count once,
/// and gates without paths count non-inert files. Rejects gross rename additions,
/// duplicate pattern credit and an empty implicit scope.
#[test]
fn raise_logical_growth_and_implicit_scope_are_exact() {
    for mode in ["modified-rename", "overlap", "no-paths"] {
        let repo = Repo::new();
        fs::create_dir_all(repo.root.join("src")).unwrap();
        repo.commit(
            &repo.root,
            "src/original.rs",
            &lines_text(1000),
            "test: original module",
        );
        let transform = |text: String| match mode {
            "overlap" => text.replace("paths=['src/']", "paths=['src/','src/*.rs']"),
            "no-paths" => text.replace("paths=['src/']\n", ""),
            _ => text,
        };
        repo.redeclare_gates(&transform(declaration(10000, 20000)));
        let id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", mode])));
        let wt = repo.home.join(".worktrees").join(format!("repo-{mode}"));
        if mode == "modified-rename" {
            git(&wt, &["mv", "src/original.rs", "src/moved.rs"]);
            repo.commit(
                &wt,
                "src/moved.rs",
                &lines_text(1100),
                "test: grow moved module",
            );
        } else {
            repo.commit(
                &wt,
                if mode == "no-paths" {
                    "outside.rs"
                } else {
                    "src/added.rs"
                },
                &lines_text(100),
                "test: module",
            );
        }
        repo.commit(
            &wt,
            ".arc/gates.toml",
            &transform(declaration(10400, 20000)),
            "test: raise declaration",
        );
        snapshot(&repo, &wt);
        verify(&repo, &wt, 10300, 1);
        raise(&repo, &wt).assert().success();
        let state = shown(&repo, &wt, &id);
        let increase = &state["budget_raises"][0]["raise"]["increases"][0];
        assert_eq!(increase["added"], 100, "{mode}: {increase}");
        assert_eq!(increase["files"].as_array().unwrap().len(), 1);
    }
}

/// Claim: head scope edits, cross-scope moves, deletions and transient commit
/// padding cannot earn allowance. Rejects counting head-controlled scope or commit
/// churn in place of the trusted final target-to-head delta.
#[test]
fn raise_scope_and_final_delta_cannot_be_self_enlarged() {
    let mut accepted = Vec::new();
    for mode in ["paths", "inert", "cross-scope", "deletion", "transient"] {
        let repo = Repo::new();
        fs::create_dir_all(repo.root.join("src/inert")).unwrap();
        repo.commit(
            &repo.root,
            "outside.rs",
            &lines_text(1000),
            "test: existing module",
        );
        let base =
            declaration(10000, 20000).replace("inert=['.arc/']", "inert=['.arc/','src/inert/']");
        repo.redeclare_gates(&base);
        let id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", mode])));
        let wt = repo.home.join(".worktrees").join(format!("repo-{mode}"));
        fs::create_dir_all(wt.join("src/inert")).unwrap();
        let mut own = base.replace("budget='10000ms'", "budget='10001ms'");
        match mode {
            "paths" => {
                own = own.replace("paths=['src/']", "paths=['**']");
                repo.commit(
                    &wt,
                    "uncovered.rs",
                    &lines_text(500),
                    "test: uncovered padding",
                );
            }
            "inert" => {
                own = own.replace("inert=['.arc/','src/inert/']", "inert=['.arc/']");
                repo.commit(
                    &wt,
                    "src/inert/padding.rs",
                    &lines_text(500),
                    "test: inert padding",
                );
            }
            "cross-scope" => {
                git(&wt, &["mv", "outside.rs", "src/moved.rs"]);
                repo.commit(
                    &wt,
                    "src/moved.rs",
                    &lines_text(1100),
                    "test: move into scope",
                );
            }
            "deletion" => {
                git(&wt, &["rm", "outside.rs"]);
                git(&wt, &["commit", "-m", "test: delete module"]);
            }
            _ => {
                repo.commit(
                    &wt,
                    "src/transient.rs",
                    &lines_text(500),
                    "test: transient module",
                );
                git(&wt, &["rm", "src/transient.rs"]);
                git(&wt, &["commit", "-m", "test: remove transient module"]);
            }
        }
        repo.commit(&wt, ".arc/gates.toml", &own, "test: proposed increase");
        snapshot(&repo, &wt);
        verify(&repo, &wt, 10001, 1);
        let result = raise(&repo, &wt).output().unwrap();
        if result.status.success() {
            accepted.push(mode);
        } else {
            let text = String::from_utf8_lossy(&result.stderr);
            assert!(text.contains("0 eligible lines"), "{mode}: {text}");
        }
        let _ = id;
    }
    assert!(
        accepted.is_empty(),
        "false credit accepted for {accepted:?}"
    );
}

/// Claim: a run used for a raise matches the complete effective suite, including
/// a newly declared gate. Rejects comparing its coverage identity with the bare
/// destination suite and refusing a genuine over-budget execution.
#[test]
fn raise_execution_matches_the_effective_added_gate_context() {
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, 10000));
    let (_, wt, _) = change_with_patchset(&repo, "added-context");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(&wt, "src/module.rs", &lines_text(200), "test: module");
    let own = format!(
        "{}[gates.extra]\ncommand='true'\nbudget='400ms'\npaths=['extra/']\n",
        declaration(10400, 10800)
    );
    repo.commit(&wt, ".arc/gates.toml", &own, "test: declarations");
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .args(["verify", "--gate", "extra", "--rerun"])
        .assert()
        .success();
    raise(&repo, &wt).arg("--total").assert().success();
}

/// Claim: a stacked member cannot spend an unintegrated prerequisite's growth.
/// Rejects treating all content above the target as this member's independent
/// size allowance when its declared base has not reached that destination.
#[test]
fn raise_stacked_members_cannot_pool_prerequisite_growth() {
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, 20000));
    stdout(repo.arc(&repo.root).args(["begin", "lower-credit"]));
    let lower = repo.home.join(".worktrees/repo-lower-credit");
    fs::create_dir_all(lower.join("src")).unwrap();
    repo.commit(
        &lower,
        "src/module.rs",
        &lines_text(500),
        "test: prerequisite module",
    );
    let base = repo.head(&lower);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "upper-credit", "--base", &base]),
    );
    let upper = repo.home.join(".worktrees/repo-upper-credit");
    repo.commit(
        &upper,
        ".arc/gates.toml",
        &declaration(12000, 20000),
        "test: upper increase",
    );
    snapshot(&repo, &upper);
    verify(&repo, &upper, 11500, 1);
    refusal(
        &mut raise(&repo, &upper),
        "destination to contain the change base",
    );
}

/// Claim: a candidate that restores the destination budget can repair an unrecorded
/// increase at the current change head. Rejects judging the candidate transition by
/// the superseded head's higher declaration.
#[test]
fn raise_candidate_can_repair_an_unrecorded_head_increase() {
    let (repo, id, wt) = setup("repair-budget", 10400, 100);
    register(&repo, &id, &wt, "raised-candidate");
    let tree = git_out(&repo.root, &["rev-parse", "HEAD^{tree}"]);
    repo.arc(&wt)
        .args([
            "candidate",
            "register",
            "--tree",
            &tree,
            "--brief",
            &id,
            "--producer",
            "tester",
            "--id",
            "repair-candidate",
        ])
        .assert()
        .success();
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .args(["candidate", "verify", "repair-candidate"])
        .assert()
        .success();
}

/// Claim: the first over-budget refusal gives this change's exact allowance and
/// maximum after the ordered work list. Rejects advice that needs an already
/// appended receipt or suggests compounding from a previously raised number.
#[test]
fn raise_first_refusal_has_the_exact_change_allowance() {
    let (repo, _, wt) = setup("first-advice", 10400, 100);
    repo.arc(&wt).env("ARC_TEST_COMMAND_RUN_MS","10300").args(["verify","--all","--rerun"]).assert().code(1)
        .stdout(predicates::str::contains("inspect arc gates over, make it faster, remove checks worth less, then arc gates raise GATE; allowance 0.4s; maximum 10.4s"));
}

/// Claim: an identical raise request leaves the immutable record set unchanged.
/// Rejects duplicate events needlessly invalidating the snapshot and its review
/// when head, policy, execution, reason and counted growth did not change.
#[test]
fn raise_repeating_identical_inputs_keeps_the_record_set() {
    let (repo, id, wt) = setup("repeat-raise", 10400, 100);
    verify(&repo, &wt, 10300, 1);
    raise(&repo, &wt).assert().success();
    let before = shown(&repo, &wt, &id)["budget_raises"].clone();
    raise(&repo, &wt)
        .assert()
        .success()
        .stdout(predicates::str::contains("unchanged"));
    assert_eq!(shown(&repo, &wt, &id)["budget_raises"], before);
}

/// Claim: an exported raise advertises the adopted store format so an older
/// importer refuses the evidence. Rejects exporting the ordinary event envelope
/// number as the compatibility barrier for budget-raise records.
#[test]
fn raise_export_advertises_the_adopted_store_format() {
    let (repo, id, wt) = setup("wire-barrier", 10400, 100);
    verify(&repo, &wt, 10300, 1);
    raise(&repo, &wt).assert().success();
    snapshot(&repo, &wt);
    let file = repo.home.join("wire.json");
    repo.arc(&wt)
        .args(["export", &id, "--output", file.to_str().unwrap()])
        .assert()
        .success();
    let value: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(value["store_format"], 9);
    let receiver = Repo::new();
    deliver_git_objects(&repo, &receiver);
    receiver
        .arc(&receiver.root)
        .args(["import", file.to_str().unwrap()])
        .assert()
        .success();
}

/// Claim: Git-marked binary text earns no allowance. Rejects accepting ASCII
/// padding solely because its bytes are UTF-8 and contain no NUL.
#[test]
fn raise_padding_binary_attributes() {
    padding("binary-attributes");
}

/// Claim: operator tightening during the environment probe refuses raise admission.
/// Rejects appending a proposal the shared judge already found inapplicable.
#[test]
fn raise_policy_tightening_during_probe_refuses_admission() {
    let repo = Repo::new();
    let probe = r#"task_common=$(git rev-parse --path-format=absolute --git-common-dir)
if test -f "$task_common/arc/tighten-raise"; then
  printf '%s\n' '[budget]' 'raise="0%"' > "$task_common/arc/operator-policy.toml"
  rm -- "$task_common/arc/tighten-raise"
fi
printf fixture"#;
    let with_probe = |budget| {
        format!(
            "{}environment={}\n",
            declaration(budget, 20000),
            toml::Value::String(probe.into())
        )
    };
    repo.redeclare_gates(&with_probe(10000));
    let (id, wt, _) = change_with_patchset(&repo, "tighten-during-probe");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(&wt, "src/module.rs", &lines_text(100), "test: module");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &with_probe(10400),
        "test: higher declaration",
    );
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    fs::write(repo.root.join(".git/arc/tighten-raise"), "").unwrap();
    refusal(
        &mut raise(&repo, &wt),
        "budget raise basis changed while counting",
    );
    assert!(shown(&repo, &wt, &id)["budget_raises"]
        .as_array()
        .unwrap()
        .is_empty());
}

fn scope_net_growth(mode: &str) {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join("src")).unwrap();
    repo.commit(
        &repo.root,
        "src/original.rs",
        &lines_text(1000),
        "test: source",
    );
    let total = if mode == "total" { 10000 } else { 20000 };
    let base = declaration(10000, total).replace("inert=['.arc/']", "inert=['.arc/','src/inert/']");
    repo.redeclare_gates(&base);
    repo.declare_gates_locally("[budget]\nexclude=['src/excluded/']\n");
    let ignored = match mode {
        "outside" => Some("outside.rs"),
        "inert" => Some("src/inert/deleted.rs"),
        "excluded" => Some("src/excluded/deleted.rs"),
        _ => None,
    };
    if !matches!(mode, "move" | "split") {
        repo.commit(
            &repo.root,
            "src/shrink.rs",
            &lines_text(300).replace("value", "shrinking"),
            "test: shrinking file",
        );
    }
    if let Some(path) = ignored {
        fs::create_dir_all(repo.root.join(path).parent().unwrap()).unwrap();
        repo.commit(
            &repo.root,
            path,
            &lines_text(700).replace("value", "ignored"),
            "test: ignored source",
        );
    }
    let id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", mode])));
    let wt = repo.home.join(".worktrees").join(format!("repo-{mode}"));
    let content = lines_text(1000);
    let lines = content.lines().collect::<Vec<_>>();
    let expected = if matches!(mode, "move" | "split") {
        0
    } else {
        200
    };
    match mode {
        "move" => {
            fs::write(
                wt.join("src/original.rs"),
                format!("{}\n", lines[..600].join("\n")),
            )
            .unwrap();
            fs::write(
                wt.join("src/moved.rs"),
                format!("{}\n", lines[600..].join("\n")),
            )
            .unwrap();
        }
        "split" => {
            git(&wt, &["rm", "src/original.rs"]);
            fs::create_dir_all(wt.join("src")).unwrap();
            for (index, chunk) in lines.chunks(250).enumerate() {
                fs::write(
                    wt.join(format!("src/part-{index}.rs")),
                    format!("{}\n", chunk.join("\n")),
                )
                .unwrap();
            }
        }
        _ => {
            if mode == "outbound" {
                git(&wt, &["mv", "src/shrink.rs", "outside-moved.rs"]);
            } else {
                git(&wt, &["rm", "src/shrink.rs"]);
            }
            fs::write(
                wt.join("src/added.rs"),
                lines_text(500).replace("value", "added"),
            )
            .unwrap();
            if let Some(path) = ignored {
                git(&wt, &["rm", path]);
            }
        }
    }
    let new_budget = if expected == 0 { 10001 } else { 10800 };
    let new_total = if mode == "total" { new_budget } else { total };
    let mut own = declaration(new_budget, new_total)
        .replace("inert=['.arc/']", "inert=['.arc/','src/inert/']");
    if mode == "head-inert" {
        own = own.replace(
            "inert=['.arc/','src/inert/']",
            "inert=['.arc/','src/inert/','src/shrink.rs']",
        );
    }
    if mode == "head-paths" {
        own = own.replace("paths=['src/']", "paths=['src/added.rs']");
    }
    fs::write(wt.join(".arc/gates.toml"), own).unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "test: scope delta"]);
    snapshot(&repo, &wt);
    verify(&repo, &wt, if expected == 0 { 10001 } else { 10300 }, 1);
    if expected == 0 {
        refusal(&mut raise(&repo, &wt), "0 eligible lines");
    } else {
        let mut command = raise(&repo, &wt);
        if mode == "total" {
            command.arg("--total");
        }
        command.assert().success();
        let state = shown(&repo, &wt, &id);
        for increase in state["budget_raises"][0]["raise"]["increases"]
            .as_array()
            .unwrap()
        {
            assert_eq!(increase["added"], expected, "{mode}: {increase}");
            assert_eq!(increase["allowance_ms"], 800, "{mode}: {increase}");
            let files = increase["files"].as_array().unwrap();
            assert_eq!(files.len(), 2, "{mode}: {files:?}");
            let shrink = files.iter().find(|f| f["path"] == "src/shrink.rs").unwrap();
            assert_eq!(shrink["before"], 300);
            assert_eq!(shrink["after"], 0);
            assert_eq!(shrink["added"], 0);
        }
    }
}

/// Claim: moving 400 lines between counted files earns zero allowance.
/// Rejects destination growth without the matching source shrinkage.
#[test]
fn raise_scope_net_move() {
    scope_net_growth("move");
}

/// Claim: splitting one counted file into smaller files earns zero allowance.
/// Rejects additions below Git's rename threshold without the deleted source.
#[test]
fn raise_scope_net_split() {
    scope_net_growth("split");
}

/// Claim: adding 500 counted lines and deleting 300 earns 200 lines of credit.
/// Rejects positive-only growth and omission of shrinking files from the record.
#[test]
fn raise_scope_net_add_and_delete() {
    scope_net_growth("net");
}

/// Claim: gate and total raises both count 500 additions minus 300 deletions.
/// Rejects either subject retaining positive-only growth.
#[test]
fn raise_scope_net_total() {
    scope_net_growth("total");
}

/// Claim: out-of-path deletion cannot debit the gate's net 200 eligible lines.
/// Rejects positive-only counting or subtracting shrinkage outside the gate scope.
#[test]
fn raise_scope_net_outside_deletion() {
    scope_net_growth("outside");
}

/// Claim: inert deletion cannot debit the gate's net 200 eligible lines.
/// Rejects positive-only counting or subtracting inert shrinkage.
#[test]
fn raise_scope_net_inert_deletion() {
    scope_net_growth("inert");
}

/// Claim: trusted excluded deletion cannot debit the gate's net 200 eligible lines.
/// Rejects positive-only counting or subtracting generated/vendor shrinkage.
#[test]
fn raise_scope_net_excluded_deletion() {
    scope_net_growth("excluded");
}

fn removal_rules(route: &str, accepted: bool) {
    let mut mismatches = Vec::new();
    for mode in [
        "remove-gate",
        "remove-budget",
        "remove-total",
        "remove-table",
        "remove-all",
        "lower-gate",
        "lower-total",
    ] {
        let allowed = !matches!(mode, "remove-budget" | "remove-total" | "remove-table");
        if allowed != accepted {
            continue;
        }
        let repo = Repo::new();
        let base = declaration(10000, 20000);
        repo.redeclare_gates(&base);
        let (id, wt, _) = change_with_patchset(&repo, mode);
        let own = match mode {
            "remove-gate" => base.split("[gates.unit]").next().unwrap().to_string(),
            "remove-budget" => base.replace("budget='10000ms'\n", ""),
            "remove-total" => base.replace("total='20000ms'\n", ""),
            "remove-table" => base.replace("[budget]\ntotal='20000ms'\n", ""),
            "remove-all" => "[selection]\ninert=['.arc/']\n".into(),
            "lower-gate" => declaration(9000, 20000),
            _ => declaration(10000, 15000),
        };
        repo.commit(&wt, ".arc/gates.toml", &own, "test: declaration decision");
        snapshot(&repo, &wt);
        approve(&repo, &wt);
        verify(&repo, &wt, 8000, 0);
        let expected = if mode == "remove-budget" {
            "removes its budget; restore the budget or remove the gate"
        } else {
            "cannot remove [budget] total while budgeted gates remain"
        };
        let status = json_stdout(repo.arc(&wt).args(["status", &id]));
        let errors = status["budget_errors"].as_array().unwrap();
        if allowed {
            if !errors.is_empty() {
                mismatches.push(format!("{mode} status blocked: {errors:?}"));
            }
            assert_eq!(
                status["gates"][0]["budget"]["budget_ms"],
                if mode == "lower-gate" { 9000 } else { 10000 }
            );
            assert!(status["budget_raises"].as_array().unwrap().is_empty());
        } else {
            let text = errors
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            if !text.contains(expected) || text.contains("arc gates raise") {
                mismatches.push(format!("{mode} diagnostic: {text}"));
            }
        }
        let mut observe = |label: &str, command: &mut AssertCommand| {
            let output = command.output().unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if output.status.success() != allowed || (!allowed && !text.contains(expected)) {
                mismatches.push(format!("{mode} {label}: {}: {text}", output.status));
            }
        };
        let before = repo.head(&repo.root);
        match route {
            "integrate" => {
                observe("check", repo.arc(&wt).args(["check", &id]));
                observe("integrate", repo.arc(&repo.root).args(["integrate", &id]));
                if allowed && repo.head(&repo.root) != before {
                    let committed = git_out(&repo.root, &["show", "HEAD:.arc/gates.toml"]);
                    assert_eq!(committed.trim(), own.trim());
                }
            }
            "land" => {
                let (other, _, _) = change_with_patchset(&repo, "other-removal-member");
                observe(
                    "land",
                    repo.arc(&repo.root)
                        .env("ARC_TEST_COMMAND_RUN_MS", "8000")
                        .args(["land", &id, &other, "--debt", "combined review owed"]),
                );
            }
            "candidate" => {
                register(&repo, &id, &wt, "removal-candidate");
                observe(
                    "candidate verify",
                    repo.arc(&wt).env("ARC_TEST_COMMAND_RUN_MS", "8000").args([
                        "candidate",
                        "verify",
                        "removal-candidate",
                    ]),
                );
                let evaluation = fs::read_dir(repo.root.join(".git/arc/repository/events"))
                    .unwrap()
                    .map(|e| {
                        serde_json::from_slice::<Value>(&fs::read(e.unwrap().path()).unwrap())
                            .unwrap()
                    })
                    .find(|e| e["event_type"] == "candidate-verified")
                    .map(|e| e["event_id"].as_str().unwrap().to_string());
                let mut selection = repo.arc(&wt);
                selection.args([
                    "candidate",
                    "select",
                    "--chosen",
                    "removal-candidate",
                    "--into",
                    &id,
                    "--target",
                    &before,
                    "--rationale",
                    "review the candidate",
                ]);
                if let Some(evaluation) = evaluation {
                    selection.args(["--evaluation", &evaluation]);
                }
                observe("candidate select and promote", &mut selection);
            }
            _ => {
                let branch = git_out(&wt, &["branch", "--show-current"]);
                for cached in [false, true] {
                    let mut command = repo.arc(&wt);
                    command.env("ARC_TEST_COMMAND_RUN_MS", "8000").args([
                        "publication",
                        "--target",
                        &branch,
                        "--change",
                        &id,
                    ]);
                    if cached {
                        command.arg("--skip-green");
                    }
                    observe("publication", &mut command);
                }
            }
        }
        if !allowed || matches!(route, "candidate" | "publication") {
            assert_eq!(repo.head(&repo.root), before);
        }
    }
    assert!(mismatches.is_empty(), "{route}: {mismatches:#?}");
}

/// Claim: removing gates, removing all budgeted gates with the table, and lowering
/// bounds need no raise; the change still owes target gates. Rejects blocking
/// these declaration diffs or dropping the target obligation before integration.
#[test]
fn raise_removal_accepts_check_and_integrate() {
    removal_rules("integrate", true);
}

/// Claim: a kept gate cannot lose its budget and a retained budgeted suite cannot
/// lose its total. Rejects accepting either removal or directing it to raise.
#[test]
fn raise_removal_refuses_check_and_integrate() {
    removal_rules("integrate", false);
}

/// Claim: landing permits gate removal and lower bounds under target obligations.
/// Rejects treating removal of a whole gate as an unauthorized increase.
#[test]
fn raise_removal_accepts_land() {
    removal_rules("land", true);
}

/// Claim: landing refuses missing bounds on retained declarations with specific
/// remedies. Rejects generic raise advice or a combined landing bypass.
#[test]
fn raise_removal_refuses_land() {
    removal_rules("land", false);
}

/// Claim: candidate execution and promotion permit gate removal and lower bounds.
/// Rejects blocking these diffs or dropping the target obligation.
#[test]
fn raise_removal_accepts_candidates() {
    removal_rules("candidate", true);
}

/// Claim: candidate execution and selection refuse removing retained bounds.
/// Rejects a candidate bypass or misleading raise-record diagnostics.
#[test]
fn raise_removal_refuses_candidates() {
    removal_rules("candidate", false);
}

/// Claim: fresh and cached publication permit gate removal and lower bounds
/// against the associated destination. Rejects treating these as raises.
#[test]
fn raise_removal_accepts_publication() {
    removal_rules("publication", true);
}

/// Claim: fresh and cached publication refuse losing retained bounds with specific
/// remedies. Rejects a publication bypass or misleading raise-record diagnostics.
#[test]
fn raise_removal_refuses_publication() {
    removal_rules("publication", false);
}

/// Claim: an operator-only total caps the effective suite after a gate raise.
/// Rejects treating absent project totals as absence of the operator ceiling.
#[test]
fn raise_operator_total_without_project_total() {
    let repo = Repo::new();
    let without_total =
        |budget| declaration(budget, 20000).replace("[budget]\ntotal='20000ms'\n", "");
    repo.redeclare_gates(&without_total(10000));
    repo.declare_gates_locally("[budget]\ntotal='10399ms'\n");
    let (id, wt, _) = change_with_patchset(&repo, "operator-only-total");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(&wt, "src/module.rs", &lines_text(100), "test: module");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &without_total(10400),
        "test: higher gate",
    );
    snapshot(&repo, &wt);
    verify(&repo, &wt, 10300, 1);
    refusal(
        &mut raise(&repo, &wt),
        "integration sum 10.4s exceeds total 10.399s",
    );
    assert!(shown(&repo, &wt, &id)["budget_raises"]
        .as_array()
        .unwrap()
        .is_empty());
}

/// Claim: head inert exclusions cannot hide trusted shrinkage to enlarge a raise.
/// Rejects turning a head-controlled exclusion into extra net credit.
#[test]
fn raise_scope_net_head_inert_preserves_debits() {
    scope_net_growth("head-inert");
}

/// Claim: head path narrowing cannot hide trusted shrinkage to enlarge a raise.
/// Rejects granting more net credit by excluding the shrinking source.
#[test]
fn raise_scope_net_head_paths_preserve_debits() {
    scope_net_growth("head-paths");
}

/// Claim: a file moved out of counted scope still debits its counted source.
/// Rejects ignoring the source loss beside eligible additions.
#[test]
fn raise_scope_net_outbound_move_debits_source() {
    scope_net_growth("outbound");
}
