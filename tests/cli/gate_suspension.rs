use crate::common::*;
use serde_json::Value;

const REASON: &str = "other lanes load the host";

fn declare(repo: &Repo) {
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\nbudget='10s'\n");
}
fn suspend(repo: &Repo, args: &[&str]) -> String {
    stdout(
        repo.arc(&repo.root)
            .args(["gates", "suspend", "--reason", REASON])
            .args(args),
    )
}
/// Run every gate afresh as an 11.3 s command; its exit code and output.
fn verify_over(repo: &Repo, wt: &Path) -> (i32, String) {
    let output = repo
        .arc(wt)
        .env("ARC_TEST_COMMAND_RUN_MS", "11300")
        .args(["verify", "--all", "--rerun"])
        .output()
        .unwrap();
    (
        output.status.code().unwrap(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}
fn status(repo: &Repo, wt: &Path, id: &str) -> Value {
    json_stdout(repo.arc(wt).args(["status", id]))
}
fn latest(repo: &Repo, wt: &Path, id: &str) -> Value {
    json_stdout(repo.arc(wt).args(["show", id, "--json"]))["verifications"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone()
}
fn approve(repo: &Repo, wt: &Path) {
    repo.arc(wt)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
}
fn suspensions_in_force(repo: &Repo) -> usize {
    json_stdout(repo.arc(&repo.root).args(["gates", "--json"]))["budget_suspensions"]
        .as_array()
        .unwrap()
        .len()
}
fn repository_events(repo: &Repo) -> Vec<(PathBuf, Value)> {
    fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| {
            let value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            (path, value)
        })
        .collect()
}

/// I5: an over-budget pass authorizes nothing until a suspension covers its
/// gate, and then integrates under that suspension's name, a receipt recorded
/// earlier included. Rejects accepting the overrun with no suspension, and a
/// suspension that leaves the refusal standing or accepts unlabelled.
#[test]
fn an_over_budget_pass_is_refused_until_a_suspension_covers_its_gate() {
    let repo = Repo::new();
    declare(&repo);
    let (id, wt, _) = change_with_patchset(&repo, "suspended");
    approve(&repo, &wt);
    assert_eq!(verify_over(&repo, &wt).0, 1);
    let before = repo.head(&repo.root);
    repo.arc(&wt).args(["check", &id]).assert().code(5);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .code(5);
    assert_eq!(repo.head(&repo.root), before);

    let recorded = suspend(&repo, &["--for", "1h"]);
    assert!(
        recorded.contains("budgets suspended for every gate until"),
        "{recorded}"
    );
    let report = status(&repo, &wt, &id);
    let budget = &report["gates"][0]["budget"];
    assert_eq!(budget["verdict"], "suspended", "{report}");
    assert_eq!(budget["suspension"]["reason"], REASON);
    assert_eq!(report["gates"][0]["green_at_head"], true);
    assert_eq!(
        report["budget_suspensions"][0]["event_id"],
        budget["suspension"]["event_id"]
    );
    let check = json_stdout(repo.arc(&wt).args(["check", &id, "--json"]));
    assert_eq!(check["gates"][0]["budget"]["verdict"], "suspended");

    let (code, text) = verify_over(&repo, &wt);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("Pass (accepted over budget, ran 11.3s of 10s: budgets suspended until"),
        "{text}"
    );
    let raw = latest(&repo, &wt, &id);
    assert_eq!(raw["result"], "pass");
    assert_eq!(raw["budget_at_run"]["verdict"], "suspended");
    assert_eq!(
        raw["budget_at_run"]["suspension"]["event_id"],
        budget["suspension"]["event_id"]
    );
    let shown = json_stdout(repo.arc(&wt).args(["show", &id, "--json"]));
    assert_eq!(shown["gate_status"][0]["budget"]["verdict"], "suspended");
    assert!(stdout(repo.arc(&wt).args(["show", &id])).contains("accepted over budget"));
    let gates = json_stdout(repo.arc(&wt).args(["gates", "--json"]));
    assert_eq!(gates["budget_suspensions"][0]["reason"], REASON);
    assert_eq!(gates["gates"][0]["suspended_runs"], 1);
    assert_eq!(
        gates["gates"][0]["last_suspended"]["event_id"],
        raw["event_id"]
    );
    for command in ["gates", "catchup"] {
        assert!(
            stdout(repo.arc(&wt).args([command])).contains("budgets suspended for every gate"),
            "{command}"
        );
    }
    let config: Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 13);

    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    assert_ne!(repo.head(&repo.root), before);
}

/// I5: a suspension for another gate, one resumed and one past its end each
/// leave an over-budget pass refused, and none is reported as in force.
/// Rejects a suspension outliving its end or its scope.
#[test]
fn an_expired_resumed_or_other_gate_suspension_accepts_nothing() {
    let repo = Repo::new();
    declare(&repo);
    let (id, wt, _) = change_with_patchset(&repo, "lapsed");
    suspend(&repo, &["--for", "1h", "lint"]);
    let other_gate = verify_over(&repo, &wt).0;
    suspend(&repo, &["--for", "1h"]);
    let ended = stdout(repo.arc(&repo.root).args(["gates", "resume"]));
    assert_eq!(ended.matches("ended").count(), 2, "{ended}");
    let resumed = verify_over(&repo, &wt).0;
    suspend(&repo, &["--for", "50ms"]);
    thread::sleep(Duration::from_millis(60));
    let expired = verify_over(&repo, &wt).0;
    assert_eq!(
        [other_gate, resumed, expired],
        [1, 1, 1],
        "other gate, resumed, expired"
    );
    assert_eq!(suspensions_in_force(&repo), 0);
    let report = status(&repo, &wt, &id);
    assert_eq!(report["gates"][0]["budget"]["verdict"], "over-budget");
    assert!(report["budget_suspensions"].is_null(), "{report}");
    assert!(!stdout(repo.arc(&wt).args(["catchup"])).contains("budgets suspended"));
    assert!(stdout(repo.arc(&repo.root).args(["gates", "resume"])).contains("no budget suspension"));
}

/// I5: a suspension has an end inside the ceiling. Rejects a suspension with
/// no end, one already over, and one longer than a day.
#[test]
fn a_suspension_ends_within_a_day() {
    let repo = Repo::new();
    declare(&repo);
    let suspend = |args: &[&str]| {
        let output = repo
            .arc(&repo.root)
            .args(["gates", "suspend", "--reason", REASON])
            .args(args)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let day_and_more = (chrono::Utc::now() + chrono::Duration::hours(25)).to_rfc3339();
    let refused = [
        suspend(&[]),
        suspend(&["--for", "25h"]),
        suspend(&["--until", &day_and_more]),
        suspend(&["--until", "2020-01-01T00:00:00Z"]),
    ];
    assert_eq!(
        refused.iter().map(|(ok, _)| *ok).collect::<Vec<_>>(),
        [false; 4],
        "{refused:?}"
    );
    assert!(refused[1].1.contains("lasts at most 86400s"), "{refused:?}");
    assert_eq!(suspensions_in_force(&repo), 0);
    assert!(suspend(&["--for", "24h"]).0);
    assert_eq!(suspensions_in_force(&repo), 1);
}

/// I12: a session bound to the implementer or reviewer role records no
/// suspension. Rejects a delegated session setting aside the budgets its own
/// work is judged by.
#[test]
fn only_a_lead_records_a_suspension() {
    let repo = Repo::new();
    declare(&repo);
    for role in ["implementer", "reviewer"] {
        repo.arc(&repo.root)
            .env("ARC_ROLE", role)
            .args(["gates", "suspend", "--for", "1h", "--reason", REASON])
            .assert()
            .code(9)
            .stderr(predicates::str::contains("may not gates suspend"));
    }
    assert_eq!(suspensions_in_force(&repo), 0);
}

/// I5: a gate whose command fails is not green under a suspension that covers
/// it, and its change does not integrate. Rejects a suspension standing in
/// for the gate's result.
#[test]
fn a_failing_gate_fails_under_a_suspension() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='false'\nbudget='10s'\n");
    let (id, wt, _) = change_with_patchset(&repo, "failing");
    approve(&repo, &wt);
    suspend(&repo, &["--for", "1h"]);
    let exit = verify_over(&repo, &wt).0;
    let report = status(&repo, &wt, &id);
    assert_eq!(
        (
            exit,
            report["gates"][0]["result"].as_str(),
            report["gates"][0]["green_at_head"].as_bool()
        ),
        (1, Some("fail"), Some(false)),
        "{report}"
    );
    let before = repo.head(&repo.root);
    repo.arc(&wt).args(["check", &id]).assert().failure();
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .failure();
    assert_eq!(repo.head(&repo.root), before);
}

/// I5: publication refuses an over-budget pass, fresh or cached, while a
/// suspension accepts the same run for integration. Rejects a suspension
/// reaching publication through the change that owns the published branch.
#[test]
fn publication_refuses_an_over_budget_pass_under_a_suspension() {
    let repo = Repo::new();
    declare(&repo);
    let (id, wt, _) = change_with_patchset(&repo, "strict-publication");
    suspend(&repo, &["--for", "1h"]);
    assert_eq!(verify_over(&repo, &wt).0, 0);
    let branch = git_out(&wt, &["branch", "--show-current"]);
    for cached in [false, true] {
        let mut command = repo.arc(&repo.root);
        command.env("ARC_TEST_COMMAND_RUN_MS", "11300").args([
            "publication",
            "--target",
            &branch,
            "--change",
            &id,
        ]);
        if cached {
            command.arg("--skip-green");
        }
        command
            .assert()
            .code(1)
            .stderr(predicates::str::contains("over budget, ran 11.3s of 10s"));
    }
    let receipts = repository_events(&repo)
        .into_iter()
        .map(|(_, event)| event)
        .filter(|event| event["event_type"] == "publication-verified")
        .collect::<Vec<_>>();
    assert!(!receipts.is_empty());
    for receipt in receipts {
        assert_eq!(receipt["budget_accepted"], false, "{receipt}");
        assert_eq!(
            receipt["gates"]["unit"]["budget_at_run"]["verdict"],
            "over-budget"
        );
    }
}

/// I5: a run accepted under a suspension is never the work reference of a
/// later run, even once its clock fits a raised budget. Rejects a loaded
/// host's overrun becoming what later overruns are excused against.
#[test]
fn a_run_accepted_under_a_suspension_never_becomes_a_reference() {
    let repo = Repo::new();
    let declare = |budget: u64| {
        repo.declare_gates_locally(&format!(
            "[gates.unit]\ncommand='true'\nbudget='{budget}ms'\n"
        ))
    };
    declare(1000);
    let (id, wt, _) = change_with_patchset(&repo, "no-suspended-reference");
    let run = |ms: u64, instructions: u64, other: u64| {
        repo.arc(&wt)
            .env("ARC_TEST_COMMAND_RUN_MS", ms.to_string())
            .env(
                "ARC_TEST_GATE_WORK",
                crate::gate_work::readings(Some(instructions), other),
            )
            .args(["verify", "--all", "--rerun"])
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(run(900, 1000, 1000), Some(0));
    suspend(&repo, &["--for", "1h"]);
    assert_eq!(run(1100, 1030, 1000), Some(0));
    assert_eq!(
        latest(&repo, &wt, &id)["budget_at_run"]["verdict"],
        "suspended"
    );
    stdout(repo.arc(&repo.root).args(["gates", "resume"]));
    declare(1100);
    let exit = run(1200, 1040, 5000);
    assert_eq!(
        (
            exit,
            latest(&repo, &wt, &id)["work_reference"]["instructions"].as_u64()
        ),
        (Some(1), Some(1000))
    );
}

/// I5: a suspension is honoured only on the ledger that recorded it: an
/// export leaves it behind, and a copy placed in another ledger accepts
/// nothing there. Rejects a suspension travelling with a bundle or a replica.
#[test]
fn a_suspension_binds_only_the_ledger_that_recorded_it() {
    let source = Repo::new();
    declare(&source);
    let (carried, _, _) = change_with_patchset(&source, "carried");
    suspend(&source, &["--for", "1h"]);
    let bundle = source.home.join("carried.json");
    source
        .arc(&source.root)
        .args(["export", &carried, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let exported: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let exports_it = exported["repository_events"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|event| event["event_type"] == "gate-budgets-suspended");

    let receiver = Repo::new();
    declare(&receiver);
    let (_, wt, _) = change_with_patchset(&receiver, "received");
    let (path, _) = repository_events(&source)
        .into_iter()
        .find(|(_, event)| event["event_type"] == "gate-budgets-suspended")
        .unwrap();
    let planted = receiver.root.join(".git/arc/repository/events");
    fs::create_dir_all(&planted).unwrap();
    fs::copy(&path, planted.join(path.file_name().unwrap())).unwrap();
    let foreign_accepts = verify_over(&receiver, &wt).0 == 0;
    assert_eq!(
        (exports_it, foreign_accepts, suspensions_in_force(&receiver)),
        (false, false, 0)
    );
}

/// A run accepted under a suspension seeds no base pass, so no other change
/// inherits it. Rejects a baseline holding an overrun a suspension accepted.
#[test]
fn a_run_accepted_under_a_suspension_cannot_seed_a_base_pass() {
    let repo = Repo::new();
    repo.redeclare_gates(
        "[selection]\nbase_passes=true\n[gates.unit]\ncommand='true'\nbudget='10s'\npaths=['src/']\n",
    );
    let (id, wt, _) = change_with_patchset(&repo, "no-suspended-base");
    approve(&repo, &wt);
    suspend(&repo, &["--for", "1h"]);
    assert_eq!(verify_over(&repo, &wt).0, 0);
    repo.arc(&repo.root)
        .args(["integrate", &id])
        .assert()
        .success();
    for (_, event) in repository_events(&repo) {
        if event["event_type"] == "gate-baseline-recorded" {
            assert!(
                event["baseline"]["gates"]["unit"].is_null(),
                "a suspended run seeded a baseline: {event}"
            );
        }
    }
}

/// A run accepted under a suspension cannot justify a budget raise. Rejects
/// taking it for the clean overrun a raise needs because it fits the proposal.
#[test]
fn a_run_accepted_under_a_suspension_cannot_justify_a_raise() {
    let repo = Repo::new();
    repo.redeclare_gates("[budget]\ntotal='20s'\n[selection]\ninert=['.arc/']\n[gates.unit]\ncommand='true'\nbudget='10s'\npaths=['src/']\n");
    let (_, wt, _) = change_with_patchset(&repo, "no-suspended-raise");
    fs::create_dir_all(wt.join("src")).unwrap();
    repo.commit(
        &wt,
        "src/code.rs",
        &(0..1000).map(|i| format!("line_{i}\n")).collect::<String>(),
        "test: add source",
    );
    repo.commit(&wt, ".arc/gates.toml", "[budget]\ntotal='20s'\n[selection]\ninert=['.arc/']\n[gates.unit]\ncommand='true'\nbudget='12s'\npaths=['src/']\n", "test: declare raise");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    suspend(&repo, &["--for", "1h"]);
    assert_eq!(verify_over(&repo, &wt).0, 0);
    stdout(repo.arc(&repo.root).args(["gates", "resume"]));
    repo.arc(&wt)
        .args([
            "gates",
            "raise",
            "unit",
            "--reason",
            "Measured coverage cost",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "newest direct execution must be clean",
        ));
}
