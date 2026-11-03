//! `close --assert-integrated --correspondence`: a change whose commits
//! reached the target as other commits arc did not produce.

use crate::common::*;
use serde_json::{json, Value};

/// A change with two commits on separate files, snapshotted. Returns the
/// change id and its two commits, oldest first.
fn two_commit_change(repo: &Repo, slug: &str) -> (String, [String; 2]) {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    let first = repo.head(&worktree);
    repo.commit(&worktree, "two.txt", "two\n", "feat: two");
    let second = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    (change_id, [first, second])
}

/// A branch `name` from master's tip after one unrelated commit, so every
/// commit picked onto it gets a new id. Returns the revision it starts at.
fn landing_branch(repo: &Repo, name: &str) -> String {
    git(&repo.root, &["checkout", "-q", "-b", name, "master"]);
    repo.commit(
        &repo.root,
        &format!("{name}.txt"),
        "x\n",
        "chore: unrelated",
    );
    repo.head(&repo.root)
}

fn pick(repo: &Repo, commit: &str) -> String {
    git(&repo.root, &["cherry-pick", commit]);
    repo.head(&repo.root)
}

fn write_file(repo: &Repo, name: &str, body: &Value) -> String {
    let path = repo.home.join(name);
    fs::write(&path, serde_json::to_vec_pretty(body).unwrap()).unwrap();
    path.to_str().unwrap().to_string()
}

fn correspondence(
    change: &str,
    head: &str,
    before: &str,
    revision: &str,
    pairs: &[(&str, &str)],
) -> Value {
    json!({
        "schema": "arc-correspondence/1",
        "change": change,
        "patchset": "ps-01",
        "source_head": head,
        "target_before": before,
        "revision": revision,
        "commits": pairs
            .iter()
            .map(|(source, landed)| json!({"source": source, "landed": landed}))
            .collect::<Vec<_>>(),
    })
}

fn close(
    repo: &Repo,
    change: &str,
    into: &str,
    before: &str,
    revision: &str,
    file: &str,
) -> std::process::Output {
    repo.arc(&repo.root)
        .args([
            "close",
            change,
            "--assert-integrated",
            revision,
            "--into",
            into,
            "--target-before",
            before,
            "--correspondence",
            file,
        ])
        .output()
        .unwrap()
}

fn refused_naming(output: &std::process::Output, needles: &[&str]) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "accepted: {stderr}");
    for needle in needles {
        assert!(stderr.contains(needle), "{needle:?} not in: {stderr}");
    }
}

fn still_open(repo: &Repo, change: &str) {
    let status = json_stdout_any_status(repo.arc(&repo.root).args(["status", change]));
    assert!(status["closure"].is_null(), "{status}");
}

/// Claim: a correspondence must map every commit of the patchset; one
/// source commit left unmapped refuses the closure and names that commit.
/// Rejects a partial map closing the whole change as integrated.
#[test]
fn an_unmapped_source_commit_refuses_naming_it() {
    let repo = Repo::new();
    let (change, [first, second]) = two_commit_change(&repo, "picked");
    let before = landing_branch(&repo, "picked-onto");
    let landed_first = pick(&repo, &first);
    let file = write_file(
        &repo,
        "partial.json",
        &correspondence(
            &change,
            &second,
            &before,
            &landed_first,
            &[(&first, &landed_first)],
        ),
    );
    let output = close(&repo, &change, "picked-onto", &before, &landed_first, &file);
    refused_naming(&output, &[&second, "unmapped"]);
    still_open(&repo, &change);
}

/// Claim: a complete, ordered, patch-equal correspondence closes the change
/// as asserted, never via a landing and with no gate evidence; `status`,
/// `show`, `explain` and the event say what was asserted, and an export
/// carries it to another store unchanged. Rejects a closure that drops the
/// correspondence or reads as guarded.
#[test]
fn a_complete_ordered_correspondence_closes_asserted_and_shows_it() {
    let repo = Repo::new();
    let (change, [first, second]) = two_commit_change(&repo, "picked");
    let before = landing_branch(&repo, "picked-onto");
    let landed_first = pick(&repo, &first);
    let landed_second = pick(&repo, &second);
    let file = write_file(
        &repo,
        "full.json",
        &correspondence(
            &change,
            &second,
            &before,
            &landed_second,
            &[(&first, &landed_first), (&second, &landed_second)],
        ),
    );
    let output = close(
        &repo,
        &change,
        "picked-onto",
        &before,
        &landed_second,
        &file,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let closure =
        json_stdout(repo.arc(&repo.root).args(["status", &change, "--json"]))["closure"].clone();
    assert_eq!(closure["integration"], "asserted", "{closure}");
    assert!(closure["landing"].is_null(), "{closure}");
    assert!(closure["authorization"].is_null(), "{closure}");
    assert_eq!(closure["target_before"], before.as_str());
    assert_eq!(closure["integrated_commit"], landed_second.as_str());
    let expected = json!([
        {"source": first, "landed": landed_first},
        {"source": second, "landed": landed_second},
    ]);
    assert_eq!(closure["correspondence"], expected, "{closure}");

    let shown = stdout(repo.arc(&repo.root).args(["show", &change]));
    assert!(shown.contains("asserted correspondence"), "{shown}");
    assert!(
        shown.contains(&format!("`{}` -> `{}`", &first[..12], &landed_first[..12])),
        "{shown}"
    );
    let explained = stdout(repo.arc(&repo.root).args(["explain", &change]));
    assert!(
        explained.contains(&format!("{}->{}", &second[..12], &landed_second[..12])),
        "{explained}"
    );

    let bundle = repo.home.join("picked.json");
    repo.arc(&repo.root)
        .args(["export", &change, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let recipient = Repo::new();
    deliver_git_objects(&repo, &recipient);
    let imported = recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let arrived = json_stdout(
        recipient
            .arc(&recipient.root)
            .args(["status", &change, "--json"]),
    )["closure"]
        .clone();
    assert_eq!(arrived["integration"], "asserted", "{arrived}");
    assert_eq!(arrived["correspondence"], expected, "{arrived}");
    assert_eq!(arrived["target_before"], before.as_str());
}

/// Claim: the landed commits must follow source order along
/// `target-before..revision`, and must lie inside it. Rejects a map whose
/// matches are complete and unique but reversed, or that point at commits
/// the target already held before the asserted introduction.
#[test]
fn landed_commits_out_of_order_or_before_the_boundary_refuse() {
    let repo = Repo::new();
    let (change, [first, second]) = two_commit_change(&repo, "picked");

    let before = landing_branch(&repo, "reversed");
    let landed_second = pick(&repo, &second);
    let landed_first = pick(&repo, &first);
    let file = write_file(
        &repo,
        "reversed.json",
        &correspondence(
            &change,
            &second,
            &before,
            &landed_first,
            &[(&first, &landed_first), (&second, &landed_second)],
        ),
    );
    let output = close(&repo, &change, "reversed", &before, &landed_first, &file);
    refused_naming(&output, &[&landed_second, "source order"]);
    still_open(&repo, &change);

    git(&repo.root, &["checkout", "-q", "-b", "early", "master"]);
    let early_first = pick(&repo, &first);
    let early_second = pick(&repo, &second);
    repo.commit(&repo.root, "later.txt", "later\n", "chore: later");
    let boundary = repo.head(&repo.root);
    repo.commit(&repo.root, "after.txt", "after\n", "chore: after");
    let revision = repo.head(&repo.root);
    let file = write_file(
        &repo,
        "early.json",
        &correspondence(
            &change,
            &second,
            &boundary,
            &revision,
            &[(&first, &early_first), (&second, &early_second)],
        ),
    );
    let output = close(&repo, &change, "early", &boundary, &revision, &file);
    refused_naming(&output, &[&early_first, "outside"]);
    still_open(&repo, &change);
}

/// Claim: each remaining rule refuses and names its commit: a duplicate in
/// either direction, a pair whose patches differ, a source commit with no
/// ordinary patch identity, and a file bound to another revision. Rejects
/// any of them closing the change.
#[test]
fn duplicates_patch_mismatches_empty_commits_and_wrong_bindings_refuse() {
    let repo = Repo::new();
    let (change, [first, second]) = two_commit_change(&repo, "picked");
    let before = landing_branch(&repo, "picked-onto");
    let landed_first = pick(&repo, &first);
    let landed_second = pick(&repo, &second);
    let attempt = |name: &str, pairs: &[(&str, &str)], revision: &str| {
        let file = write_file(
            &repo,
            name,
            &correspondence(&change, &second, &before, revision, pairs),
        );
        close(
            &repo,
            &change,
            "picked-onto",
            &before,
            &landed_second,
            &file,
        )
    };

    let output = attempt(
        "dup-landed.json",
        &[(&first, &landed_second), (&second, &landed_second)],
        &landed_second,
    );
    refused_naming(&output, &[&landed_second, "mapped twice"]);
    let output = attempt(
        "dup-source.json",
        &[(&first, &landed_first), (&first, &landed_second)],
        &landed_second,
    );
    refused_naming(&output, &[&first, "mapped twice"]);
    let output = attempt(
        "swapped.json",
        &[(&first, &before), (&second, &landed_second)],
        &landed_second,
    );
    refused_naming(&output, &[&before, "outside"]);
    let output = attempt(
        "mismatch.json",
        &[(&first, &landed_second), (&second, &landed_first)],
        &landed_second,
    );
    refused_naming(&output, &[&landed_second, "patch"]);
    let output = attempt(
        "binding.json",
        &[(&first, &landed_first), (&second, &landed_second)],
        &landed_first,
    );
    refused_naming(&output, &["revision", &landed_first]);
    still_open(&repo, &change);

    let empty = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "empty"])));
    let worktree = repo.home.join(".worktrees/repo-empty");
    repo.commit(&worktree, "e.txt", "e\n", "feat: e");
    let real = repo.head(&worktree);
    git(
        &worktree,
        &["commit", "-q", "--allow-empty", "-m", "chore: nothing"],
    );
    let hollow = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", "empty"]));
    git(&repo.root, &["checkout", "-q", "picked-onto"]);
    let start = repo.head(&repo.root);
    let landed = pick(&repo, &real);
    let file = write_file(
        &repo,
        "hollow.json",
        &correspondence(&empty, &hollow, &start, &landed, &[(&real, &landed)]),
    );
    let output = close(&repo, &empty, "picked-onto", &start, &landed, &file);
    refused_naming(&output, &[&hollow, "changes nothing"]);
}

/// Claim: without `--correspondence`, `--assert-integrated` keeps requiring
/// the asserted revision to contain the source head, and `--correspondence`
/// needs `--target-before`. Rejects a cherry-picked revision closing the
/// change on its own word.
#[test]
fn without_a_correspondence_a_cherry_pick_still_refuses() {
    let repo = Repo::new();
    let (change, [first, second]) = two_commit_change(&repo, "picked");
    landing_branch(&repo, "picked-onto");
    pick(&repo, &first);
    let revision = pick(&repo, &second);
    let output = repo
        .arc(&repo.root)
        .args([
            "close",
            &change,
            "--assert-integrated",
            &revision,
            "--into",
            "picked-onto",
        ])
        .output()
        .unwrap();
    refused_naming(&output, &["is not an integration of this change"]);
    let output = repo
        .arc(&repo.root)
        .args([
            "close",
            &change,
            "--assert-integrated",
            &revision,
            "--correspondence",
            "whatever.json",
        ])
        .output()
        .unwrap();
    refused_naming(&output, &["--target-before"]);
    still_open(&repo, &change);
}
