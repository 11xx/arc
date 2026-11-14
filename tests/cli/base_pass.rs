use crate::common::*;
use serde_json::Value;

const GATES: &str = "[selection]\nbase_passes=true\ninert=['**/*.md']\n[gates.unit]\ncommand='true'\npaths=['src/']\nevents=['integration']\n[gates.other]\ncommand='true'\npaths=['other/']\nevents=['integration']\n";

fn begin(repo: &Repo, slug: &str, file: &str) -> (String, PathBuf) {
    begin_profile(repo, slug, file, "local")
}
fn begin_profile(repo: &Repo, slug: &str, file: &str, profile: &str) -> (String, PathBuf) {
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", slug, "--profile", profile]),
    );
    let id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_owned();
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    fs::create_dir_all(wt.join(file).parent().unwrap()).unwrap();
    repo.commit(&wt, file, "content\n", "test: content");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    (id, wt)
}

fn seed(text: &str) -> Repo {
    let repo = Repo::new();
    repo.redeclare_gates(text);
    warm(&repo);
    repo
}

fn warm(repo: &Repo) {
    let (id, wt) = begin(repo, "warm", "src/code.rs");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
}

fn state(repo: &Repo, wt: &Path) -> Value {
    json_stdout(repo.arc(wt).args(["show", "--json"]))
}
fn bases(state: &Value) -> usize {
    state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["base_pass"].is_object())
        .count()
}
fn executed(state: &Value) -> usize {
    state["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| !entry["base_pass"].is_object() && !entry["delta"].is_object())
        .count()
}

/// Claim: an opted-in, warmed documentation change integrates with zero gate
/// executions and exposes its source in every explanation. Rejects a cold run
/// disguised as reuse, or a reuse accepted without a complete proof.
#[test]
fn warmed_documentation_integrates_without_execution() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "docs", "docs/notes.md");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "100")
        .args(["verify", "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("base pass from"));
    let s = state(&repo, &wt);
    assert_eq!(
        bases(&s),
        2,
        "both gates must carry base-pass receipts: {s}"
    );
    assert_eq!(executed(&s), 0, "documentation must execute no gate: {s}");
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    for args in [
        vec!["status"],
        vec!["check"],
        vec!["show"],
        vec!["explain", &id],
    ] {
        repo.arc(&wt)
            .args(args)
            .assert()
            .success()
            .stdout(predicates::str::is_match("base pass|base_pass").unwrap());
    }
    let report = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert!(
        report["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|gate| gate["history"]["base_passes"] == 1),
        "{report}"
    );
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let s = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    assert_eq!(s["closure"]["outcome"], "integrated");
}

/// Claim: adoption and --rerun execute the entire required suite. Rejects a
/// change enabling base passes for itself or --rerun keeping a base pass.
#[test]
fn cold_adoption_and_explicit_rerun_execute() {
    let repo = Repo::new();
    repo.redeclare_gates(GATES);
    let (_, cold) = begin(&repo, "cold", "notes.md");
    repo.arc(&cold).args(["verify", "--all"]).assert().success();
    assert_eq!(executed(&state(&repo, &cold)), 2);
    let repo = seed(GATES);
    let (_, wt) = begin(&repo, "rerun", "notes.md");
    repo.arc(&wt)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    assert_eq!(bases(&state(&repo, &wt)), 0);
    assert_eq!(executed(&state(&repo, &wt)), 2);
}

/// Claim: unnamed paths and selection configuration execute every gate even
/// when an inert rule overlaps them. Rejects a declaration waiving itself.
#[test]
fn unnamed_and_selection_inputs_execute_every_gate() {
    for (slug, file, declaration) in [
        ("unknown", "unknown.xyz", GATES.to_owned()),
        (
            "config",
            ".arc/helper.md",
            GATES.replace("['**/*.md']", "['**/*.md','.arc/']"),
        ),
        ("coverage", ".arc/gates.toml", GATES.to_owned()),
    ] {
        let repo = seed(&declaration);
        let (_, wt) = begin(
            &repo,
            slug,
            if file == ".arc/gates.toml" {
                "notes.md"
            } else {
                file
            },
        );
        if file == ".arc/gates.toml" {
            repo.commit(
                &wt,
                file,
                &format!("{declaration}\n# selection input\n"),
                "test: selection",
            );
        }
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        assert_eq!(bases(&state(&repo, &wt)), 0, "{slug}");
        assert_eq!(executed(&state(&repo, &wt)), 2, "{slug}");
    }
}

/// Claim: publication audits integration-only gates used by a base pass,
/// even with --skip-green. Rejects an empty publication tier escaping the audit.
#[test]
fn publication_runs_every_participating_gate() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "publish", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(bases(&state(&repo, &wt)), 2);
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let baseline_count = baseline_files(&repo).len();
    repo.arc(&repo.root)
        .args([
            "publication",
            "--target",
            "master",
            "--profile",
            "local",
            "--skip-green",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("publication gate unit: pass"))
        .stdout(predicates::str::contains("publication gate other: pass"));
    assert_eq!(baseline_files(&repo).len(), baseline_count);
}

fn event_files(repo: &Repo, id: &str) -> Vec<PathBuf> {
    let mut files = fs::read_dir(repo.root.join(".git/arc/changes").join(id).join("events"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    files
}
fn baseline_files(repo: &Repo) -> Vec<PathBuf> {
    let mut files = fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["event_type"]
                == "gate-baseline-recorded"
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Claim: a baseline outside the head's first-parent history or outside the
/// actual target history is unavailable. Rejects branch-name ancestry or a
/// force move relabelling a baseline as current content.
#[test]
fn nonancestor_and_force_moved_targets_miss() {
    for move_target in [false, true] {
        let repo = seed(GATES);
        let old = git_out(&repo.root, &["rev-parse", "master^1"]);
        if move_target {
            git(&repo.root, &["reset", "--hard", &old]);
        }
        let (_, wt) = begin(&repo, "nonancestor", "notes.md");
        if !move_target {
            git(&wt, &["rebase", "--onto", &old, "master"]);
        }
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        assert_eq!(bases(&state(&repo, &wt)), 0);
        assert_eq!(executed(&state(&repo, &wt)), 2);
    }
}

/// Claim: source event, profile, environment and commit binding remain binding.
/// Rejects inferring a profile/event from applicability or trusting a failed probe.
#[test]
fn profiles_events_environments_and_commit_bindings_do_not_cross() {
    for case in ["profile", "event", "environment", "probe-failed", "commit"] {
        let declaration = match case {
            "event" => GATES.replace("events=['integration']\n", ""),
            "environment" | "probe-failed" => GATES.replace("command='true'", "command='true'\nenvironment='printf %s \"${BASE_ENV:-base}\"; test \"${BASE_FAIL:-no}\" = no'"),
            "commit" => GATES.replace("paths=['src/']", "paths=['src/']\nbinds='commit'"),
            _ => GATES.to_owned(),
        };
        let repo = seed(&declaration);
        let (_, wt) = begin_profile(
            &repo,
            case,
            "notes.md",
            if case == "profile" { "direct" } else { "local" },
        );
        let mut verify = repo.arc(&wt);
        verify.args(["verify", "--all"]);
        match case {
            "event" => {
                verify.args(["--event", "publication"]);
            }
            "environment" => {
                verify.env("BASE_ENV", "different");
            }
            "probe-failed" => {
                verify.env("BASE_FAIL", "yes");
            }
            _ => {}
        }
        verify.assert().success();
        let s = state(&repo, &wt);
        assert_eq!(bases(&s), usize::from(case == "commit"), "{case}: {s}");
        assert_eq!(
            executed(&s),
            if case == "commit" { 1 } else { 2 },
            "{case}: {s}"
        );
    }
}

/// Claim: dangerous paths cannot be cleared by inert declarations or selectors.
/// Rejects a script's empty answer overriding the destination's danger policy.
#[test]
fn dangerous_inert_paths_execute_the_entire_suite() {
    let repo = Repo::new();
    repo.redeclare_gates(&GATES.replace("command='true'", "command='true'\nselector='exit 0'"));
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        "[danger]\npaths=['**/risk.md']\n",
        "test: danger",
    );
    warm(&repo);
    let (_, wt) = begin(&repo, "danger", "docs/risk.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(bases(&state(&repo, &wt)), 0);
    assert_eq!(executed(&state(&repo, &wt)), 2);
}

/// Claim: complete endpoint diffs include rename sources, deletions, mode and
/// type changes, gitlinks and unusual names. Rejects a rename-to-inert escape
/// or incomplete paths silently declaring a gate unaffected.
#[test]
fn structural_paths_and_unusual_names_are_complete() {
    for case in [
        "rename",
        "delete",
        "mode",
        "symlink",
        "gitlink",
        "newline",
        "invalid-utf8",
    ] {
        let repo = seed(GATES);
        let (_, wt) = begin(&repo, case, "notes.md");
        git(&wt, &["config", "diff.renames", "true"]);
        match case {
            "rename" => {
                fs::create_dir_all(wt.join("docs")).unwrap();
                git(&wt, &["mv", "src/code.rs", "docs/code.md"]);
            }
            "delete" => git(&wt, &["rm", "src/code.rs"]),
            "mode" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(wt.join("src/code.rs"), fs::Permissions::from_mode(0o755))
                    .unwrap();
                git(&wt, &["update-index", "--chmod=+x", "src/code.rs"]);
            }
            "symlink" => {
                fs::remove_file(wt.join("src/code.rs")).unwrap();
                std::os::unix::fs::symlink("../notes.md", wt.join("src/code.rs")).unwrap();
                git(&wt, &["add", "src/code.rs"]);
            }
            "gitlink" => {
                fs::create_dir_all(wt.join("src/submodule")).unwrap();
                let head = repo.head(&wt);
                git(
                    &wt,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{head},src/submodule"),
                    ],
                );
            }
            "newline" => {
                fs::write(wt.join("src/line\nbreak.rs"), "code").unwrap();
                git(&wt, &["add", "."]);
            }
            "invalid-utf8" => {
                use std::os::unix::ffi::OsStringExt;
                fs::write(
                    wt.join(std::ffi::OsString::from_vec(b"src/bad\xff.rs".to_vec())),
                    "code",
                )
                .unwrap();
                git(&wt, &["add", "."]);
            }
            _ => unreachable!(),
        }
        git(&wt, &["commit", "-m", "test: structural change"]);
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        let s = state(&repo, &wt);
        assert_eq!(
            bases(&s),
            usize::from(case != "invalid-utf8"),
            "{case}: {s}"
        );
        assert_eq!(
            executed(&s),
            if case == "invalid-utf8" { 2 } else { 1 },
            "{case}: {s}"
        );
    }
}

/// Claim: an over-budget newest entry executes once rather than using an older
/// faster pass. Rejects either cherry-picking the old pass or refusing without
/// executing a previously unobserved gate; later observations are not retried.
#[test]
fn budget_reduction_executes_without_an_older_faster_fallback() {
    let declaration = GATES.replace("command='true'", "command='true'\nbudget='10s'");
    let repo = Repo::new();
    repo.redeclare_gates(&declaration);
    for (slug, ms) in [("fast", "100"), ("slow", "9000")] {
        let (id, wt) = begin(&repo, slug, &format!("{slug}.md"));
        repo.arc(&wt)
            .env("ARC_TEST_COMMAND_RUN_MS", ms)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .success();
        integrate_base_change(&repo, &id, &wt);
    }
    repo.declare_gates_locally(
        "[gates.unit]\ncommand='true'\npaths=['src/']\nevents=['integration']\nbudget='1s'\n",
    );
    let (_, wt) = begin(&repo, "budget", "notes.md");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "1100")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    let s = state(&repo, &wt);
    assert_eq!(bases(&s), 1, "{s}");
    assert_eq!(executed(&s), 1, "{s}");
    repo.arc(&wt).args(["verify", "--all"]).assert().code(1);
    assert_eq!(executed(&state(&repo, &wt)), 1);
}

/// Claim: replay rejects a base pass whose recorded path list differs from
/// the retained Git diff, even when the frozen path classification itself
/// would permit reuse. Rejects trusting a recorded path list or digest
/// without checking retained Git objects.
#[test]
fn replay_refuses_a_fabricated_diff() {
    replay_refuses_fabricated(false);
}

/// Claim: replay rejects a base pass whose recorded revision does not descend
/// from the baseline by first parent. Rejects trusting a recorded revision or
/// ancestry claim without checking retained Git objects.
#[test]
fn replay_refuses_fabricated_ancestry() {
    replay_refuses_fabricated(true);
}

/// A change's base pass with its path list emptied, or with its revision
/// moved to an unrelated root of the same tree when `ancestry`, must fail
/// replay.
fn replay_refuses_fabricated(ancestry: bool) {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "fabricated", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let orphan = if ancestry {
        let tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
        Some(git_out(
            &wt,
            &["commit-tree", &tree, "-m", "test: unrelated root"],
        ))
    } else {
        None
    };
    for file in event_files(&repo, &id) {
        let mut event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        if event["event_type"] == "verification-run-started" {
            if let Some(orphan) = &orphan {
                event["revision"] = orphan.clone().into();
                fs::write(&file, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
            }
        }
        if event["event_type"] != "verification-base-reused" {
            continue;
        }
        if let Some(orphan) = &orphan {
            event["revision"] = orphan.clone().into();
            event["witness"]["revision"] = orphan.clone().into();
        } else {
            event["witness"]["changed_paths"] = serde_json::json!([]);
        }
        fs::write(file, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
    }
    repo.arc(&wt)
        .args(["show", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "Git diff or first-parent ancestry mismatch",
        ));
}

/// Claim: a reuse resolves authenticated compact facts without the producing
/// change's ledger. Rejects a hidden dependency on that ledger.
#[test]
fn reuse_replays_and_exports_without_the_source_change() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "portable", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let source = fs::read_dir(repo.root.join(".git/arc/changes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("warm-")
        })
        .unwrap();
    fs::remove_dir_all(source).unwrap();
    assert_eq!(bases(&state(&repo, &wt)), 2);
    let bundle = repo.home.join("portable.json");
    repo.arc(&wt)
        .args(["export", &id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let bundle: Value = serde_json::from_slice(&fs::read(bundle).unwrap()).unwrap();
    assert!(bundle["repository_events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["event_type"] == "gate-baseline-recorded"));
    assert!(bundle["objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|object| object["role"] == "retention-commit"));
}

/// Claim: missing entries and ineligible source receipts do not mint executions
/// in a new baseline. Rejects inherited passes, dirty waivers and unknown facts
/// becoming independent execution authority.
#[test]
fn reused_integration_records_no_fictitious_execution() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "derived", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let files = baseline_files(&repo);
    assert_eq!(files.len(), 2);
    let baseline: Value =
        serde_json::from_slice(&fs::read(files.last().unwrap()).unwrap()).unwrap();
    assert!(
        baseline["baseline"]["gates"]
            .as_object()
            .unwrap()
            .is_empty(),
        "{baseline}"
    );
    assert_eq!(baseline["baseline"]["revision"], repo.head(&repo.root));
}

/// Claim: a directly named command/probe input remains protected under inert
/// patterns. Rejects a script body changing while its declaration string and
/// all gate path mappings remain the same.
#[test]
fn executable_selection_inputs_cannot_be_inert() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join("scripts")).unwrap();
    repo.commit(&repo.root, "scripts/check.sh", "true\n", "test: gate input");
    repo.redeclare_gates(
        &GATES
            .replace("['**/*.md']", "['**/*.md','scripts/']")
            .replace("command='true'", "command='sh scripts/check.sh'"),
    );
    warm(&repo);
    let (_, wt) = begin(&repo, "input", "scripts/check.sh");
    repo.commit(
        &wt,
        "scripts/check.sh",
        "true\n# command input\n",
        "test: gate input change",
    );
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(bases(&state(&repo, &wt)), 0);
    assert_eq!(executed(&state(&repo, &wt)), 2);
}

/// Claim: malformed, duration-free, dirty, attested or mismatched baseline
/// execution facts cannot seed a new pass. Rejects provenance laundering.
#[test]
fn ineligible_baseline_execution_facts_miss() {
    for field in [
        "attested",
        "worktree_dirty",
        "tree_moved",
        "run_ms",
        "declaration",
        "profile",
        "event",
        "environment",
        "unknown-cleanliness",
        "over-budget",
        "bypass",
    ] {
        let repo = seed(&GATES.replace("command='true'", "command='true'\nbudget='1h'"));
        let file = baseline_files(&repo).pop().unwrap();
        let mut record: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        for entry in record["baseline"]["gates"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            match field {
                "attested" | "worktree_dirty" | "tree_moved" => {
                    entry["execution"][field] = true.into()
                }
                "unknown-cleanliness" => entry["execution"]["worktree_dirty"] = Value::Null,
                "over-budget" => entry["execution"]["run_ms"] = 3_600_001.into(),
                "bypass" => entry["execution"]["result"] = "bypassed".into(),
                "run_ms" => {
                    entry["execution"].as_object_mut().unwrap().remove("run_ms");
                }
                "declaration" => entry["execution"]["declaration"] = "sha256:wrong".into(),
                "profile" => entry["run"]["profile"] = "direct".into(),
                "event" => entry["run"]["event"] = "publication".into(),
                "environment" => {
                    entry["execution"]["environment"] =
                        serde_json::json!({"identity":"different","producing_store":"foreign"})
                }
                _ => unreachable!(),
            }
        }
        fs::write(file, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let (_, wt) = begin(&repo, &field.replace('_', "-"), "notes.md");
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        assert_eq!(bases(&state(&repo, &wt)), 0, "{field}");
        assert_eq!(executed(&state(&repo, &wt)), 2, "{field}");
    }
}

/// Claim: concurrent integrations expose only exact successful final revisions.
/// Rejects a baseline read after releasing the target guard or a baseline for
/// a refused transition. No timer determines which integration wins.
#[test]
fn racing_integrations_record_exact_successful_baselines() {
    let repo = seed(GATES);
    let mut ids = Vec::new();
    for slug in ["race-a", "race-b"] {
        let (id, wt) = begin(&repo, slug, &format!("{slug}.md"));
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        repo.arc(&wt)
            .args(["review", "--verdict", "approved"])
            .assert()
            .success();
        ids.push(id);
    }
    let a = repo.spawn_arc_piped(&repo.root, &["integrate", &ids[0]]);
    let b = repo.spawn_arc_piped(&repo.root, &["integrate", &ids[1]]);
    let outcomes = [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()];
    let successes = outcomes
        .iter()
        .filter(|output| output.status.success())
        .count();
    assert!(successes >= 1, "{outcomes:?}");
    let baselines = baseline_files(&repo);
    assert_eq!(baselines.len(), 1 + successes);
    for file in &baselines {
        let record: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        let baseline = &record["baseline"];
        assert_eq!(baseline["revision"], baseline["completion"]["revision"]);
        assert_eq!(
            baseline["tree"],
            git_out(
                &repo.root,
                &[
                    "rev-parse",
                    &format!("{}^{{tree}}", baseline["revision"].as_str().unwrap())
                ]
            )
        );
    }
    let last: Value =
        serde_json::from_slice(&fs::read(baselines.last().unwrap()).unwrap()).unwrap();
    assert_eq!(last["baseline"]["revision"], repo.head(&repo.root));
}

/// Claim: stacks include lower content, and stack/composed land mint one
/// final-tree baseline. Rejects gating only the top patch or manufacturing
/// passes for intermediate member revisions.
#[test]
fn both_landing_forms_record_only_the_final_tree() {
    for composed in [false, true] {
        let repo = seed(GATES);
        let (lower, lower_wt) = begin(&repo, "lower", "src/lower.rs");
        let lower_head = repo.head(&lower_wt);
        let (upper, upper_wt) = if composed {
            begin(&repo, "upper", "upper.md")
        } else {
            let out = stdout(
                repo.arc(&repo.root)
                    .args(["begin", "upper", "--base", &lower_head]),
            );
            let id = opened_change_id(&out);
            let wt = repo.home.join(".worktrees/repo-upper");
            repo.commit(&wt, "upper.md", "docs\n", "test: upper docs");
            repo.arc(&wt).args(["snapshot"]).assert().success();
            (id, wt)
        };
        if !composed {
            repo.arc(&upper_wt)
                .args(["verify", "--all"])
                .assert()
                .success();
            let s = state(&repo, &upper_wt);
            assert_eq!(bases(&s), 1, "lower source affects unit: {s}");
            assert_eq!(executed(&s), 1);
        }
        for wt in [&lower_wt, &upper_wt] {
            repo.arc(wt)
                .args(["review", "--verdict", "approved"])
                .assert()
                .success();
        }
        repo.arc(&repo.root)
            .args([
                "land",
                &lower,
                &upper,
                "--debt",
                "combined content awaits review",
            ])
            .assert()
            .success();
        let files = baseline_files(&repo);
        assert_eq!(files.len(), 2, "one baseline for the entire landing");
        let record: Value =
            serde_json::from_slice(&fs::read(files.last().unwrap()).unwrap()).unwrap();
        assert_eq!(
            record["baseline"]["completion"]["event_type"],
            "landing-completed"
        );
        assert_eq!(record["baseline"]["revision"], repo.head(&repo.root));
        assert_eq!(
            record["baseline"]["tree"],
            git_out(&repo.root, &["rev-parse", "HEAD^{tree}"])
        );
    }
}

/// Claim: a prepared or unapplied landing never publishes a baseline; recovery
/// may publish only the installed final subject. Rejects phantom baselines
/// surviving rollback, authorization-only state or interrupted application.
#[test]
fn landing_crash_and_recovery_do_not_publish_prepared_baselines() {
    for point in ["authorized", "moved", "applied"] {
        let repo = seed(GATES);
        let (lower, _) = begin(&repo, "lower", "lower.md");
        let (upper, _) = begin(&repo, "upper", "upper.md");
        let before = repo.head(&repo.root);
        let output = repo
            .arc(&repo.root)
            .env("ARC_TEST_LANDING_CRASH", point)
            .args([
                "land",
                &lower,
                &upper,
                "--debt",
                "combined content awaits review",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(86), "{point}: {output:?}");
        assert_eq!(
            baseline_files(&repo).len(),
            1,
            "{point}: no pre-completion baseline"
        );
        let out = String::from_utf8_lossy(&output.stdout);
        let attempt = out
            .lines()
            .find_map(|line| line.strip_prefix("landing: "))
            .unwrap();
        repo.arc(&repo.root)
            .args(["land", "--recover", attempt])
            .assert()
            .success();
        if point == "authorized" {
            assert_eq!(repo.head(&repo.root), before);
            assert_eq!(baseline_files(&repo).len(), 1);
        } else {
            let files = baseline_files(&repo);
            assert_eq!(files.len(), 2);
            let last: Value =
                serde_json::from_slice(&fs::read(files.last().unwrap()).unwrap()).unwrap();
            assert_eq!(last["baseline"]["revision"], repo.head(&repo.root));
        }
    }
}

/// Claim: named verification consumes a base pass and --rerun creates an actual
/// profiled execution usable by a later integration baseline. Rejects limiting
/// base passes to --all or presenting a named reuse as execution.
#[test]
fn named_verification_reuses_and_records_profiled_executions() {
    let repo = seed(GATES.split("[gates.other]").next().unwrap());
    let (id, wt) = begin(&repo, "named", "notes.md");
    repo.arc(&wt)
        .args(["verify", "--gate", "unit"])
        .assert()
        .success();
    assert_eq!(bases(&state(&repo, &wt)), 1);
    assert_eq!(executed(&state(&repo, &wt)), 0);
    repo.arc(&wt)
        .args(["verify", "--gate", "unit", "--rerun"])
        .assert()
        .success();
    assert_eq!(executed(&state(&repo, &wt)), 1);
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let file = baseline_files(&repo).pop().unwrap();
    let baseline: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    assert_eq!(
        baseline["baseline"]["gates"]["unit"]["run"]["profile"],
        "local"
    );
}

/// Claim: every frozen witness coordinate is checked at isolated replay,
/// including the originating ledger authority. Rejects treating an embedded
/// integration, a digest or a merge-target claim as its own authority, including
/// a later witness sharing the earlier witness's Git coordinates.
#[test]
fn replay_refuses_corrupted_witness_coordinates() {
    let repo = seed(GATES);
    for (index, field) in [
        "baseline-event",
        "digest",
        "algorithm",
        "gate",
        "tree",
        "merge-target",
        "later-merge-target",
        "target-revision",
        "origin",
        "source-profile",
        "source-command",
        "source-receipt",
        "source-context",
        "run",
        "run-command",
    ]
    .iter()
    .enumerate()
    {
        let (id, wt) = begin(
            &repo,
            &format!("coordinate-{index}"),
            &format!("notes-{index}.md"),
        );
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        let files = event_files(&repo, &id)
            .into_iter()
            .filter(|file| {
                serde_json::from_slice::<Value>(&fs::read(file).unwrap()).unwrap()["event_type"]
                    == "verification-base-reused"
            })
            .collect::<Vec<_>>();
        let file = if *field == "later-merge-target" {
            files.last()
        } else {
            files.first()
        }
        .unwrap()
        .clone();
        let original = fs::read(&file).unwrap();
        let mut event: Value = serde_json::from_slice(&original).unwrap();
        let mut restored_run = None;
        let baseline_file = baseline_files(&repo).pop().unwrap();
        let baseline_bytes = fs::read(&baseline_file).unwrap();
        let mut baseline: Value = serde_json::from_slice(&baseline_bytes).unwrap();
        match *field {
            "baseline-event" => event["witness"]["baseline_event_id"] = "missing".into(),
            "digest" => event["witness"]["baseline_digest"] = "sha256:bad".into(),
            "algorithm" => event["witness"]["algorithm"] = "arc-base-paths/0".into(),
            "gate" => event["witness"]["gate"] = "missing".into(),
            "tree" => event["witness"]["tree"] = baseline["baseline"]["tree"].clone(),
            "merge-target" | "later-merge-target" => {
                event["witness"]["against_target"] = baseline["baseline"]["revision"].clone()
            }
            "target-revision" => {
                event["witness"]["target_revision"] =
                    git_out(&repo.root, &["rev-parse", "master^1"]).into()
            }
            "origin" => event["repository_id"] = "foreign".into(),
            "source-profile" => baseline["baseline"]["profile"] = "direct".into(),
            "source-command" => {
                baseline["baseline"]["gates"]["unit"]["declaration"]["command"] = "false".into()
            }
            "source-receipt" => {
                baseline["baseline"]["gates"]["unit"]["execution"]["event_id"] = "forged".into()
            }
            "source-context" => {
                baseline["baseline"]["gates"]["unit"]["declaration"]["coverage"]["inert"] =
                    serde_json::json!(["**"])
            }
            "run" => event["run_id"] = "missing".into(),
            "run-command" => {
                let run_file = repo
                    .root
                    .join(".git/arc/changes")
                    .join(&id)
                    .join("events")
                    .join(format!("{}.json", event["run_id"].as_str().unwrap()));
                let bytes = fs::read(&run_file).unwrap();
                let mut run: Value = serde_json::from_slice(&bytes).unwrap();
                for declared in run["gates"].as_array_mut().unwrap() {
                    declared["command"] = "false".into();
                }
                fs::write(&run_file, serde_json::to_vec_pretty(&run).unwrap()).unwrap();
                restored_run = Some((run_file, bytes));
            }
            _ => unreachable!(),
        }
        fs::write(
            &baseline_file,
            serde_json::to_vec_pretty(&baseline).unwrap(),
        )
        .unwrap();
        fs::write(&file, serde_json::to_vec_pretty(&event).unwrap()).unwrap();
        let output = repo.arc(&wt).args(["show", "--json"]).output().unwrap();
        assert!(
            !output.status.success(),
            "{field} must refuse replay: {output:?}"
        );
        fs::write(file, original).unwrap();
        fs::write(baseline_file, baseline_bytes).unwrap();
        if let Some((run_file, bytes)) = restored_run {
            fs::write(run_file, bytes).unwrap();
        }
    }
}

/// Claim: standalone transport retains baseline authority separately from
/// verification facts and carries every Git witness object. Rejects facts-only
/// import minting integration authority, needing the producing change ledger,
/// or older importers silently skipping the advertised writer format.
#[test]
fn standalone_import_requires_baseline_integration_authority() {
    let source = seed(GATES);
    let (id, wt) = begin(&source, "transport", "notes.md");
    source.arc(&wt).args(["verify", "--all"]).assert().success();
    let input = source.home.join("transport.json");
    source
        .arc(&wt)
        .args(["export", &id, "--output", input.to_str().unwrap()])
        .assert()
        .success();
    let original: Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    assert_eq!(
        original["store_format"], 12,
        "exports must fence pre-S6 importers"
    );
    let origin = original["repository_id"].as_str().unwrap();
    for integration in [false, true] {
        let receiver = Repo::new();
        receiver
            .arc(&receiver.root)
            .args(["replica", "init", "authority"])
            .assert()
            .success();
        receiver
            .arc(&receiver.root)
            .args(["replica", "pair", "producer", "--repository-id", origin])
            .assert()
            .success();
        let replica = json_stdout(
            receiver
                .arc(&receiver.root)
                .args(["replica", "status", "--json"]),
        );
        let mut bundle = original.clone();
        bundle["project_id"] = replica["project_id"].clone();
        deliver_git_objects(&source, &receiver);
        git(
            &receiver.root,
            &["reset", "--hard", "refs/delivered/heads/master"],
        );
        git(
            &receiver.root,
            &[
                "branch",
                "arc/transport",
                "refs/delivered/heads/arc/transport",
            ],
        );
        let roles = if integration {
            "['facts','integration']"
        } else {
            "['facts']"
        };
        receiver.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles={roles}\nactors=['tester']\n"));
        fs::write(&input, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
        let digest = hex::encode(Sha256::digest(fs::read(&input).unwrap()));
        let output = receiver
            .arc(&receiver.root)
            .args([
                "import",
                input.to_str().unwrap(),
                "--publisher",
                "channel",
                "--received-sha256",
                &digest,
            ])
            .output()
            .unwrap();
        if integration {
            assert!(output.status.success(), "{output:?}");
            let state = json_stdout(receiver.arc(&receiver.root).args(["show", &id, "--json"]));
            assert_eq!(bases(&state), 2);
            let config: Value = serde_json::from_slice(
                &fs::read(receiver.root.join(".git/arc/config.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(config["schema_version"], 10);
        } else {
            assert!(
                !output.status.success(),
                "facts-only baseline must be refused"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Integration"),
                "{output:?}"
            );
        }
    }
}

/// Claim: candidates execute their required gates and promotion moves only the
/// change branch. Rejects consuming target base passes or minting a target
/// baseline during evaluation, selection or promotion.
#[test]
fn candidates_execute_and_never_record_a_target_baseline() {
    let repo = seed(GATES.split("[gates.other]").next().unwrap());
    repo.declare_gates_locally("[candidates]\nevaluation_reuse='matching-coordinates'\n");
    let (id, wt) = begin(&repo, "candidate", "notes.md");
    repo.arc(&wt).args(["claim", &id]).assert().success();
    repo.arc(&wt)
        .args(["brief", &id, "--body-file", "-"])
        .write_stdin("Evaluate the complete candidate.\n")
        .assert()
        .success();
    let tree = git_out(&wt, &["rev-parse", "HEAD^{tree}"]);
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
            "proposal",
        ])
        .assert()
        .success();
    let before = repo.head(&repo.root);
    let output = stdout(repo.arc(&wt).args(["candidate", "verify", "proposal"]));
    let evaluation = output
        .lines()
        .find_map(|line| line.strip_prefix("evaluation: "))
        .unwrap();
    assert!(output.contains("gate unit: pass"), "{output}");
    repo.arc(&wt)
        .args([
            "candidate",
            "select",
            "--chosen",
            "proposal",
            "--into",
            &id,
            "--target",
            &before,
            "--rationale",
            "complete candidate",
            "--evaluation",
            evaluation,
        ])
        .assert()
        .success();
    assert_eq!(repo.head(&repo.root), before);
    assert_eq!(baseline_files(&repo).len(), 1);
    let s = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    assert_eq!(bases(&s), 0);
    assert_eq!(
        executed(&s),
        0,
        "candidate checks remain separate from integration evidence"
    );
}

/// Claim: second-parent reachability does not satisfy the first-parent baseline
/// policy. Rejects replacing the declared ancestry boundary with merge-base
/// reachability even when the endpoint trees differ only in documentation.
#[test]
fn second_parent_baselines_execute_every_gate() {
    let repo = seed(GATES);
    let old = git_out(&repo.root, &["rev-parse", "master^1"]);
    let (_, wt) = begin(&repo, "second-parent", "notes.md");
    git(&wt, &["rebase", "--onto", &old, "master"]);
    git(
        &wt,
        &["merge", "--no-ff", "master", "-m", "test: second parent"],
    );
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(bases(&state(&repo, &wt)), 0);
    assert_eq!(executed(&state(&repo, &wt)), 2);
}

/// Claim: unavailable original execution objects cause a baseline miss and full
/// execution. Rejects replaying claimed execution coordinates from hashes alone.
#[test]
fn missing_execution_objects_cause_full_execution() {
    let repo = Repo::new();
    repo.redeclare_gates(GATES);
    let (id, wt) = begin(&repo, "warm", "src/code.rs");
    repo.arc(&wt)
        .args(["verify", "--against", "master"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let file = baseline_files(&repo).pop().unwrap();
    let record: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    let revision = record["baseline"]["gates"]["unit"]["execution"]["revision"]
        .as_str()
        .unwrap();
    assert_ne!(revision, repo.head(&repo.root));
    fs::remove_file(
        repo.root
            .join(".git/objects")
            .join(&revision[..2])
            .join(&revision[2..]),
    )
    .unwrap();
    let (_, wt) = begin(&repo, "missing-object", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(bases(&state(&repo, &wt)), 0);
    assert_eq!(executed(&state(&repo, &wt)), 2);
}

/// A change whose own run of every gate exited `exit`, verified again: its
/// later pass must come from executing the gates, never from a baseline.
fn local_refusal_executes_again(exit: &str) {
    let repo = seed(&GATES.replace("command='true'", "command='exit \"${ARC_GATE_RESULT:-0}\"'"));
    let (_, wt) = begin(&repo, &format!("refusal-{exit}"), "notes.md");
    repo.arc(&wt)
        .env("ARC_GATE_RESULT", exit)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let s = state(&repo, &wt);
    assert_eq!(
        bases(&s),
        0,
        "{exit}: a local refusal forbids baseline fallback"
    );
    assert_eq!(executed(&s), 4);
}

/// Claim: a change whose own gate run failed executes the gates again; a
/// baseline cannot answer them. Rejects fallback from a local failure to
/// another change's earlier successful execution.
#[test]
fn a_local_failure_cannot_be_rescued_by_a_baseline() {
    local_refusal_executes_again("1");
}

/// Claim: a change whose own gate run was not admitted executes the gates
/// again; a baseline cannot answer them. Rejects fallback from a not-admitted
/// attempt to another change's earlier successful execution.
#[test]
fn a_local_not_admitted_run_cannot_be_rescued_by_a_baseline() {
    local_refusal_executes_again("75");
}

/// Claim: provisional integration can consume already canonical evidence but
/// cannot publish a canonical target baseline. Rejects staging movement being
/// reported as an authoritative integration or as new gate execution.
#[test]
fn provisional_integration_consumes_evidence_without_a_baseline() {
    let worker = seed(GATES.split("[gates.other]").next().unwrap());
    let (id, wt) = begin(&worker, "staged-docs", "notes.md");
    worker.arc(&wt).args(["verify", "--all"]).assert().success();
    worker
        .arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    let worker_id = json_stdout(worker.arc(&worker.root).args(["replica", "id", "--json"]));
    let authority = Repo::new();
    deliver_git_objects(&worker, &authority);
    git(
        &authority.root,
        &["reset", "--hard", "refs/delivered/heads/master"],
    );
    authority
        .arc(&authority.root)
        .args(["replica", "init", "authority"])
        .assert()
        .success();
    authority
        .arc(&authority.root)
        .args([
            "replica",
            "pair",
            "worker",
            "--repository-id",
            worker_id["repository_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    let pairing = authority.home.join("pairing.json");
    authority
        .arc(&authority.root)
        .args(["replica", "export", "--output", pairing.to_str().unwrap()])
        .assert()
        .success();
    let bytes = fs::read(&pairing).unwrap();
    let pairing_value: Value = serde_json::from_slice(&bytes).unwrap();
    let origin = pairing_value["source_replica_id"].as_str().unwrap();
    worker.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['authority']\nactors=['tester']\n"));
    worker
        .arc(&worker.root)
        .args([
            "replica",
            "import",
            pairing.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &hex::encode(Sha256::digest(&bytes)),
        ])
        .assert()
        .success();
    let before = worker.head(&worker.root);
    worker
        .arc(&wt)
        .args(["integrate", &id, "--provisional"])
        .assert()
        .success();
    assert_eq!(worker.head(&worker.root), before);
    assert_eq!(baseline_files(&worker).len(), 1);
    let s = state(&worker, &wt);
    assert!(s["closure"].is_null());
    assert_eq!(s["provisional_integrations"].as_array().unwrap().len(), 1);
    assert_eq!(bases(&s), 1);
    assert_eq!(executed(&s), 0);
}

/// Claim: a verifier that captured empty state cannot publish a base pass after
/// another verifier records a refusal. Rejects a later reuse masking that actual
/// observation through a stale first-verification snapshot.
#[test]
fn concurrent_verifiers_cannot_rescue_a_later_local_failure() {
    use std::io::Write;
    let repo = seed(&GATES.replace("command='true'", "command='exit \"${ARC_GATE_RESULT:-0}\"'"));
    let (_, wt) = begin(&repo, "concurrent-verifiers", "notes.md");
    let template = repo.arc(&wt);
    let mut command = Command::new(template.get_program());
    command
        .args(["verify", "--all"])
        .current_dir(&wt)
        .env("ARC_TEST_IMPORT_RENDEZVOUS", "base-pass-before-append")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    let mut child = command.spawn().unwrap();
    let mut reader = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "arc test: import base-pass-before-append\n" {
            break;
        }
    }
    repo.arc(&wt)
        .env("ARC_GATE_RESULT", "1")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    child.stdin.take().unwrap().write_all(b"+").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stale verifier must refuse reuse: {output:?}"
    );
    let s = state(&repo, &wt);
    assert_eq!(bases(&s), 0);
    assert_eq!(executed(&s), 2);
    assert!(s["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["result"] == "fail"));
}

/// Claim: replay enforces the per-gate observation boundary even if an invalid reuse is
/// delivered directly. Rejects accepting a base pass after an independent local
/// observation of the same gate merely because its Git and coverage witness is valid.
#[test]
fn replay_refuses_base_pass_after_a_local_failure() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "later-observation", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let file = event_files(&repo, &id)
        .into_iter()
        .find(|file| {
            serde_json::from_slice::<Value>(&fs::read(file).unwrap()).unwrap()["event_type"]
                == "verification-base-reused"
        })
        .unwrap();
    let event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let later: Value = event_files(&repo, &id)
        .iter()
        .filter_map(|path| {
            let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            (value["event_type"] == "verification-base-reused"
                && value["event_id"] != event["event_id"])
                .then_some(value)
        })
        .next()
        .unwrap();
    let gate = later["gate"].as_str().unwrap();
    let baseline: Value =
        serde_json::from_slice(&fs::read(baseline_files(&repo).pop().unwrap()).unwrap()).unwrap();
    let source = &baseline["baseline"]["gates"][gate]["execution"];
    let source_file = repo
        .root
        .join(".git/arc/changes")
        .join(source["change_id"].as_str().unwrap())
        .join("events")
        .join(format!("{}.json", source["event_id"].as_str().unwrap()));
    let mut failure: Value = serde_json::from_slice(&fs::read(source_file).unwrap()).unwrap();
    failure["event_id"] = event["event_id"].clone();
    failure["change_id"] = id.into();
    failure["revision"] = event["revision"].clone();
    failure["tree"] = event["tree"].clone();
    failure["tested_tree"] = event["tree"].clone();
    if failure["work_tree"].is_object() {
        failure["work_tree"]["tree"] = failure["tested_tree"].clone();
        failure["work_tree"]["revision"] = failure["revision"].clone();
    }
    failure["result"] = "fail".into();
    failure["exit_code"] = 1.into();
    failure["run_id"] = Value::Null;
    fs::write(file, serde_json::to_vec_pretty(&failure).unwrap()).unwrap();
    repo.arc(&wt)
        .args(["show", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "base pass cannot displace another verification observation",
        ));
}

/// Claim: named verification answers each previously unobserved gate from the
/// target baseline. Rejects another gate's observation exhausting eligibility,
/// or a failed gate gaining a baseline answer after its head changes.
#[test]
fn named_gates_keep_independent_baseline_eligibility() {
    let repo = seed(&GATES.replace("command='true'", "command='exit \"${ARC_GATE_RESULT:-0}\"'"));
    let (_, wt) = begin(&repo, "one-at-a-time", "notes.md");
    for gate in ["unit", "other"] {
        repo.arc(&wt)
            .args(["verify", "--gate", gate])
            .assert()
            .success()
            .stdout(predicates::str::contains("base pass from"));
    }
    assert_eq!(bases(&state(&repo, &wt)), 2);
    assert_eq!(executed(&state(&repo, &wt)), 0);

    let (_, wt) = begin(&repo, "failed-first", "notes.md");
    repo.arc(&wt)
        .env("ARC_GATE_RESULT", "1")
        .args(["verify", "--gate", "unit", "--rerun"])
        .assert()
        .code(1);
    repo.commit(&wt, "later.md", "later\n", "test: later head");
    repo.arc(&wt)
        .args(["verify", "--gate", "other"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["verify", "--gate", "unit"])
        .assert()
        .success();
    let state = state(&repo, &wt);
    assert_eq!(
        bases(&state),
        1,
        "only the unobserved other gate reuses: {state}"
    );
    assert_eq!(executed(&state), 2, "unit must execute again: {state}");
}

/// Claim: two named verifiers racing on one unobserved gate append one baseline
/// answer. Rejects two stale snapshots publishing duplicate gate observations.
#[test]
fn racing_named_verifiers_append_one_baseline_answer() {
    use std::io::Write;
    let repo = seed(GATES);
    let (_, wt) = begin(&repo, "racing-named", "notes.md");
    let template = repo.arc(&wt);
    let mut command = Command::new(template.get_program());
    command
        .args(["verify", "--gate", "unit"])
        .current_dir(&wt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in template.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    command.env("ARC_TEST_IMPORT_RENDEZVOUS", "base-pass-before-append");
    let mut child = command.spawn().unwrap();
    let mut reader = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "arc test: import base-pass-before-append\n" {
            break;
        }
    }
    repo.arc(&wt)
        .args(["verify", "--gate", "unit"])
        .assert()
        .success();
    child.stdin.take().unwrap().write_all(b"+").unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(1));
    assert_eq!(bases(&state(&repo, &wt)), 1);
    assert_eq!(executed(&state(&repo, &wt)), 0);
}

/// Claim: baseline storage excludes command output and reuse storage is linear
/// in gate count. Rejects replicated output tails or a whole baseline per gate.
#[test]
fn noisy_executions_produce_compact_baselines_and_references() {
    let gates = GATES.replace("command='true'", "command='python3 -c \"print(chr(120)*20000)\"'")
        + "[gates.third]\ncommand='python3 -c \"print(chr(120)*20000)\"'\npaths=['third/']\nevents=['integration']\n";
    let repo = seed(&gates);
    let file = baseline_files(&repo).pop().unwrap();
    let bytes = fs::read(file).unwrap();
    let baseline: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        bytes.len() < 18_000,
        "baseline weighs {} bytes",
        bytes.len()
    );
    for entry in baseline["baseline"]["gates"].as_object().unwrap().values() {
        assert!(entry["execution"].get("output_tail").is_none());
        assert!(entry["execution"].get("output_artifact").is_none());
        assert!(entry["execution"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert!(entry["run"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
    }
    let (id, wt) = begin(&repo, "compact", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let mut bytes = 0;
    for file in event_files(&repo, &id) {
        let body = fs::read(file).unwrap();
        let event: Value = serde_json::from_slice(&body).unwrap();
        if event["event_type"] == "verification-base-reused" {
            assert!(event["witness"].get("baseline").is_none());
            bytes += body.len();
        }
    }
    assert!(bytes < 6000, "three reuse receipts weigh {bytes} bytes");
    assert_eq!(bases(&state(&repo, &wt)), 3);
}

/// Claim: publication obtains owed gates through the authority check's shared
/// states. Rejects an additional whole-ledger reduction for participant discovery.
#[test]
fn publication_does_not_reduce_unrelated_changes() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "indexed-publication", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    begin(&repo, "unrelated", "unrelated.md");
    let output = repo
        .arc(&repo.root)
        .env("ARC_TRACE", "1")
        .args(["publication", "--target", "master", "--skip-green"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("publication gate unit: pass"));
    let stderr = String::from_utf8(output.stderr).unwrap();
    let trace: Value = serde_json::from_str(
        stderr
            .lines()
            .find(|line| line.contains("arc-perf/1"))
            .unwrap(),
    )
    .unwrap();
    assert!(
        trace["counts"]["changes_reduced"].as_u64().unwrap_or(0) <= 6,
        "participant discovery must not add another reduction of the three changes: {trace}"
    );
}

/// Claim: the eight-baseline cache expires old refs without invalidating an
/// accepted receipt. Rejects unbounded refs or selecting an evicted baseline
/// merely because its Git objects remain reachable through an open branch.
#[test]
fn retention_bounds_refs_and_preserves_existing_receipts() {
    let repo = seed(GATES);
    let (_, used) = begin(&repo, "old-used", "old-used.md");
    repo.arc(&used).args(["verify", "--all"]).assert().success();
    let (_, unused) = begin(&repo, "old-unused", "old-unused.md");
    for index in 0..10 {
        let (id, wt) = begin(
            &repo,
            &format!("integration-{index}"),
            &format!("src/file-{index}.rs"),
        );
        repo.arc(&wt)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .success();
        repo.arc(&wt)
            .args(["review", "--verdict", "approved"])
            .assert()
            .success();
        repo.arc(&repo.root)
            .args(["integrate", &id])
            .assert()
            .success();
    }
    let refs = git_out(
        &repo.root,
        &["for-each-ref", "--format=%(refname)", "refs/arc/baselines/"],
    );
    assert_eq!(refs.lines().count(), 8, "{refs}");
    assert_eq!(bases(&state(&repo, &used)), 2);
    repo.arc(&unused)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(bases(&state(&repo, &unused)), 0);
    assert_eq!(executed(&state(&repo, &unused)), 2);
}

/// Claim: a first verify checks first-parent ancestry once for the shared gate
/// subject and visits only the baseline range. Rejects full-history walks or
/// repeating the same immutable ancestry proof for each gate and append.
#[test]
fn first_verify_bounds_and_shares_ancestry_walks() {
    let repo = Repo::new();
    repo.redeclare_gates(GATES);
    let mut parent = repo.head(&repo.root);
    let tree = git_out(&repo.root, &["rev-parse", "HEAD^{tree}"]);
    for _ in 0..40 {
        parent = git_out(
            &repo.root,
            &["commit-tree", &tree, "-p", &parent, "-m", "test: history"],
        );
    }
    git(&repo.root, &["reset", "--hard", &parent]);
    warm(&repo);
    let (_, wt) = begin(&repo, "bounded-history", "notes.md");
    let output = repo
        .arc(&wt)
        .env("ARC_TRACE", "1")
        .args(["verify", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let trace: Value = serde_json::from_str(
        stderr
            .lines()
            .find(|line| line.contains("arc-perf/1"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(trace["counts"]["base_ancestry_walks"], 1, "{trace}");
    assert_eq!(trace["counts"]["base_ancestry_commits"], 2, "{trace}");
}

/// Claim: publication audits base-pass participants even when the integrating
/// completion has no cache snapshot. Rejects making the publication backstop
/// depend on an optional post-completion baseline record.
#[test]
fn publication_audits_without_the_integrating_cache_snapshot() {
    let repo = seed(GATES);
    let (id, wt) = begin(&repo, "missing-cache-snapshot", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    fs::remove_file(baseline_files(&repo).pop().unwrap()).unwrap();
    repo.arc(&repo.root)
        .args(["publication", "--target", "master", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("publication gate unit: pass"))
        .stdout(predicates::str::contains("publication gate other: pass"));
}

fn raised_baseline(changed_command: bool) -> (Repo, String, String) {
    fn declaration(budget: u64, command: &str) -> String {
        format!("[budget]\ntotal='20000ms'\n[selection]\nbase_passes=true\ninert=['**/*.md']\n[gates.unit]\ncommand={command:?}\nbudget='{budget}ms'\npaths=['src/']\nevents=['integration']\n")
    }
    let repo = Repo::new();
    repo.redeclare_gates(&declaration(10000, "true"));
    let (warm, wt) = begin(&repo, "before-raise", "src/initial.rs");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &warm])
        .assert()
        .success();
    let before = repo.head(&repo.root);
    let (id, wt) = begin(&repo, "raised", "src/growth.rs");
    let lines = (0..100).map(|i| format!("item_{i}\n")).collect::<String>();
    repo.commit(&wt, "src/growth.rs", &lines, "test: counted growth");
    repo.commit(
        &wt,
        ".arc/gates.toml",
        &declaration(
            10400,
            if changed_command {
                "printf declaration-v2"
            } else {
                "true"
            },
        ),
        "test: proposed budget",
    );
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "10300")
        .args(["verify", "--all", "--rerun"])
        .assert()
        .code(1);
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "unit",
            "--reason",
            "Measured after reducing the added work",
        ])
        .assert()
        .success();
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    let after = repo.head(&repo.root);
    (repo, before, after)
}

/// Claim: a pass authorized by a landed raise answers an unaffected change under
/// the raised budget. Rejects judging reuse by the obsolete budget at execution
/// or discarding the execution that the completed raise authorized.
#[test]
fn raised_budget_judges_the_following_base_pass() {
    let (repo, _, _) = raised_baseline(false);
    let (_, wt) = begin(&repo, "after-raise", "notes.md");
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "15000")
        .args(["verify", "--all"])
        .assert()
        .success();
    let s = state(&repo, &wt);
    assert_eq!(bases(&s), 1, "{s}");
    assert_eq!(executed(&s), 0, "{s}");
    assert_eq!(s["gate_status"][0]["budget"]["budget_ms"], 10400);
    assert_eq!(s["gate_status"][0]["budget"]["run_ms"], 10300);
    assert_eq!(s["verifications"][0]["budget_at_run"]["budget_ms"], 10000);
}

/// Claim: a declaration changed alongside a landed raise executes its own check
/// on the next change. Rejects answering the changed declaration with the older
/// baseline's pass even though its timing fits the raised budget.
#[test]
fn raised_declaration_does_not_borrow_the_old_identity() {
    let (repo, _, _) = raised_baseline(true);
    let (id, wt) = begin(&repo, "changed-declaration", "notes.md");
    let output = repo.arc(&wt).args(["verify", "--all"]).output().unwrap();
    let direct = event_files(&repo, &id)
        .iter()
        .filter(|file| {
            let event: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
            event["event_type"] == "verification-recorded" && event["gate"] == "unit"
        })
        .count();
    assert_eq!(direct, 1, "the changed check must execute: {output:?}");
    let s = state(&repo, &wt);
    assert_eq!(
        executed(&s),
        1,
        "the changed check must execute: {output:?} {s}"
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(bases(&s), 0, "{s}");
    assert_eq!(s["verifications"][0]["command"], "printf declaration-v2");
}

/// Claim: raise evidence requires an execution even when a legitimate base pass
/// has the exact requested head/tree and qualifying timing. Rejects treating a
/// reuse as direct execution after the destination moves back to an older bound.
#[test]
fn raise_never_uses_a_base_pass_as_direct_execution() {
    let (repo, before, after) = raised_baseline(false);
    git(&repo.root, &["reset", "--hard", &before]);
    let out = stdout(repo.arc(&repo.root).args(["begin", "borrowed-raise"]));
    let id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap();
    let wt = repo.home.join(".worktrees/repo-borrowed-raise");
    git(&wt, &["reset", "--hard", &after]);
    git(
        &wt,
        &["commit", "--allow-empty", "-m", "test: equivalent tree"],
    );
    git(&repo.root, &["reset", "--hard", &after]);
    repo.arc(&wt).args(["snapshot"]).assert().success();
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    assert_eq!(bases(&state(&repo, &wt)), 1);
    git(&repo.root, &["reset", "--hard", &before]);
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "unit",
            "--reason",
            "Measured after reducing the added work",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "newest direct execution at this exact head",
        ));
    assert!(
        state(&repo, &wt)["budget_raises"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{id}"
    );
}

fn integrate_base_change(repo: &Repo, id: &str, wt: &Path) {
    repo.arc(wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", id])
        .assert()
        .success();
}

/// Claim: consecutive unaffected integrations reuse real executions across empty
/// baseline snapshots. Rejects alternating between reuse and full execution or
/// relabelling reused evidence as an execution in a later baseline.
#[test]
fn three_documentation_integrations_execute_no_gate_after_warming() {
    let repo = seed(GATES);
    for index in 0..3 {
        let (id, wt) = begin(
            &repo,
            &format!("consecutive-{index}"),
            &format!("notes-{index}.md"),
        );
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        let s = state(&repo, &wt);
        assert_eq!(bases(&s), 2, "documentation change {index}: {s}");
        assert_eq!(executed(&s), 0, "documentation change {index}: {s}");
        integrate_base_change(&repo, &id, &wt);
        let last: Value =
            serde_json::from_slice(&fs::read(baseline_files(&repo).pop().unwrap()).unwrap())
                .unwrap();
        assert!(last["baseline"]["gates"].as_object().unwrap().is_empty());
    }
}

/// Claim: the newest retained entry blocks an older matching declaration even
/// when the declaration returns to an earlier value. Rejects searching past an
/// incompatible entry for an older pass that would otherwise fit the diff.
#[test]
fn newest_gate_entry_blocks_an_older_matching_declaration() {
    let repo = seed(GATES);
    let different = GATES.replace(
        "[gates.other]\ncommand='true'",
        "[gates.other]\ncommand='printf other-v2'",
    );
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        &different,
        "test: changed target declaration",
    );
    let (id, wt) = begin(&repo, "newer-entries", "src/update.rs");
    repo.arc(&wt)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    integrate_base_change(&repo, &id, &wt);
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        GATES,
        "test: restored target declaration",
    );
    let (_, wt) = begin(&repo, "matching-older", "notes.md");
    repo.arc(&wt).args(["verify", "--all"]).assert().success();
    let s = state(&repo, &wt);
    assert_eq!(
        bases(&s),
        0,
        "the newest entries must prevent older matching passes: {s}"
    );
    assert_eq!(executed(&s), 2, "{s}");
}

/// Claim: covered paths changed before the change's own base still force the
/// gate to execute. Rejects comparing only the change delta rather than the
/// complete chosen-baseline-to-head diff.
#[test]
fn covered_target_changes_since_the_baseline_execute_the_gate() {
    let repo = seed(GATES);
    repo.commit(
        &repo.root,
        "src/target-update.rs",
        "changed\n",
        "test: target advances",
    );
    let (id, wt) = begin(&repo, "after-target-update", "notes.md");
    let output = repo.arc(&wt).args(["verify", "--all"]).output().unwrap();
    let direct = event_files(&repo, &id)
        .iter()
        .filter(|file| {
            let event: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
            event["event_type"] == "verification-recorded" && event["gate"] == "unit"
        })
        .count();
    assert_eq!(
        direct, 1,
        "the covered target path must cause execution: {output:?}"
    );
    assert!(output.status.success(), "{output:?}");
    let s = state(&repo, &wt);
    assert_eq!(bases(&s), 1, "{s}");
    assert_eq!(executed(&s), 1, "{s}");
}

/// Claim: gates can cite different execution baselines in one run, and their
/// independent proofs survive source-ledger removal, standalone transport and
/// publication auditing. Rejects one shared baseline/diff or missing citations.
#[test]
fn two_baselines_replay_export_import_and_audit_independently() {
    let source = seed(GATES);
    let (code, code_wt) = begin(&source, "unit-only", "src/update.rs");
    source
        .arc(&code_wt)
        .args(["verify", "--all"])
        .assert()
        .success();
    integrate_base_change(&source, &code, &code_wt);
    let (id, wt) = begin(&source, "two-baselines", "notes.md");
    source.arc(&wt).args(["verify", "--all"]).assert().success();
    let s = state(&source, &wt);
    assert_eq!(bases(&s), 2, "both gates must reuse: {s}");
    assert_eq!(executed(&s), 0, "{s}");
    let ids = s["verifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["base_pass"]["baseline_event_id"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 2, "{s}");
    for entry in fs::read_dir(source.root.join(".git/arc/changes")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap().to_str().unwrap() != id {
            fs::remove_dir_all(path).unwrap();
        }
    }
    assert_eq!(bases(&state(&source, &wt)), 2);
    let input = source.home.join("two-baselines.json");
    source
        .arc(&wt)
        .args(["export", &id, "--output", input.to_str().unwrap()])
        .assert()
        .success();
    let mut bundle: Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    assert_eq!(
        bundle["repository_events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["event_type"] == "gate-baseline-recorded")
            .count(),
        2
    );
    integrate_base_change(&source, &id, &wt);
    source
        .arc(&source.root)
        .args(["publication", "--target", "master", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("publication gate unit: pass"))
        .stdout(predicates::str::contains("publication gate other: pass"));
    let receiver = Repo::new();
    receiver
        .arc(&receiver.root)
        .args(["replica", "init", "authority"])
        .assert()
        .success();
    let origin = bundle["repository_id"].as_str().unwrap().to_owned();
    receiver
        .arc(&receiver.root)
        .args(["replica", "pair", "producer", "--repository-id", &origin])
        .assert()
        .success();
    let replica = json_stdout(
        receiver
            .arc(&receiver.root)
            .args(["replica", "status", "--json"]),
    );
    bundle["project_id"] = replica["project_id"].clone();
    deliver_git_objects(&source, &receiver);
    git(
        &receiver.root,
        &["reset", "--hard", "refs/delivered/heads/master"],
    );
    git(
        &receiver.root,
        &[
            "branch",
            "arc/two-baselines",
            "refs/delivered/heads/arc/two-baselines",
        ],
    );
    receiver.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{origin}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['facts','integration']\nactors=['tester']\n"));
    fs::write(&input, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    let digest = hex::encode(Sha256::digest(fs::read(&input).unwrap()));
    receiver
        .arc(&receiver.root)
        .args([
            "import",
            input.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &digest,
        ])
        .assert()
        .success();
    let imported = json_stdout(receiver.arc(&receiver.root).args(["show", &id, "--json"]));
    assert_eq!(bases(&imported), 2, "{imported}");
}
