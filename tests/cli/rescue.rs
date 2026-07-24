use super::common::*;
use predicates::prelude::*;

fn begin(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let output = stdout(repo.arc(&repo.root).args(["begin", slug]));
    (
        opened_change_id(&output),
        repo.home.join(".worktrees").join(format!("repo-{slug}")),
    )
}

fn claim_from_dead_session(repo: &Repo, slug: &str) {
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "dead actor")
        .env("ARC_HARNESS", "dead-harness")
        .env("ARC_SESSION", "dead-session")
        .args(["claim", slug, "--stage-budget", "launch=1s"])
        .assert()
        .success();
}

#[test]
fn stale_foreign_claim_is_abandoned_and_reports_owner() {
    let repo = Repo::new();
    let (change_id, worktree) = begin(&repo, "stale-rescue");
    claim_from_dead_session(&repo, "stale-rescue");
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Owner: dead actor via dead-harness/dead-session",
        ))
        .stdout(predicate::str::contains("State: stale"))
        .stdout(predicate::str::contains("Abandoned: yes"));
}

#[test]
fn fresh_foreign_claim_is_not_abandoned() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "fresh-rescue");
    claim_from_dead_session(&repo, "fresh-rescue");

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains("State: active"))
        .stdout(predicate::str::contains("Abandoned: no"));
}

#[test]
fn rescue_reports_dirty_and_clean_worktrees() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "dirty-rescue");

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains("Uncommitted edits: absent"));
    fs::write(worktree.join("uncommitted.txt"), "work\n").unwrap();
    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains("Uncommitted edits: present"));
}

#[test]
fn rescue_reports_missing_patchset_without_head_drift() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "no-patchset-rescue");

    repo.arc(&worktree)
        .arg("rescue")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Branch head: no patchset recorded",
        ))
        .stdout(predicate::str::contains("moved past").not());
}

#[test]
fn take_transfers_a_stale_claim_and_records_displaced_owner() {
    let repo = Repo::new();
    let (change_id, worktree) = begin(&repo, "take-rescue");
    claim_from_dead_session(&repo, "take-rescue");
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&worktree)
        .args(["rescue", "--take"])
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).arg("status"))).unwrap();
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
    let claims = stdout(repo.arc(&worktree).args([
        "events",
        "--change",
        "take-rescue",
        "--type",
        "claim-set",
    ]));
    let takeover: serde_json::Value = serde_json::from_str(claims.lines().last().unwrap()).unwrap();
    assert_eq!(takeover["displaced"]["actor"], "dead actor");
    assert_eq!(takeover["displaced"]["stage"], "launch");
}

#[test]
fn take_claims_an_expired_foreign_claim_without_recording_displacement() {
    let repo = Repo::new();
    let (change_id, worktree) = begin(&repo, "expired-rescue");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "dead actor")
        .env("ARC_HARNESS", "dead-harness")
        .env("ARC_SESSION", "dead-session")
        .args(["claim", "expired-rescue", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &change_id, "claim-set", 5);

    repo.arc(&worktree)
        .args(["rescue", "--take"])
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).arg("status"))).unwrap();
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
    let claims = stdout(repo.arc(&worktree).args([
        "events",
        "--change",
        "expired-rescue",
        "--type",
        "claim-set",
    ]));
    let takeover: serde_json::Value = serde_json::from_str(claims.lines().last().unwrap()).unwrap();
    assert!(takeover.get("displaced").is_none());
}

#[test]
fn take_refuses_a_fresh_claim_without_changing_owner() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "refuse-rescue");
    claim_from_dead_session(&repo, "refuse-rescue");

    repo.arc(&worktree)
        .args(["rescue", "--take"])
        .assert()
        .code(8)
        .stderr(predicate::str::contains("not yet stale"));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&worktree).arg("status"))).unwrap();
    assert_eq!(status["claim"]["owner"]["session"], "dead-session");
}

#[test]
fn rescue_json_uses_versioned_schema() {
    let repo = Repo::new();
    let (_, worktree) = begin(&repo, "json-rescue");
    let output = stdout(repo.arc(&worktree).args(["rescue", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["schema"], "arc-rescue/1");
}
