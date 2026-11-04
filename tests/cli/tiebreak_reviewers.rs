use super::common::*;
use serde_json::{json, Value};

const SOL: &str = "gpt-6.1-sol#xhigh";
const OPUS: &str = "claude-opus-5-5#high";
const ENTRY: &str =
    "tiebreak_reviewers = [{ model = 'gpt-6.1-sol#xhigh', since = '2026-11-04T00:00:00Z' }]\n";

fn policy(repo: &Repo, entries: &str, assessed: bool) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        &format!(
            "[policy]\nforbid_self_approval = true\n{entries}{}",
            if assessed {
                "review_clearance = 'assessed'\n"
            } else {
                ""
            }
        ),
        "test: review policy",
    );
}

fn subject(repo: &Repo, second: &str) -> (String, PathBuf) {
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "mixed"])));
    let worktree = repo.home.join(".worktrees/repo-mixed");
    for (actor, session, model, harness) in [
        ("Sol", "sol-writer", SOL, "codex"),
        ("Other", "other-writer", second, "claude"),
    ] {
        repo.commit(
            &worktree,
            &format!("{actor}.txt"),
            "work\n",
            &format!("test: subject work\n\nImplemented-by: {harness}:{model}"),
        );
        let mut cmd = repo.arc(&worktree);
        cmd.env("ARC_ACTOR", actor)
            .env("ARC_SESSION", session)
            .env("ARC_HARNESS", harness)
            .args(["--model", model, "snapshot", "mixed"]);
        if actor == "Other" {
            cmd.args(["--contributors", "Sol,Other"]);
        }
        cmd.assert().success();
    }
    (change, worktree)
}

fn reader(repo: &Repo, cwd: &Path, verb: &str, model: &str) -> AssertCommand {
    let mut cmd = repo.arc(cwd);
    cmd.env("ARC_ACTOR", "Reader")
        .env("ARC_SESSION", "reader-session")
        .env("ARC_HARNESS", "codex")
        .args(["--model", model, verb, "mixed", "--verdict", "approved"]);
    cmd
}

fn ship(repo: &Repo, worktree: &Path) {
    repo.arc(worktree)
        .args(["integrate", "mixed", "--debt", "fixture unread"])
        .assert()
        .success();
}

fn debt(repo: &Repo) -> Value {
    json_stdout(repo.arc(&repo.root).args(["status", "mixed", "--json"]))["debt"].clone()
}

/// Claim: only an elected mixed-work reader discharges debt. Rejects the
/// unconditional same-model bar and a blanket permission without policy.
#[test]
fn tiebreak_audit_clears_mixed_debt_only_with_target_policy() {
    for elected in [true, false] {
        let repo = Repo::new();
        policy(&repo, if elected { ENTRY } else { "" }, false);
        let (_, worktree) = subject(&repo, OPUS);
        ship(&repo, &worktree);
        if elected {
            reader(&repo, &worktree, "audit", SOL)
                .assert()
                .success()
                .stdout(predicates::str::contains("debt discharged"))
                .stderr(predicates::str::contains("not that it was independent").not());
            assert_eq!(debt(&repo)["standing"], "discharged");
            for verb in ["show", "status"] {
                let text = stdout(repo.arc(&worktree).args([verb, "mixed"]));
                assert!(
                    text.contains("tie-break reviewer gpt-6.1-sol#xhigh (policy since 2026-11-04"),
                    "{text}"
                );
            }
        } else {
            reader(&repo, &worktree, "audit", SOL)
                .assert()
                .failure()
                .stderr(predicates::str::contains(
                    "matches the ledger implementer's model",
                ))
                .stderr(predicates::str::contains("tiebreak_reviewers"));
            assert_eq!(debt(&repo)["standing"], "owed");
        }
    }
}

/// Claim: an election never clears work authored solely by that model,
/// including several efforts. Rejects treating effort differences as mixing.
#[test]
fn tiebreak_refuses_single_model_work_at_multiple_efforts() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (_, worktree) = subject(&repo, "gpt-6.1-sol#medium");
    ship(&repo, &worktree);
    reader(&repo, &worktree, "audit", SOL)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "matches the ledger implementer's model",
        ));
}

/// Claim: effort and the read-time interval bind the election. Rejects
/// matching only a model name or applying a future entry early.
#[test]
fn tiebreak_refuses_wrong_effort_and_reads_outside_the_interval() {
    for (model, entry) in [
        ("gpt-6.1-sol#medium", ENTRY.to_string()),
        (SOL, ENTRY.replace("2026-11-04", "2099-11-04")),
        (
            SOL,
            ENTRY.replace(
                "since = '2026-11-04T00:00:00Z'",
                "since = '2020-01-01T00:00:00Z', until = '2020-01-02T00:00:00Z'",
            ),
        ),
    ] {
        let repo = Repo::new();
        policy(&repo, &entry, false);
        let (_, worktree) = subject(&repo, OPUS);
        ship(&repo, &worktree);
        reader(&repo, &worktree, "audit", model)
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "matches the ledger implementer's model",
            ));
    }
}

/// Claim: an elected coordinate never clears an implementer's actor or
/// session, or an assumed reader. Rejects using an election as self-review.
#[test]
fn tiebreak_refuses_author_actor_session_and_assumed_identity() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (_, worktree) = subject(&repo, OPUS);
    ship(&repo, &worktree);
    reader(&repo, &worktree, "audit", SOL)
        .env("ARC_ACTOR", "Sol")
        .assert()
        .failure()
        .stderr(predicates::str::contains("matches contributor Sol"));
    reader(&repo, &worktree, "audit", SOL)
        .env("ARC_SESSION", "sol-writer")
        .assert()
        .failure();
    reader(&repo, &worktree, "audit", SOL)
        .env_remove("ARC_ACTOR")
        .assert()
        .failure()
        .stderr(predicates::str::contains("assumed the auditing identity"));
    assert_eq!(debt(&repo)["standing"], "owed");
}

/// Claim: an elected mixed-work verdict supplies integration's approval.
/// Rejects wiring the exception only into the post-integration audit path.
#[test]
fn tiebreak_approval_gates_open_mixed_work() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (_, worktree) = subject(&repo, OPUS);
    let read = reader(&repo, &worktree, "review", SOL).output().unwrap();
    assert!(read.status.success(), "{read:?}");
    repo.arc(&worktree)
        .args(["check", "mixed"])
        .assert()
        .success();
    assert!(!String::from_utf8_lossy(&read.stderr).contains("not that it was independent"));
    assert!(stdout(repo.arc(&worktree).args(["show", "mixed"]))
        .contains("tie-break reviewer gpt-6.1-sol#xhigh (policy since 2026-11-04"));
    repo.arc(&worktree)
        .args(["integrate", "mixed"])
        .assert()
        .success();
}

fn ladder(repo: &Repo, sol_rank: u64) {
    let table = json!({"schema":"arc-review-ladder/1", "version":[{
        "id":"fixture", "effective_from":"2020-01-01T00:00:00Z", "source":"fixture capabilities", "gap":false,
        "model":[
            {"model":"gpt-6.1-sol", "identities":["gpt-6.1-sol"], "family":"openai", "superseded":false,
             "efforts":[{"effort":"xhigh", "rank":sol_rank, "floor":2}]},
            {"model":"claude-opus-5-5", "identities":["claude-opus-5-5"], "family":"anthropic", "superseded":false,
             "efforts":[{"effort":"high", "rank":3, "floor":3}]},
            {"model":"support", "identities":["support"], "family":"third", "superseded":false,
             "efforts":[{"effort":"high", "rank":3, "floor":2}]}
        ]
    }]});
    repo.commit(
        &repo.root,
        ".arc/review-ladder.toml",
        &toml::to_string(&table).unwrap(),
        "test: target capability ladder",
    );
}

/// Claim: the election replaces only family independence. Rejects a
/// same-family bar on a capable reader and bypassing another author's floor.
#[test]
fn tiebreak_ladder_clears_only_when_every_author_floor_is_met() {
    for rank in [2, 3] {
        let repo = Repo::new();
        policy(&repo, ENTRY, true);
        ladder(&repo, rank);
        let (_, worktree) = subject(&repo, OPUS);
        reader(&repo, &worktree, "review", SOL).assert().success();
        let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
        let judgment = report["review_context"]["judgments"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(
            judgment["kind"],
            if rank == 3 { "clearing" } else { "findings" },
            "{report}"
        );
        if rank == 3 {
            repo.arc(&worktree)
                .args(["check", "mixed"])
                .assert()
                .success();
        } else {
            assert_eq!(judgment["reason"], "below-floor");
            repo.arc(&worktree)
                .args(["check", "mixed"])
                .assert()
                .failure();
        }
        ship(&repo, &worktree);
        reader(&repo, &worktree, "audit", SOL).assert().success();
        assert_eq!(
            debt(&repo)["standing"],
            if rank == 3 { "discharged" } else { "owed" }
        );
    }
}

/// Claim: a candidate cannot elect its own reviewer. Rejects reading the
/// candidate checkout instead of committed destination policy.
#[test]
fn tiebreak_candidate_policy_cannot_authorize_its_own_approval() {
    let repo = Repo::new();
    policy(&repo, "", false);
    let (_, worktree) = subject(&repo, OPUS);
    repo.commit(
        &worktree,
        ".arc/policy.toml",
        &format!("[policy]\nforbid_self_approval = true\n{ENTRY}"),
        "test: candidate election",
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Other")
        .env("ARC_SESSION", "other-writer")
        .args([
            "--model",
            OPUS,
            "snapshot",
            "mixed",
            "--contributors",
            "Sol,Other",
        ])
        .assert()
        .success();
    reader(&repo, &worktree, "review", SOL).assert().success();
    repo.arc(&worktree)
        .args(["check", "mixed"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("self-approval"));
}

/// Claim: later elections or date edits never re-judge a recorded read.
/// Rejects recomputing a grant or its absence from mutable destination dates.
#[test]
fn tiebreak_decision_is_frozen_when_the_read_is_recorded() {
    for elected in [true, false] {
        let repo = Repo::new();
        policy(&repo, if elected { ENTRY } else { "" }, false);
        let (_, worktree) = subject(&repo, OPUS);
        reader(&repo, &worktree, "review", SOL).assert().success();
        policy(&repo, if elected { "" } else { ENTRY }, false);
        let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
        assert_eq!(report["has_valid_approval"], elected, "{report}");
    }
}

/// Claim: closed-change reads use the target's current election and the
/// operator layer. Rejects freezing new audits to pre-integration policy.
#[test]
fn tiebreak_audits_use_current_destination_and_operator_elections() {
    for operator in [false, true] {
        let repo = Repo::new();
        policy(&repo, "", false);
        let (_, worktree) = subject(&repo, OPUS);
        ship(&repo, &worktree);
        if operator {
            fs::write(
                repo.root.join(".git/arc/operator-policy.toml"),
                format!("[policy]\n{ENTRY}"),
            )
            .unwrap();
        } else {
            policy(&repo, ENTRY, false);
        }
        reader(&repo, &worktree, "audit", SOL)
            .assert()
            .success()
            .stdout(predicates::str::contains("debt discharged"));
        if !operator {
            policy(&repo, "", false);
        }
        if operator {
            fs::remove_file(repo.root.join(".git/arc/operator-policy.toml")).unwrap();
        }
        assert_eq!(debt(&repo)["standing"], "discharged");
    }
}

/// Claim: invalid or overlapping same-coordinate entries refuse by entry
/// name; different coordinates may coexist. Rejects silent policy typos.
#[test]
fn tiebreak_policy_validates_each_entry_and_same_coordinate_intervals() {
    for entry in [
        "{model='gpt-6.1-sol', since='2020-01-01T00:00:00Z'}",
        "{model='gpt-6.1-sol#xhigh'}",
        "{model='gpt-6.1-sol#xhigh#high', since='2020-01-01T00:00:00Z'}",
        "{model='gpt-6.1-sol#xhigh medium', since='2020-01-01T00:00:00Z'}",
        "{model='gpt-6.1-sol#xhigh', since='bad-date'}",
        "{model='gpt-6.1-sol#xhigh', since='2020-01-01T00:00:00Z', until='2019-01-01T00:00:00Z'}",
        "{model='gpt-6.1-sol#xhigh', since='2020-01-01T00:00:00Z', untill='2021-01-01T00:00:00Z'}",
        "{model='gpt-6.1-sol#xhigh', since='2020-01-01T00:00:00Z'}, {model='gpt-6.1-sol#xhigh', since='2021-01-01T00:00:00Z'}",
        "{model='gpt-6.1-sol#xhigh', since='2020-01-01T00:00:00Z'}, {model='gpt-6.1-sol#medium', since='2021-01-01T00:00:00Z'}",
    ] {
        let repo = Repo::new();
        policy(&repo, &format!("tiebreak_reviewers = [{entry}]\n"), false);
        repo.arc(&repo.root).args(["begin", "invalid"]).assert().failure()
            .stderr(predicates::str::contains("tiebreak_reviewers["));
    }
    let repo = Repo::new();
    policy(&repo, "tiebreak_reviewers = [\n{model='gpt-6.1-sol#xhigh', since='2020-01-01T00:00:00Z', until='2021-01-01T00:00:00Z'},\n{model='gpt-6.1-sol#xhigh', since='2021-01-01T00:00:00Z'},\n{model='claude-opus-5-5#high', since='2020-01-01T00:00:00Z'}]\n", false);
    repo.arc(&repo.root)
        .args(["begin", "valid"])
        .assert()
        .success();
}

/// Claim: the session store must agree with the complete elected coordinate,
/// including effort. Rejects ignoring same-model effort contradictions.
#[test]
fn tiebreak_refuses_a_contradictory_observed_coordinate() {
    const SESSION: &str = "4c0a9d1e-2b7f-4e55-9a31-8d6f0c2e7b14";
    for observed in ["gpt-6.1-sol#medium", OPUS] {
        let repo = Repo::new();
        policy(&repo, ENTRY, false);
        let (_, worktree) = subject(&repo, OPUS);
        ship(&repo, &worktree);
        let dir = repo.home.join("codex-state/sessions/2026/11/04");
        fs::create_dir_all(&dir).unwrap();
        let (model, effort) = observed.split_once('#').unwrap();
        fs::write(dir.join(format!("rollout-2026-11-04T00-00-00-{SESSION}.jsonl")), format!("{}\n{}\n",
            json!({"type":"session_meta","timestamp":"2026-11-04T00:00:00Z","payload":{"id":SESSION}}),
            json!({"type":"turn_context","timestamp":"2026-11-04T00:01:00Z","payload":{"model":model,"effort":effort}}))).unwrap();
        reader(&repo, &worktree, "audit", SOL)
            .env("ARC_SESSION", SESSION)
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "matches the ledger implementer's model",
            ));
        assert_eq!(debt(&repo)["standing"], "owed");
    }
}

/// Claim: read time is compared with inclusive since and exclusive until.
/// Rejects treating either endpoint as open-ended or ignoring created_at.
#[test]
fn tiebreak_read_time_uses_the_declared_interval_endpoints() {
    let repo = Repo::new();
    policy(&repo, "tiebreak_reviewers = [{model='gpt-6.1-sol#xhigh', since='2020-01-01T00:00:00Z', until='2099-01-01T00:00:00Z'}]\n", false);
    let (change, worktree) = subject(&repo, OPUS);
    reader(&repo, &worktree, "review", SOL).assert().success();
    for (time, clears) in [
        ("2020-01-01T00:00:00Z", true),
        ("2019-12-31T23:59:59.999999999Z", false),
        ("2098-12-31T23:59:59.999999999Z", true),
        ("2099-01-01T00:00:00Z", false),
    ] {
        rewrite_event(&repo, &change, "verdict-recorded", |event| {
            event["created_at"] = json!(time)
        });
        let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
        assert_eq!(report["has_valid_approval"], clears, "{time}: {report}");
    }
}

/// Claim: operator policy rejects invalid elections before publication, and
/// duplicates across project/operator layers refuse when combined.
#[test]
fn tiebreak_operator_publication_and_layering_validate_elections() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let file = repo.home.join("operator.toml");
    fs::write(
        &file,
        "[policy]\ntiebreak_reviewers=[{model='gpt-6.1-sol#xhigh'}]\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["policy", "write", "--body-file"])
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicates::str::contains("tiebreak_reviewers[0]"));
    assert!(!repo.root.join(".git/arc/operator-policy.toml").exists());
    fs::create_dir_all(repo.root.join(".git/arc")).unwrap();
    fs::write(
        repo.root.join(".git/arc/operator-policy.toml"),
        format!("[policy]\n{ENTRY}"),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["begin", "duplicate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("tiebreak_reviewers[0]"))
        .stderr(predicates::str::contains("overlaps"));
}

/// Claim: unreadable election policy cannot grant an exception or erase a
/// recorded review. Rejects propagating election lookup errors before append.
#[test]
fn tiebreak_unreadable_policy_records_the_review_without_an_exception() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (change, worktree) = subject(&repo, OPUS);
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        "[policy\n",
        "test: unreadable policy",
    );
    reader(&repo, &worktree, "review", SOL)
        .assert()
        .success()
        .stdout(predicates::str::contains("verdict: Approved"))
        .stderr(predicates::str::contains(
            "is recorded; whether it gates could not be evaluated",
        ));
    let recorded = fs::read_dir(event_dir(&repo, &change))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<Value>(&fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .find(|event| event["event_type"] == "verdict-recorded")
        .unwrap();
    assert!(recorded.get("tiebreak_reviewer").is_none(), "{recorded}");
    policy(&repo, ENTRY, false);
    let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
    assert_eq!(report["has_valid_approval"], false, "{report}");
}

/// Claim: an unavailable election cannot bar reporting audit findings or
/// grant model independence. Rejects refusing the read or discharging debt.
#[test]
fn tiebreak_unreadable_policy_keeps_an_audit_findings_read() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (change, worktree) = subject(&repo, OPUS);
    ship(&repo, &worktree);
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        "[policy\n",
        "test: unreadable policy",
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reader")
        .env("ARC_SESSION", "reader-session")
        .env("ARC_HARNESS", "codex")
        .args([
            "--model",
            SOL,
            "audit",
            "mixed",
            "--verdict",
            "changes-requested",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("audit: ChangesRequested"));
    let recorded = fs::read_dir(event_dir(&repo, &change))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<Value>(&fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .find(|event| event["event_type"] == "audit-verdict-recorded")
        .unwrap();
    assert!(recorded.get("tiebreak_reviewer").is_none(), "{recorded}");
    policy(&repo, ENTRY, false);
    assert_eq!(debt(&repo)["standing"], "owed");
}

/// Claim: session independence follows the represented reader, not its writer.
/// Rejects an author session hiding in --on-behalf-of and a writer's session
/// barring a genuinely different represented reader.
#[test]
fn tiebreak_checks_the_represented_readers_authoring_session() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (_, worktree) = subject(&repo, OPUS);
    ship(&repo, &worktree);
    reader(&repo, &worktree, "audit", SOL)
        .args(["--on-behalf-of", &format!("codex:sol-writer:{SOL}")])
        .assert()
        .failure();
    reader(&repo, &worktree, "audit", SOL)
        .env("ARC_SESSION", "sol-writer")
        .args(["--on-behalf-of", &format!("codex:independent-reader:{SOL}")])
        .assert()
        .success()
        .stdout(predicates::str::contains("debt discharged"));
}

/// Claim: a missing harness does not turn another effort of the reader's
/// model into mixed authorship. Rejects granting on a bare ledger coordinate.
#[test]
fn tiebreak_a_missing_author_harness_cannot_create_mixed_models() {
    for second in ["gpt-6.1-sol#medium", OPUS] {
        let repo = Repo::new();
        policy(&repo, ENTRY, false);
        let (change, worktree) = subject(&repo, second);
        rewrite_event(&repo, &change, "patchset-added", |event| {
            event["harness"] = Value::Null
        });
        ship(&repo, &worktree);
        let mut audit = reader(&repo, &worktree, "audit", SOL);
        if second == OPUS {
            audit
                .assert()
                .success()
                .stdout(predicates::str::contains("debt discharged"));
        } else {
            audit.assert().failure();
            assert_eq!(debt(&repo)["standing"], "owed");
        }
    }
}

/// Claim: aliases of the same ladder model cannot manufacture mixed work.
/// Rejects clearing self-authored work through a second accepted spelling.
#[test]
fn tiebreak_ladder_aliases_cannot_create_mixed_authorship() {
    for assessed in [true, false] {
        let repo = Repo::new();
        policy(&repo, ENTRY, assessed);
        ladder(&repo, 3);
        let mut table: toml::Value =
            toml::from_str(&fs::read_to_string(repo.root.join(".arc/review-ladder.toml")).unwrap())
                .unwrap();
        table["version"][0]["model"][0]["identities"]
            .as_array_mut()
            .unwrap()
            .push(toml::Value::String("sol-alias".into()));
        repo.commit(
            &repo.root,
            ".arc/review-ladder.toml",
            &toml::to_string(&table).unwrap(),
            "test: accepted model alias",
        );
        let (_, worktree) = subject(&repo, "sol-alias#xhigh");
        reader(&repo, &worktree, "review", SOL).assert().success();
        let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
        assert_eq!(
            report["has_valid_approval"], false,
            "all authors resolve to the reader's model: {report}"
        );
        let recorded = json_stdout(repo.arc(&worktree).args(["show", "mixed", "--json"]));
        assert!(
            recorded["verdicts"][0].get("tiebreak_reviewer").is_none(),
            "{recorded}"
        );
        if assessed {
            let judgment = report["review_context"]["judgments"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(judgment["kind"], "findings", "{report}");
        }
    }
}

/// Claim: an election on one destination cannot bootstrap another target's
/// policy. Rejects carrying a frozen exemption through --into to an unelected target.
#[test]
fn tiebreak_elections_cannot_transfer_to_another_destination() {
    let repo = Repo::new();
    policy(&repo, "", false);
    git(&repo.root, &["switch", "-c", "elected"]);
    policy(&repo, ENTRY, false);
    let (_, worktree) = subject(&repo, OPUS);
    reader(&repo, &worktree, "review", SOL).assert().success();
    let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
    assert_eq!(report["has_valid_approval"], true, "{report}");
    git(&repo.root, &["switch", "master"]);
    repo.arc(&worktree)
        .args(["integrate", "mixed", "--into", "master", "--dry-run"])
        .assert()
        .failure();
    repo.arc(&worktree)
        .args([
            "integrate",
            "mixed",
            "--into",
            "master",
            "--debt",
            "fixture destination read owed",
        ])
        .assert()
        .success();
    reader(&repo, &worktree, "audit", SOL)
        .assert()
        .success()
        .stdout(predicates::str::contains("debt discharged"));
    let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
    assert_eq!(
        report["audit_verdicts"][0]["tiebreak_target"], "master",
        "{report}"
    );
}

/// Claim: an unavailable election adds no bar to an ordinary independent
/// audit. Rejects requiring tie-break policy when no model exception is used.
#[test]
fn tiebreak_unavailable_election_preserves_an_ordinary_audit() {
    let repo = Repo::new();
    policy(&repo, ENTRY, false);
    let (_, worktree) = subject(&repo, OPUS);
    ship(&repo, &worktree);
    repo.commit(
        &repo.root,
        ".arc/policy.toml",
        "[policy\n",
        "test: unreadable policy",
    );
    reader(&repo, &worktree, "audit", SOL).assert().failure();
    reader(&repo, &worktree, "audit", "independent#high")
        .assert()
        .success()
        .stdout(predicates::str::contains("audit: Approved"));
    policy(&repo, ENTRY, false);
    assert_eq!(debt(&repo)["standing"], "discharged");
    let report = json_stdout(repo.arc(&worktree).args(["status", "mixed", "--json"]));
    assert!(
        report["audit_verdicts"][0]
            .get("tiebreak_reviewer")
            .is_none(),
        "{report}"
    );
}
