//! Must-land-together relations and `arc land`: changes that reach their
//! target in one guarded transition, or not at all.

use crate::common::*;
use serde_json::Value;

/// A repository declaring one gate every change passes.
fn gated_repo() -> Repo {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.smoke]\ncommand = \"test -f README.md\"\n");
    repo
}

/// Open a change on `base` (the target tip when absent), commit one file,
/// snapshot, and approve it. Gates are left to the caller.
fn reviewed_change(repo: &Repo, slug: &str, base: Option<&str>) -> (String, PathBuf, String) {
    let mut args = vec!["begin", slug];
    if let Some(base) = base {
        args.extend(["--base", base]);
    }
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(&args)));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    let head = repo.head(&worktree);
    (change_id, worktree, head)
}

/// A reviewed change with its gates green at its own head.
fn ready_change(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
    let (change_id, worktree, head) = reviewed_change(repo, slug, None);
    repo.arc(&worktree)
        .args(["verify", slug, "--all"])
        .assert()
        .success();
    (change_id, worktree, head)
}

fn relate(repo: &Repo, a: &str, b: &str) {
    repo.arc(&repo.root)
        .args(["metadata", a, "--lands-with", b])
        .assert()
        .success();
}

fn status(repo: &Repo, change: &str) -> Value {
    json_stdout_any_status(repo.arc(&repo.root).args(["status", change]))
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Claim: a change whose must-land-together partner is still open cannot
/// integrate alone by any path, and each refusal names `arc land` with every
/// open member. Rejects: `integrate`, a queue, `check` or an asserted
/// closure treating the relation as advisory.
#[test]
fn a_change_with_an_open_partner_integrates_only_together() {
    let repo = gated_repo();
    let (a, _, _) = ready_change(&repo, "alpha");
    let (b, _, _) = ready_change(&repo, "beta");
    relate(&repo, &a, &b);

    let check = repo.arc(&repo.root).args(["check", &a]).output().unwrap();
    assert_eq!(check.status.code(), Some(19), "{}", stderr_of(&check));

    let report = status(&repo, &a);
    assert_eq!(report["lands_with"]["members"][0]["change_id"], b.as_str());
    assert_eq!(report["lands_with"]["members"][0]["status"], "open");
    assert_eq!(report["next_action"], "land_together");

    let alone = repo
        .arc(&repo.root)
        .args(["integrate", &a])
        .output()
        .unwrap();
    assert_eq!(alone.status.code(), Some(19), "{}", stderr_of(&alone));
    let mut pair = [a.clone(), b.clone()];
    pair.sort();
    let remedy = format!("arc land {} {}", pair[0], pair[1]);
    assert!(
        String::from_utf8_lossy(&alone.stderr).contains(&remedy),
        "{}",
        stderr_of(&alone)
    );

    let queue = repo
        .arc(&repo.root)
        .args(["integrate", &a, &b])
        .output()
        .unwrap();
    assert_eq!(queue.status.code(), Some(19), "{}", stderr_of(&queue));

    let target = git_out(&repo.root, &["rev-parse", "master"]);
    let head = status(&repo, &b)["current_head"]
        .as_str()
        .unwrap()
        .to_string();
    let asserted = repo
        .arc(&repo.root)
        .args(["close", &b, "--assert-integrated", &head])
        .output()
        .unwrap();
    assert!(!asserted.status.success());
    assert!(
        stderr_of(&asserted).contains(&remedy),
        "{}",
        stderr_of(&asserted)
    );

    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), target);
    assert!(status(&repo, &a)["closure"].is_null());
    assert!(status(&repo, &b)["closure"].is_null());
}

/// Claim: components are the transitive closure of relations, and a member
/// closed without integrating wedges the rest until the relation is
/// withdrawn with a reason. Rejects: a direct-edges-only reading, and a
/// wedged component that lets a member land as if the abandoned one had.
#[test]
fn an_abandoned_member_wedges_its_component_until_the_relation_is_withdrawn() {
    let repo = gated_repo();
    let (a, _, _) = ready_change(&repo, "alpha");
    let (b, _, _) = ready_change(&repo, "beta");
    let (c, _, _) = ready_change(&repo, "gamma");
    relate(&repo, &a, &b);
    relate(&repo, &b, &c);

    let members: Vec<String> = status(&repo, &a)["lands_with"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["change_id"].as_str().unwrap().to_string())
        .collect();
    let mut expected = vec![b.clone(), c.clone()];
    expected.sort();
    assert_eq!(members, expected);

    repo.arc(&repo.root)
        .args(["close", &c, "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", &a, "--remove-lands-with", &b])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--reason"));
    repo.arc(&repo.root)
        .args([
            "metadata",
            &b,
            "--remove-lands-with",
            &a,
            "--reason",
            "a ships first",
        ])
        .assert()
        .success();

    let wedged = repo
        .arc(&repo.root)
        .args(["integrate", &b])
        .output()
        .unwrap();
    assert_eq!(wedged.status.code(), Some(19), "{}", stderr_of(&wedged));
    assert!(
        stderr_of(&wedged).contains(&format!("--remove-lands-with {c}")),
        "{}",
        stderr_of(&wedged)
    );

    repo.arc(&repo.root)
        .args(["integrate", &a])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "metadata",
            &b,
            "--remove-lands-with",
            &c,
            "--reason",
            "gamma was dropped",
        ])
        .assert()
        .success();
    // Free of its component, beta answers to the next blocker: alpha moved
    // the fast-forward target under it.
    let freed = repo.arc(&repo.root).args(["check", &b]).output().unwrap();
    assert_eq!(freed.status.code(), Some(11), "{}", stderr_of(&freed));
}

/// Claim: a relation names two open changes with one target, and is one
/// repository event. Rejects: relating a closed change or a change to
/// itself, and a relation stored on only one change's ledger.
#[test]
fn a_relation_is_one_repository_event_between_open_changes() {
    let repo = gated_repo();
    let (a, _, _) = ready_change(&repo, "alpha");
    let (b, _, _) = ready_change(&repo, "beta");
    let (c, _, _) = ready_change(&repo, "gamma");
    repo.arc(&repo.root)
        .args(["metadata", &a, "--lands-with", &a])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["close", &c, "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", &a, "--lands-with", &c])
        .assert()
        .failure()
        .stderr(predicates::str::contains("closed"));
    relate(&repo, &a, &b);
    repo.arc(&repo.root)
        .args(["metadata", &b, "--lands-with", &a])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already lands with"));

    let events: Vec<Value> = stdout(repo.arc(&repo.root).args(["events", "--repository"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|event: &Value| event["event_type"] == "lands-with-declared")
        .collect();
    assert_eq!(events.len(), 1);
    let mut pair = vec![a.clone(), b.clone()];
    pair.sort();
    assert_eq!(events[0]["changes"], serde_json::json!(pair));
    assert_eq!(
        status(&repo, &b)["lands_with"]["members"][0]["change_id"],
        a.as_str()
    );
}
