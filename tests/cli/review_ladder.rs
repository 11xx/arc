//! Ladder import is local publication; show reads committed target content.

use super::common::*;
use serde_json::{json, Value};
use std::os::unix::fs::symlink;

fn ladder(id: &str) -> Value {
    json!({
        "schema": "arc-review-ladder/1",
        "version": [{
            "id": id, "effective_from": "2026-10-07T00:00:00Z",
            "source": "fixture capability judgment", "gap": false,
            "model": [
                {"model": "reader", "identities": ["reader"], "family": "lab-a",
                 "superseded": false, "efforts": [{"effort": "high", "rank": 2, "floor": 2}]},
                {"model": "author", "identities": ["author"], "family": "lab-b",
                 "superseded": true, "efforts": [{"effort": "high", "rank": 2, "floor": 2}]}
            ]
        }]
    })
}

fn input(repo: &Repo, id: &str) -> PathBuf {
    let file = repo.home.join(format!("{id}.json"));
    fs::write(&file, ladder(id).to_string()).unwrap();
    file
}

fn open(repo: &Repo, slug: &str) -> PathBuf {
    repo.arc(&repo.root)
        .args(["begin", slug])
        .assert()
        .success();
    repo.home.join(".worktrees").join(format!("repo-{slug}"))
}

fn import(repo: &Repo, cwd: &Path, file: &Path) {
    repo.arc(cwd)
        .args(["review-ladder", "import"])
        .arg(file)
        .assert()
        .success();
}

fn commit_registry(repo: &Repo, id: &str) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    // JSON is an import source; the accepted repository file is always TOML.
    let worktree = open(repo, id);
    import(repo, &worktree, &input(repo, id));
    git(&worktree, &["add", ".arc/review-ladder.toml"]);
    git(&worktree, &["commit", "-m", "test: capability table"]);
    repo.arc(&worktree)
        .args(["snapshot", id])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["integrate", id, "--debt", "fixture registry"])
        .assert()
        .success();
}

/// Claim: JSON imports become TOML only inside the invoking change checkout.
/// Rejects writing the target checkout or accepting an explicit different change.
#[test]
fn import_writes_only_the_current_change() {
    let repo = Repo::new();
    let worktree = open(&repo, "registry");
    let file = input(&repo, "candidate");
    let nested = worktree.join("nested");
    fs::create_dir(&nested).unwrap();
    import(&repo, &nested, &file);
    let stored = fs::read_to_string(worktree.join(".arc/review-ladder.toml")).unwrap();
    assert!(
        stored.starts_with("schema = \"arc-review-ladder/1\""),
        "{stored}"
    );
    assert!(!repo.root.join(".arc/review-ladder.toml").exists());
    assert!(!nested.join(".arc").exists());
    repo.arc(&repo.root)
        .args(["--change", "registry", "review-ladder", "import"])
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicates::str::contains("current open change"));
}

/// Claim: a candidate's own table supplies no accepted registry.
/// Rejects show reading the checkout's imported but unmerged ladder.
#[test]
fn a_candidate_cannot_show_its_own_ladder_as_accepted() {
    let repo = Repo::new();
    let worktree = open(&repo, "candidate");
    import(&repo, &worktree, &input(&repo, "candidate"));
    repo.arc(&worktree)
        .args(["review-ladder", "show", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no accepted review ladder"));
}

/// Claim: accepted means the target branch's current committed table.
/// Rejects a candidate's override, dirty target edits, or a closure-time freeze.
#[test]
fn show_reads_the_current_target_even_for_a_closed_change() {
    let repo = Repo::new();
    commit_registry(&repo, "first");
    let worktree = open(&repo, "candidate");
    import(&repo, &worktree, &input(&repo, "candidate"));
    let read = |cwd: &Path, selected: bool| {
        let mut command = repo.arc(cwd);
        if selected {
            command.args(["--change", "candidate"]);
        }
        json_stdout(command.args(["review-ladder", "show", "--json"]))
    };
    for cwd in [&worktree, &repo.root] {
        assert_eq!(read(cwd, true)["version"][0]["id"], "first");
    }
    repo.arc(&worktree)
        .args(["close", "candidate", "--abandoned"])
        .assert()
        .success();
    // Advance the target without touching the retained candidate worktree.
    let bytes = fs::read_to_string(repo.root.join(".arc/review-ladder.toml")).unwrap();
    fs::write(
        repo.root.join(".arc/review-ladder.toml"),
        bytes.replace("first", "later"),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/review-ladder.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: accepted registry advances"],
    );
    fs::write(
        repo.root.join(".arc/review-ladder.toml"),
        "invalid dirty table",
    )
    .unwrap();
    for cwd in [&worktree, &repo.root] {
        assert_eq!(read(cwd, true)["version"][0]["id"], "later");
    }
    assert_eq!(read(&worktree, false)["version"][0]["id"], "later");
}

/// Claim: a symlinked .arc directory cannot redirect publication to another checkout.
/// Rejects a validated input being written through a linked parent directory.
#[test]
fn import_refuses_a_linked_arc_directory() {
    let repo = Repo::new();
    let worktree = open(&repo, "linked");
    let other = repo.root.join(".arc");
    fs::create_dir_all(&other).unwrap();
    symlink(&other, worktree.join(".arc")).unwrap();
    repo.arc(&worktree)
        .args(["review-ladder", "import"])
        .arg(input(&repo, "linked"))
        .assert()
        .failure()
        .stderr(predicates::str::contains("symlink"));
    assert!(!other.join("review-ladder.toml").exists());
}

/// Claim: refused data cannot replace a previously imported ladder.
/// Rejects validation after publication or a correction silently being ignored.
#[test]
fn invalid_import_keeps_the_previous_file() {
    let repo = Repo::new();
    let worktree = open(&repo, "atomic");
    let file = input(&repo, "valid");
    import(&repo, &worktree, &file);
    let target = worktree.join(".arc/review-ladder.toml");
    let before = fs::read(&target).unwrap();
    let mut invalid = ladder("invalid");
    invalid["correction"] = json!([{"id": "correction-one"}]);
    fs::write(&file, invalid.to_string()).unwrap();
    repo.arc(&worktree)
        .args(["review-ladder", "import"])
        .arg(&file)
        .assert()
        .failure()
        .stderr(predicates::str::contains("corrections-unsupported"));
    assert_eq!(fs::read(target).unwrap(), before);
}

/// Claim: a closed change's accepted registry belongs to its recorded
/// integration destination. Rejects using the originally proposed target
/// after integration explicitly selected another branch.
#[test]
fn show_uses_the_recorded_integration_destination() {
    let repo = Repo::new();
    commit_registry(&repo, "original");
    git(&repo.root, &["switch", "-c", "alternate"]);
    let file = repo.root.join(".arc/review-ladder.toml");
    let bytes = fs::read_to_string(&file).unwrap();
    fs::write(&file, bytes.replace("original", "alternate")).unwrap();
    git(&repo.root, &["add", ".arc/review-ladder.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: alternate destination registry"],
    );
    repo.arc(&repo.root)
        .args(["begin", "subject", "--target", "master"])
        .assert()
        .success();
    let worktree = repo.home.join(".worktrees/repo-subject");
    fs::write(worktree.join("subject.txt"), "fixture work").unwrap();
    git(&worktree, &["add", "subject.txt"]);
    git(&worktree, &["commit", "-m", "test: subject work"]);
    repo.arc(&worktree)
        .args(["snapshot", "subject"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "integrate",
            "subject",
            "--into",
            "alternate",
            "--shape",
            "no-ff",
            "--debt",
            "fixture integration",
        ])
        .assert()
        .success();
    let value = json_stdout(repo.arc(&worktree).args([
        "--change",
        "subject",
        "review-ladder",
        "show",
        "--json",
    ]));
    assert_eq!(value["version"][0]["id"], "alternate");
}

/// Claim: an explicit selection cannot publish into a closed change's checkout.
/// Rejects retaining only the branch check when current-open-change selection
/// supplies the lifecycle boundary.
#[test]
fn import_refuses_an_explicit_closed_change_even_on_its_branch() {
    let repo = Repo::new();
    let worktree = open(&repo, "closed");
    repo.arc(&worktree)
        .args(["close", "closed", "--abandoned"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["--change", "closed", "review-ladder", "import"])
        .arg(input(&repo, "closed"))
        .assert()
        .failure()
        .stderr(predicates::str::contains("current open change"));
    assert!(!worktree.join(".arc/review-ladder.toml").exists());
}

fn committed_subject(repo: &Repo, slug: &str) -> PathBuf {
    let worktree = open(repo, slug);
    fs::write(worktree.join("work.txt"), "fixture work").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-m", "test: subject work"]);
    repo.arc(&worktree)
        .args(["--model", "author#high", "snapshot", slug])
        .assert()
        .success();
    worktree
}

/// Claim: state, review, status and integration preflight share the target's
/// committed ladder context. Rejects a candidate override or missing wiring.
#[test]
fn review_context_is_shared_by_state_review_and_integration_preflight() {
    let repo = Repo::new();
    commit_registry(&repo, "target");
    let worktree = committed_subject(&repo, "context");
    import(&repo, &worktree, &input(&repo, "candidate"));
    git(&worktree, &["add", ".arc/review-ladder.toml"]);
    git(&worktree, &["commit", "-m", "test: candidate registry"]);
    repo.arc(&worktree)
        .args(["snapshot", "context"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            "reader#high",
            "review",
            "context",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    for args in [
        vec!["show", "context", "--json"],
        vec!["status", "context", "--json"],
        vec!["review", "context", "--json"],
        vec!["check", "context", "--json"],
    ] {
        let value = json_stdout(repo.arc(&worktree).args(args));
        assert_eq!(
            value["review_context"]["authorizing"]["version_ids"],
            json!(["target"])
        );
        assert_eq!(
            value["review_context"]["accepted"]["version_ids"],
            json!(["target"])
        );
    }
}

/// Claim: post-integration audit projection follows the accepted target head,
/// while the integration authority remains at its closure basis.
#[test]
fn audit_context_follows_the_accepted_registry_without_moving_authorization() {
    let repo = super::review_clearance::assessed_repo();
    commit_registry(&repo, "before");
    let worktree = committed_subject(&repo, "audit-context");
    repo.arc(&worktree)
        .args(["integrate", "audit-context", "--debt", "fixture unread"])
        .assert()
        .success();
    let file = repo.root.join(".arc/review-ladder.toml");
    let bytes = fs::read_to_string(&file).unwrap();
    fs::write(&file, bytes.replace("before", "after")).unwrap();
    git(&repo.root, &["add", ".arc/review-ladder.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: accepted registry advances"],
    );
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            "reader#high",
            "audit",
            "audit-context",
            "--verdict",
            "approved",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("still owes"));
    let value = json_stdout(
        repo.arc(&worktree)
            .args(["show", "audit-context", "--json"]),
    );
    assert_eq!(
        value["review_context"]["authorizing"]["version_ids"],
        json!(["before"])
    );
    assert_eq!(
        value["review_context"]["accepted"]["version_ids"],
        json!(["after"])
    );
    assert!(value["debt"]["discharged_by"].is_null());
}
