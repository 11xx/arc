use crate::common::*;
use serde_json::{json, Value};
use std::io::Write;

fn reviewed(repo: &Repo, slug: &str, base: Option<&str>, empty: bool) -> (String, PathBuf, String) {
    let mut args = vec!["begin", slug];
    if let Some(base) = base {
        args.extend(["--base", base]);
    }
    let id = opened_change_id(&stdout(repo.arc(&repo.root).args(args)));
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    if empty {
        git(
            &wt,
            &["commit", "--allow-empty", "-m", "test: empty member"],
        );
    } else {
        repo.commit(&wt, &format!("{slug}.txt"), "member\n", "test: member");
    }
    snapshot_and_approve(repo, &wt);
    let head = repo.head(&wt);
    (id, wt, head)
}

fn snapshot_and_approve(repo: &Repo, wt: &Path) {
    repo.arc(wt).args(["snapshot"]).assert().success();
    repo.arc(wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
}

fn pair(repo: &Repo, composed: bool, same_tree: bool) -> [(String, PathBuf, String); 2] {
    let lower = reviewed(repo, "lower", None, composed && same_tree);
    let upper = reviewed(
        repo,
        "upper",
        (!composed).then_some(lower.2.as_str()),
        false,
    );
    [lower, upper]
}

fn receipts(repo: &Repo, top: &str) -> Vec<Value> {
    json_stdout(repo.arc(&repo.root).args(["show", top, "--json"]))["verifications"]
        .as_array()
        .unwrap()
        .clone()
}

fn assert_unmoved(repo: &Repo, before: &str, ids: &[&str]) {
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    for id in ids {
        let shown = json_stdout(repo.arc(&repo.root).args(["show", id, "--json"]));
        assert!(shown["closure"].is_null(), "{shown}");
        assert!(shown["pending_landing"].is_null(), "{shown}");
    }
}

fn land(repo: &Repo, lower: &str, upper: &str) -> AssertCommand {
    let mut cmd = repo.arc(&repo.root);
    cmd.args([
        "land",
        lower,
        upper,
        "--debt",
        "composed context needs review",
        "--max-attempts",
        "3",
    ]);
    cmd
}

/// Claim: both landing forms refuse the same over-budget receipt as integrate,
/// leave the target unmoved and do not retry or split. Rejects raw passes or
/// suppressed member gate blockers authorizing a landing.
fn landing_budgets_refuse_integrates_over_budget_receipt_in_both_forms(composed: bool) {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\nbudget='10s'\n");
    let [(lower, _, _), (upper, wt, _)] = pair(&repo, composed, true);
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "11000")
        .args(["verify", "--all"])
        .assert()
        .code(1);
    let before = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args(["integrate", &upper])
        .assert()
        .code(5)
        .stderr(predicates::str::contains("over budget"));
    land(&repo, &lower, &upper)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .assert()
        .code(5)
        .stderr(predicates::str::contains(&lower))
        .stderr(predicates::str::contains(
            "gate unit: over budget, ran 11s of 10s",
        ));
    assert_unmoved(&repo, &before, &[&lower, &upper]);
    let raw = receipts(&repo, &upper);
    assert_eq!(raw.len(), 1, "{raw:?}");
    assert_eq!(raw[0]["run_ms"], 11000);
}

/// Claim: both landing forms refuse the same over-budget receipt as integrate,
/// leave the target unmoved and do not retry or split. Rejects raw passes or
/// suppressed member gate blockers authorizing a landing.
#[test]
fn landing_budgets_refuse_integrates_over_budget_receipt_in_both_forms_stack() {
    landing_budgets_refuse_integrates_over_budget_receipt_in_both_forms(false);
}

/// Claim: both landing forms refuse the same over-budget receipt as integrate,
/// leave the target unmoved and do not retry or split. Rejects raw passes or
/// suppressed member gate blockers authorizing a landing.
#[test]
fn landing_budgets_refuse_integrates_over_budget_receipt_in_both_forms_composed() {
    landing_budgets_refuse_integrates_over_budget_receipt_in_both_forms(true);
}

/// Claim: a later member's tighter budget governs the one combined execution,
/// including its derived limit, in both forms. Equality is accepted and each
/// member's total is retained. Rejects keeping the first same-identity budget.
fn landing_budgets_fold_the_later_members_tighter_budget(composed: bool) {
    for run_ms in [10000, 11000] {
        let repo = Repo::new();
        repo.redeclare_gates("[budget]\ntotal='40s'\n[gates.unit]\ncommand='true'\nbudget='30s'\n");
        let [(lower, _, _), (upper, wt, _)] = pair(&repo, composed, false);
        repo.commit(
            &wt,
            ".arc/gates.toml",
            "[budget]\ntotal='35s'\n[gates.unit]\ncommand='true'\nbudget='10s'\n",
            "test: tighten budget",
        );
        snapshot_and_approve(&repo, &wt);
        let before = repo.head(&repo.root);
        let output = land(&repo, &lower, &upper)
            .env("ARC_TEST_COMMAND_RUN_MS", run_ms.to_string())
            .output()
            .unwrap();
        let raw = receipts(&repo, &upper);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0]["budget_at_run"]["budget_ms"], 10000);
        assert_eq!(raw[0]["execution_limit_ms"], 100000);
        assert_eq!(raw[0]["run_ms"], run_ms);
        if run_ms > 10000 {
            assert_eq!(output.status.code(), Some(5));
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains(&upper), "{error}");
            assert!(error.contains("ran 11s of 10s"), "{error}");
            assert_unmoved(&repo, &before, &[&lower, &upper]);
        } else {
            assert!(output.status.success(), "{output:?}");
            for (id, total, budget) in [(&lower, 40000, 30000), (&upper, 35000, 10000)] {
                let shown = json_stdout(repo.arc(&repo.root).args(["show", id, "--json"]));
                let auth = &shown["closure"]["authorization"];
                assert_eq!(auth["budget_rules"]["total"], total);
                assert_eq!(auth["gates"]["unit"]["budget_ms"], budget);
            }
        }
    }
}

/// Claim: a later member's tighter budget governs the one combined execution,
/// including its derived limit, in both forms. Equality is accepted and each
/// member's total is retained. Rejects keeping the first same-identity budget.
#[test]
fn landing_budgets_fold_the_later_members_tighter_budget_stack() {
    landing_budgets_fold_the_later_members_tighter_budget(false);
}

/// Claim: a later member's tighter budget governs the one combined execution,
/// including its derived limit, in both forms. Equality is accepted and each
/// member's total is retained. Rejects keeping the first same-identity budget.
#[test]
fn landing_budgets_fold_the_later_members_tighter_budget_composed() {
    landing_budgets_fold_the_later_members_tighter_budget(true);
}

/// Claim: invalid totals independently refuse landing even with green rows.
/// Rejects suppressing declaration errors with member gate blockers.
fn landing_budgets_validate_green_members(composed: bool) {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[budget]\ntotal='9s'\n[gates.unit]\ncommand='true'\nbudget='10s'\n",
    );
    let [(lower, low_wt, _), (upper, wt, _)] = pair(&repo, composed, false);
    for wt in [&low_wt, &wt] {
        repo.arc(wt)
            .env("ARC_TEST_COMMAND_RUN_MS", "1")
            .args(["verify", "--all"])
            .assert()
            .success();
    }
    for id in [&lower, &upper] {
        let s = json_stdout(repo.arc(&repo.root).args(["status", id]));
        assert!(s["gates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["green_at_head"] == true));
    }
    let before = repo.head(&repo.root);
    land(&repo, &lower, &upper)
        .assert()
        .code(5)
        .stderr(predicates::str::contains(
            "integration sum 10s exceeds total 9s",
        ));
    assert_unmoved(&repo, &before, &[&lower, &upper]);
}

/// Claim: the composed union fits the minimum member total even when every
/// member's own suite fits. Rejects validating member declarations alone.
#[test]
fn landing_budgets_validate_the_combined_total() {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[budget]\ntotal='10s'\n[gates.unit]\ncommand='true'\nbudget='1s'\n[gates.alpha]\ncommand='true'\nbudget='6s'\nprofiles=['lower']\n[gates.beta]\ncommand='true'\nbudget='6s'\nprofiles=['upper']\n",
    );
    let mut ids = Vec::new();
    for name in ["lower", "upper"] {
        ids.push(opened_change_id(&stdout(repo.arc(&repo.root).args([
            "begin",
            name,
            "--profile",
            name,
        ]))));
        let wt = repo.home.join(".worktrees").join(format!("repo-{name}"));
        repo.commit(&wt, &format!("{name}.txt"), "member\n", "test: member");
        snapshot_and_approve(&repo, &wt);
        repo.arc(&wt)
            .env("ARC_TEST_COMMAND_RUN_MS", "1")
            .args(["verify", "--all"])
            .assert()
            .success();
    }
    let [lower, upper] = ids.as_slice() else {
        unreachable!()
    };
    let before = repo.head(&repo.root);
    land(&repo, lower, upper)
        .assert()
        .code(5)
        .stderr(predicates::str::contains("combined suite"))
        .stderr(predicates::str::contains(
            "integration sum 13s exceeds total 10s",
        ));
    assert_unmoved(&repo, &before, &[lower, upper]);
}

/// Claim: invalid totals independently refuse landing even with green rows.
/// Rejects suppressing declaration errors with member gate blockers.
#[test]
fn landing_budgets_validate_green_members_stack() {
    landing_budgets_validate_green_members(false);
}

/// Claim: invalid totals independently refuse landing even with green rows.
/// Rejects suppressing declaration errors with member gate blockers.
#[test]
fn landing_budgets_validate_green_members_composed() {
    landing_budgets_validate_green_members(true);
}

/// Claim: untimed shared receipts cannot satisfy budgeted gates in either form,
/// even when legacy duration_ms is small. Rejects treating missing run_ms as
/// exempt, substituting duration_ms or rerunning to hide the refusal.
fn landing_budgets_refuse_untimed_shared_receipts(composed: bool) {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\nbudget='10s'\n");
    let [(lower, _, _), (upper, wt, _)] = pair(&repo, composed, true);
    repo.arc(&wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .args(["verify", "--all"])
        .assert()
        .success();
    rewrite_event(&repo, &upper, "verification-recorded", |v| {
        v.as_object_mut().unwrap().remove("run_ms");
        v.as_object_mut().unwrap().remove("budget_at_run");
        v["duration_ms"] = json!(1);
    });
    let before = repo.head(&repo.root);
    land(&repo, &lower, &upper)
        .env("ARC_TEST_COMMAND_RUN_MS", "1")
        .assert()
        .code(5)
        .stderr(predicates::str::contains(&lower))
        .stderr(predicates::str::contains(
            "gate unit: unmeasured: run_ms is missing for budget 10s",
        ));
    assert_unmoved(&repo, &before, &[&lower, &upper]);
    assert_eq!(receipts(&repo, &upper).len(), 1);
}

/// Claim: untimed shared receipts cannot satisfy budgeted gates in either form,
/// even when legacy duration_ms is small. Rejects treating missing run_ms as
/// exempt, substituting duration_ms or rerunning to hide the refusal.
#[test]
fn landing_budgets_refuse_untimed_shared_receipts_stack() {
    landing_budgets_refuse_untimed_shared_receipts(false);
}

/// Claim: untimed shared receipts cannot satisfy budgeted gates in either form,
/// even when legacy duration_ms is small. Rejects treating missing run_ms as
/// exempt, substituting duration_ms or rerunning to hide the refusal.
#[test]
fn landing_budgets_refuse_untimed_shared_receipts_composed() {
    landing_budgets_refuse_untimed_shared_receipts(true);
}

fn paused_land(
    repo: &Repo,
    lower: &str,
    upper: &str,
) -> (Child, BufReader<std::process::ChildStderr>) {
    let template = land(repo, lower, upper);
    let mut command = Command::new(template.get_program());
    command
        .args(template.get_args())
        .current_dir(&repo.root)
        .env("ARC_TEST_COMMAND_RUN_MS", "9000")
        .env("ARC_TEST_IMPORT_RENDEZVOUS", "landing-gated")
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
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "landing never gated: {line}"
        );
        if line == "arc test: import landing-gated\n" {
            break;
        }
    }
    (child, reader)
}

fn release_land(
    mut child: Child,
    mut reader: BufReader<std::process::ChildStderr>,
) -> (i32, String) {
    child.stdin.take().unwrap().write_all(b"+").unwrap();
    let output = child.wait_with_output().unwrap();
    let mut error = String::new();
    reader.read_to_string(&mut error).unwrap();
    (output.status.code().unwrap(), error)
}

/// Claim: final authorization judges the actual top or composed receipt against
/// every member, independently of execution's exit code. Rejects authorizing a
/// shared receipt whose timing refuses after execution has already returned zero.
fn landing_budgets_rejudge_receipts_at_final_acceptance(composed: bool) {
    for untimed in [false, true] {
        let repo = Repo::new();
        repo.declare_gates_locally("[gates.unit]\ncommand='true'\nbudget='10s'\n");
        let [(lower, _, _), (upper, _, _)] = pair(&repo, composed, false);
        let before = repo.head(&repo.root);
        let (child, reader) = paused_land(&repo, &lower, &upper);
        rewrite_event(&repo, &upper, "verification-recorded", |v| {
            if untimed {
                v.as_object_mut().unwrap().remove("run_ms");
            } else {
                v["run_ms"] = json!(11000);
            }
        });
        let (code, error) = release_land(child, reader);
        assert_eq!(code, 5, "{error}");
        assert!(error.contains(&lower), "{error}");
        assert!(
            error.contains(if untimed {
                "unmeasured"
            } else {
                "ran 11s of 10s"
            }),
            "{error}"
        );
        assert_unmoved(&repo, &before, &[&lower, &upper]);
    }
}

/// Claim: final authorization judges the actual top or composed receipt against
/// every member, independently of execution's exit code. Rejects authorizing a
/// shared receipt whose timing refuses after execution has already returned zero.
#[test]
fn landing_budgets_rejudge_receipts_at_final_acceptance_stack() {
    landing_budgets_rejudge_receipts_at_final_acceptance(false);
}

/// Claim: final authorization judges the actual top or composed receipt against
/// every member, independently of execution's exit code. Rejects authorizing a
/// shared receipt whose timing refuses after execution has already returned zero.
#[test]
fn landing_budgets_rejudge_receipts_at_final_acceptance_composed() {
    landing_budgets_rejudge_receipts_at_final_acceptance(true);
}

/// Claim: budget-only authority changes are detected before either landing form
/// moves the target, even when receipts fit both declarations. Rejects execution
/// identity hiding a changed member budget or total from confirmation.
fn landing_budgets_confirm_budget_only_authority_changes(composed: bool) {
    for total in [false, true] {
        let repo = Repo::new();
        let declaration = |amount| {
            format!(
                "[gates.unit]\ncommand='true'\nbudget='{}'\n{}",
                if total { "10s" } else { amount },
                if total {
                    format!("[budget]\ntotal='{amount}'\n")
                } else {
                    String::new()
                }
            )
        };
        repo.declare_gates_locally(&declaration("20s"));
        let [(lower, _, _), (upper, _, _)] = pair(&repo, composed, false);
        let before = repo.head(&repo.root);
        let (child, reader) = paused_land(&repo, &lower, &upper);
        repo.declare_gates_locally(&declaration("15s"));
        let (code, error) = release_land(child, reader);
        assert_ne!(code, 0);
        assert!(
            error.contains(if total {
                "budget_rules moved"
            } else {
                ".gate_budgets moved"
            }),
            "{error}"
        );
        assert_unmoved(&repo, &before, &[&lower, &upper]);
    }
}

/// Claim: budget-only authority changes are detected before either landing form
/// moves the target, even when receipts fit both declarations. Rejects execution
/// identity hiding a changed member budget or total from confirmation.
#[test]
fn landing_budgets_confirm_budget_only_authority_changes_stack() {
    landing_budgets_confirm_budget_only_authority_changes(false);
}

/// Claim: budget-only authority changes are detected before either landing form
/// moves the target, even when receipts fit both declarations. Rejects execution
/// identity hiding a changed member budget or total from confirmation.
#[test]
fn landing_budgets_confirm_budget_only_authority_changes_composed() {
    landing_budgets_confirm_budget_only_authority_changes(true);
}
