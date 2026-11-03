//! A complete pass at an unchanged tree answers for every later invocation:
//! `verify` reuses it unless told to `--rerun`, and records the reuse. A gate
//! that reads commit messages or history declares `binds = "commit"`, and its
//! pass answers only for the commit it ran on.

use crate::common::*;

/// Lines a gate's counter file holds: how many times that gate's command ran.
fn runs(repo: &Repo, gate: &str) -> usize {
    fs::read_to_string(repo.home.join(format!("{gate}.runs")))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// A command that records its own run in a counter outside every worktree,
/// then succeeds only when `extra` does.
fn counting(repo: &Repo, gate: &str, extra: &str) -> String {
    let counter = repo.home.join(format!("{gate}.runs"));
    format!("echo run >> '{}' && {extra}", counter.display())
}

/// A declaration's `command` line for [`counting`].
fn command(repo: &Repo, gate: &str, extra: &str) -> String {
    format!("command = {:?}", counting(repo, gate, extra))
}

/// Declare gates in the operator policy, so redeclaring them moves no tree.
fn declare(repo: &Repo, gates: &[(&str, String)]) {
    let text: String = gates
        .iter()
        .map(|(name, body)| format!("[gates.{name}]\n{body}\n"))
        .collect();
    repo.declare_gates_locally(&text);
}

fn reuse_events(repo: &Repo, worktree: &Path, change: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(worktree).args([
        "events",
        "--change",
        change,
        "--type",
        "verification-reused",
    ]))
    .lines()
    .filter(|line| !line.trim().is_empty())
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

fn gate_green(repo: &Repo, worktree: &Path, change: &str, gate: &str) -> bool {
    let status = json_stdout(repo.arc(worktree).args(["status", change]));
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == gate)
        .unwrap_or_else(|| panic!("no gate {gate} in {status}"))["green_at_head"]
        == true
}

/// Claim: a second `verify --all` at an unchanged tree runs no gate and
/// records `verification-reused` for each. Rejects rerunning a complete pass
/// at the same tree, declaration and environment because the caller did not
/// ask for reuse.
#[test]
fn a_second_verify_all_at_an_unchanged_tree_reuses_and_runs_nothing() {
    let repo = Repo::new();
    declare(
        &repo,
        &[
            ("build", command(&repo, "build", "true")),
            ("test", command(&repo, "test", "true")),
        ],
    );
    let (id, worktree, _) = change_with_patchset(&repo, "unchanged");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "test")), (1, 1));

    let second = stdout(repo.arc(&worktree).args(["verify", "--all"]));
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "test")),
        (1, 1),
        "an unchanged tree ran its gates again:\n{second}"
    );
    assert!(second.contains("build: skipped (green at head"), "{second}");
    assert!(second.contains("gates: 2/2 pass"), "{second}");
    assert_eq!(reuse_events(&repo, &worktree, &id).len(), 2);
}

/// Claim: one named gate already green at the tree is reused as well. Rejects
/// a reuse rule that only a whole-set run consults.
#[test]
fn a_named_gate_green_at_the_tree_is_reused() {
    let repo = Repo::new();
    declare(&repo, &[("build", command(&repo, "build", "true"))]);
    let (id, worktree, _) = change_with_patchset(&repo, "named");
    repo.arc(&worktree)
        .args(["verify", "--gate", "build"])
        .assert()
        .success();
    let second = stdout(repo.arc(&worktree).args(["verify", "--gate", "build"]));
    assert_eq!(runs(&repo, "build"), 1, "{second}");
    assert!(second.contains("build: skipped (green at head"), "{second}");
    assert_eq!(reuse_events(&repo, &worktree, &id).len(), 1);
}

/// Claim: `--rerun` runs a gate whose pass at this tree would otherwise be
/// reused. Rejects a flag that is accepted and then ignored.
#[test]
fn rerun_runs_gates_already_green_at_the_tree() {
    let repo = Repo::new();
    declare(&repo, &[("build", command(&repo, "build", "true"))]);
    let (id, worktree, _) = change_with_patchset(&repo, "forced");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "--gate", "build", "--rerun"])
        .assert()
        .success();
    assert_eq!(runs(&repo, "build"), 3);
    assert!(reuse_events(&repo, &worktree, &id).is_empty());
}

/// Claim: `--skip-green` stays accepted and changes nothing, since reuse is
/// the default. Rejects removing the flag scripts already pass.
#[test]
fn skip_green_is_an_accepted_no_op() {
    let repo = Repo::new();
    declare(&repo, &[("build", command(&repo, "build", "true"))]);
    let (_, worktree, _) = change_with_patchset(&repo, "alias");
    repo.arc(&worktree)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "--all", "--skip-green"])
        .assert()
        .success();
    assert_eq!(runs(&repo, "build"), 1);
    repo.arc(&worktree)
        .args(["verify", "--all", "--skip-green", "--rerun"])
        .assert()
        .failure();
}

/// Claim: a gate whose declaration changed since its pass runs again, while
/// an unchanged neighbour is reused. Rejects reuse keyed by gate name and
/// tree alone.
#[test]
fn a_changed_declaration_reruns() {
    let repo = Repo::new();
    let build = counting(&repo, "build", "true");
    let test = counting(&repo, "test", "true");
    declare(
        &repo,
        &[
            ("build", format!("command = {build:?}")),
            ("test", format!("command = {test:?}")),
        ],
    );
    let (_, worktree, _) = change_with_patchset(&repo, "redeclared");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    declare(
        &repo,
        &[
            ("build", format!("command = {build:?}")),
            ("test", format!("command = {:?}", format!("{test} && true"))),
        ],
    );
    let second = stdout(repo.arc(&worktree).args(["verify", "--all"]));
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "test")),
        (1, 2),
        "{second}"
    );
}

/// Claim: a gate whose environment probe now yields another identity, or
/// none, runs again, while a gate without a probe is reused. Rejects reuse
/// that ignores the environment the pass was produced in.
#[test]
fn a_changed_or_unknown_environment_reruns() {
    let repo = Repo::new();
    let marker = repo.home.join("environment");
    fs::write(&marker, "one\n").unwrap();
    declare(
        &repo,
        &[
            ("build", command(&repo, "build", "true")),
            (
                "test",
                format!(
                    "command = {:?}\nenvironment = {:?}",
                    counting(&repo, "test", "true"),
                    format!("cat '{}'", marker.display())
                ),
            ),
        ],
    );
    let (_, worktree, _) = change_with_patchset(&repo, "moved-env");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();

    // The same environment reuses.
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "test")), (1, 1));

    fs::write(&marker, "two\n").unwrap();
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "test")), (1, 2));

    // A probe that yields no identity says nothing about where the pass ran.
    fs::remove_file(&marker).unwrap();
    let _ = repo
        .arc(&worktree)
        .args(["verify", "--all"])
        .output()
        .unwrap();
    assert_eq!((runs(&repo, "build"), runs(&repo, "test")), (1, 3));
}

/// Claim: a gate whose newest run failed, or was not admitted, runs again,
/// even with an older pass at the same tree; once it passes, it is reused.
/// Rejects reusing a pass the newest evidence superseded.
#[test]
fn a_failing_or_unadmitted_newest_run_reruns() {
    let repo = Repo::new();
    let outcome = repo.home.join("outcome");
    fs::write(&outcome, "0").unwrap();
    declare(
        &repo,
        &[
            ("build", command(&repo, "build", "true")),
            (
                "test",
                format!(
                    "command = {:?}",
                    counting(
                        &repo,
                        "test",
                        &format!("exit $(cat '{}')", outcome.display())
                    )
                ),
            ),
        ],
    );
    let (_, worktree, _) = change_with_patchset(&repo, "superseded");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(runs(&repo, "test"), 1);

    fs::write(&outcome, "1").unwrap();
    repo.arc(&worktree)
        .args(["verify", "--gate", "test", "--rerun"])
        .assert()
        .failure();
    assert_eq!(runs(&repo, "test"), 2);

    // The newest run failed, so the older pass is no candidate.
    fs::write(&outcome, "75").unwrap();
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .failure();
    assert_eq!(runs(&repo, "test"), 3);

    // Nor is it behind a run that was not admitted.
    fs::write(&outcome, "0").unwrap();
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(runs(&repo, "test"), 4);

    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "test")),
        (1, 4),
        "a pass that is the newest evidence is reused"
    );
}

/// Claim: a gate declaring `binds = "commit"` is not green after `arc
/// rewrite trailers` makes a new commit around the same tree, and `verify`
/// runs it there, while an ordinary gate stays green and is reused. Rejects
/// a pass for one commit answering for another that holds the same tree,
/// for a gate that reads commit messages.
#[test]
fn a_commit_bound_gate_reruns_after_a_trailer_rewrite_at_the_same_tree() {
    let repo = Repo::new();
    declare(
        &repo,
        &[
            ("build", command(&repo, "build", "true")),
            (
                "history",
                format!(
                    "command = {:?}\nbinds = \"commit\"",
                    counting(&repo, "history", "true")
                ),
            ),
        ],
    );
    let (_, worktree, verified) = change_with_patchset(&repo, "reworded");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    let tree = git_out(&worktree, &["rev-parse", "HEAD^{tree}"]);

    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Reviewed-by: Reviewer <reviewer@example.invalid>",
            "--from",
            &verified,
        ])
        .assert()
        .success();
    assert_ne!(repo.head(&worktree), verified);
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD^{tree}"]), tree);

    assert!(gate_green(&repo, &worktree, "reworded", "build"));
    assert!(
        !gate_green(&repo, &worktree, "reworded", "history"),
        "a commit-bound pass answered for another commit"
    );

    let rerun = stdout(repo.arc(&worktree).args(["verify", "--all"]));
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "history")),
        (1, 2),
        "{rerun}"
    );
    assert!(gate_green(&repo, &worktree, "reworded", "build"));
    assert!(gate_green(&repo, &worktree, "reworded", "history"));

    // At its own commit, a commit-bound pass is reused like any other.
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 2));
}
