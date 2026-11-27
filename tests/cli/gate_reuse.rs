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

/// Claim: a gate declaring `binds = "commit"` stays green and is reused across
/// `role_additions` trailer appends (`arc rewrite trailers --append`), but is
/// invalidated and reruns after a non-role trailer edit or a new same-tree
/// commit, while an ordinary tree-bound gate stays green across all of them.
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

    // Appending a role trailer produces a `role_additions` rewrite: both the
    // tree-bound gate and the commit-bound gate stay green and are reused.
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
    let after_role = repo.head(&worktree);
    assert_ne!(after_role, verified);
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD^{tree}"]), tree);
    assert!(gate_green(&repo, &worktree, "reworded", "build"));
    assert!(
        gate_green(&repo, &worktree, "reworded", "history"),
        "a commit-bound pass must carry across a role-addition rewrite"
    );
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "history")),
        (1, 1),
        "role-addition rewrite must reuse commit-bound evidence"
    );

    // A non-role trailer edit invalidates the commit-bound gate while keeping
    // the tree-bound gate green.
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Assisted-by: helper:editor#high",
            "--from",
            &after_role,
        ])
        .assert()
        .success();
    assert_ne!(repo.head(&worktree), after_role);
    assert_eq!(git_out(&worktree, &["rev-parse", "HEAD^{tree}"]), tree);

    assert!(gate_green(&repo, &worktree, "reworded", "build"));
    assert!(
        !gate_green(&repo, &worktree, "reworded", "history"),
        "a commit-bound pass answered across a non-role rewrite"
    );

    let rerun = stdout(repo.arc(&worktree).args(["verify", "--all"]));
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "history")),
        (1, 2),
        "{rerun}"
    );
    assert!(gate_green(&repo, &worktree, "reworded", "build"));
    assert!(gate_green(&repo, &worktree, "reworded", "history"));

    // A new empty commit at the same tree also invalidates the commit-bound gate.
    git(
        &worktree,
        &["commit", "--allow-empty", "-m", "chore: same tree"],
    );
    assert!(gate_green(&repo, &worktree, "reworded", "build"));
    assert!(
        !gate_green(&repo, &worktree, "reworded", "history"),
        "a commit-bound pass answered for a new empty commit"
    );
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 3));

    // At its own commit, a commit-bound pass is reused like any other.
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 3));
}

/// Claim: a gate declaring `binds = "commit"` stays green and is reused across
/// a `signature_only` signing rewrite (`arc rewrite sign`).
#[test]
fn a_commit_bound_gate_carries_across_signature_only_rewrites() {
    let repo = Repo::new();
    let key = crate::rewrite::signing_key(&repo)
        .expect("signing acceptance requires a sandbox signing key");
    declare(
        &repo,
        &[(
            "history",
            format!(
                "command = {:?}\nbinds = \"commit\"",
                counting(&repo, "history", "true")
            ),
        )],
    );
    let (_, worktree, verified) = change_with_patchset(&repo, "signed-commit-gate");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(runs(&repo, "history"), 1);

    crate::rewrite::arc_signing(&repo, &key, &worktree)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .assert()
        .success();
    assert_ne!(repo.head(&worktree), verified);
    assert!(
        gate_green(&repo, &worktree, "signed-commit-gate", "history"),
        "a commit-bound pass must carry across a signature-only rewrite"
    );
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!(runs(&repo, "history"), 1);
}

fn check_gate_row(repo: &Repo, worktree: &Path, change: &str, gate: &str) -> serde_json::Value {
    let check = json_stdout_any_status(repo.arc(worktree).args(["check", change, "--json"]));
    check["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == gate)
        .unwrap_or_else(|| panic!("no gate {gate} in {check}"))
        .clone()
}

/// Claim: carried commit-bound head evidence requires destination provenance,
/// so a same-tree empty commit on the target stops a role-addition rewrite
/// from carrying the stale head pass.
#[test]
fn a_commit_bound_carry_rejects_a_same_tree_target_move() {
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
    let (id, worktree, _) = change_with_patchset(&repo, "target-empty-move");
    repo.arc(&worktree)
        .args(["snapshot", &id, "--solo", "--model", "worker#medium"])
        .env("ARC_HARNESS", "codex")
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 1));

    // Move target without changing its tree, then rewrite the change head with
    // a role-only sync so the merged tree still equals the head tree.
    git(
        &repo.root,
        &["commit", "--allow-empty", "-m", "chore: empty target move"],
    );
    repo.arc(&worktree)
        .args(["rewrite", "trailers", "--sync", "--no-sign"])
        .assert()
        .success();

    let build = check_gate_row(&repo, &worktree, &id, "build");
    assert_eq!(build["result"], "pass", "{build}");
    assert_eq!(build["green_at_head"], true, "{build}");

    let history = check_gate_row(&repo, &worktree, &id, "history");
    assert_eq!(
        history["result"], "pending",
        "same-tree target move must invalidate carried commit-bound evidence: {history}"
    );
    assert_eq!(history["green_at_head"], false, "{history}");

    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 2));
}

/// Claim: a commit-bound synthesized-merge pass (`verify --against <target>`)
/// carries across a `role_additions` rewrite of the change head while the
/// destination stays fixed, and invalidates on a non-role rewrite or any
/// target move (including a same-tree empty commit).
#[test]
fn a_commit_bound_synthesized_merge_carries_across_role_additions_only_for_unchanged_target() {
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
    let (id, worktree, verified_head) = change_with_patchset(&repo, "merge-carry");

    // Advance the target with a real content change and verify the merge.
    repo.commit(&repo.root, "other.txt", "target\n", "feat: target content");
    repo.arc(&worktree)
        .args(["verify", "--against", "master"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 1));
    assert_eq!(
        check_gate_row(&repo, &worktree, &id, "history")["green_at_head"],
        true
    );

    // With master held fixed, append a complete role trailer at the source head:
    // the merged tree is unchanged, so both check and verify --against reuse
    // must keep the commit-bound synthesized-merge pass green.
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Reviewed-by: Reviewer <reviewer@example.invalid>",
            "--from",
            &verified_head,
        ])
        .assert()
        .success();
    let after_role = repo.head(&worktree);
    assert_ne!(after_role, verified_head);

    let history = check_gate_row(&repo, &worktree, &id, "history");
    assert_eq!(history["result"], "pass", "{history}");
    assert_eq!(
        history["green_at_head"], true,
        "role-addition rewrite with fixed target must preserve synthesized-merge pass: {history}"
    );
    repo.arc(&worktree)
        .args(["verify", "--against", "master", "--skip-green"])
        .assert()
        .success();
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "history")),
        (1, 1),
        "verify --against --skip-green must reuse carried synthesized-merge pass"
    );

    // A non-role rewrite with master held fixed invalidates the commit-bound
    // synthesized-merge pass while keeping the tree-bound pass green.
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Assisted-by: helper:editor#high",
            "--from",
            &after_role,
        ])
        .assert()
        .success();
    assert_eq!(
        check_gate_row(&repo, &worktree, &id, "build")["green_at_head"],
        true
    );
    let after_non_role = check_gate_row(&repo, &worktree, &id, "history");
    assert_eq!(after_non_role["result"], "pending", "{after_non_role}");
    assert_eq!(after_non_role["green_at_head"], false, "{after_non_role}");
    repo.arc(&worktree)
        .args(["verify", "--against", "master", "--skip-green"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 2));

    // A same-tree empty commit on master moves the destination without moving
    // the merged tree: the tree-bound gate stays green, while the commit-bound
    // synthesized-merge pass invalidates and reruns.
    git(
        &repo.root,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "chore: empty target advance",
        ],
    );
    assert_eq!(
        check_gate_row(&repo, &worktree, &id, "build")["green_at_head"],
        true
    );
    let after_target_move = check_gate_row(&repo, &worktree, &id, "history");
    assert_eq!(
        after_target_move["result"], "pending",
        "empty target advance must invalidate commit-bound merge evidence: {after_target_move}"
    );
    assert_eq!(
        after_target_move["green_at_head"], false,
        "{after_target_move}"
    );
    repo.arc(&worktree)
        .args(["verify", "--against", "master", "--skip-green"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 3));
}

/// Claim: carried direct commit-bound evidence compares exact recorded
/// destination equality rather than ancestry alone, so moving `master` back to
/// an older ancestor `A` stales the carried pass, `verify --against older`
/// executes rather than skipping, and records without destination provenance
/// (such as attested passes) conservatively do not carry across rewrites.
#[test]
fn a_commit_bound_carry_rejects_target_rollback_and_against_older_ancestor() {
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
    let rev_a = repo.head(&repo.root);
    git(&repo.root, &["branch", "older", &rev_a]);
    git(
        &repo.root,
        &["commit", "--allow-empty", "-m", "chore: empty B on master"],
    );
    let rev_b = repo.head(&repo.root);
    assert_ne!(rev_a, rev_b);

    let (id, worktree, verified_head) = change_with_patchset(&repo, "target-rollback");
    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();
    assert_eq!((runs(&repo, "build"), runs(&repo, "history")), (1, 1));
    repo.arc(&worktree)
        .args(["review", &id, "--verdict", "approved"])
        .env("ARC_ACTOR", "Reviewer <reviewer@example.invalid>")
        .assert()
        .success();

    // Append a complete role trailer so the commit-bound pass is carried.
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Reviewed-by: Reviewer <reviewer@example.invalid>",
            "--from",
            &verified_head,
        ])
        .assert()
        .success();
    assert_ne!(repo.head(&worktree), verified_head);

    // At the original destination B, the carried pass answers and check is ready.
    assert!(gate_green(&repo, &worktree, &id, "history"));
    let ready_at_b = json_stdout(repo.arc(&worktree).args(["check", &id, "--json"]));
    assert_eq!(ready_at_b["ready"], true, "{ready_at_b}");

    // Moving master backwards to older ancestor A must stale the carried pass.
    git(&repo.root, &["reset", "--hard", &rev_a]);
    assert_eq!(repo.head(&repo.root), rev_a);
    assert!(
        !gate_green(&repo, &worktree, &id, "history"),
        "rolling master back to older ancestor A must stale carried commit-bound pass"
    );
    let check_at_a = repo
        .arc(&worktree)
        .args(["check", &id, "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let check_at_a: serde_json::Value = serde_json::from_slice(&check_at_a).unwrap();
    assert_eq!(check_at_a["ready"], false, "{check_at_a}");
    let history_at_a = check_gate_row(&repo, &worktree, &id, "history");
    assert_eq!(history_at_a["result"], "pending", "{history_at_a}");
    assert_eq!(history_at_a["green_at_head"], false, "{history_at_a}");

    // Restore master to B: carried pass answers at B, but `verify --against older --skip-green`
    // (selecting ancestor A) must execute rather than reusing the pass recorded against B.
    git(&repo.root, &["reset", "--hard", &rev_b]);
    assert!(gate_green(&repo, &worktree, &id, "history"));
    repo.arc(&worktree)
        .args(["verify", "--against", "older", "--skip-green"])
        .assert()
        .success();
    assert_eq!(
        (runs(&repo, "build"), runs(&repo, "history")),
        (1, 2),
        "verify --against older must execute commit-bound gate instead of reusing pass recorded at B"
    );

    // Conservative fallback: attested evidence carries no observed destination
    // revision, so it does not carry across a role-addition rewrite.
    let (attested_id, attested_wt, attested_head) =
        change_with_patchset(&repo, "attested-no-carry");
    repo.arc(&attested_wt)
        .args([
            "verify",
            &attested_id,
            "--gate",
            "history",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &attested_head,
            "--execution-host",
            "ci-host",
            "--runner",
            "ci-runner",
        ])
        .assert()
        .success();
    assert!(gate_green(&repo, &attested_wt, &attested_id, "history"));
    repo.arc(&attested_wt)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Reviewed-by: Reviewer <reviewer@example.invalid>",
            "--from",
            &attested_head,
        ])
        .assert()
        .success();
    assert!(
        !gate_green(&repo, &attested_wt, &attested_id, "history"),
        "evidence without recorded destination provenance must not carry across rewrites"
    );
}

/// Claim: direct verification pins its recorded `target_revision` against Git
/// garbage collection so rolling the target back, pruning unreferenced objects,
/// exporting the change bundle, and importing into a fresh receiver all succeed.
#[test]
fn a_direct_verification_target_revision_survives_gc_and_imports_cleanly() {
    let repo = Repo::new();
    declare(
        &repo,
        &[(
            "history",
            format!(
                "command = {:?}\nbinds = \"commit\"",
                counting(&repo, "history", "true")
            ),
        )],
    );
    let rev_a = repo.head(&repo.root);
    let (id, worktree, _) = change_with_patchset(&repo, "gc-target-rev");

    // Advance master to a sibling commit B before running the direct gate.
    repo.commit(&repo.root, "sibling.txt", "sibling\n", "feat: sibling B");
    let rev_b = repo.head(&repo.root);
    assert_ne!(rev_a, rev_b);

    repo.arc(&worktree)
        .args(["verify", "--all"])
        .assert()
        .success();

    // Roll master back to A, expire reflogs, and aggressively prune objects.
    git(&repo.root, &["reset", "--hard", &rev_a]);
    git(&repo.root, &["reflog", "expire", "--expire=now", "--all"]);
    git(&repo.root, &["gc", "--prune=now"]);
    assert_eq!(
        git_out(&repo.root, &["cat-file", "-t", &rev_b]),
        "commit",
        "recorded target_revision must remain pinned against git gc"
    );

    // Export the bundle, deliver Git objects, and import into a fresh receiver.
    let bundle = repo.home.join("gc-target-rev.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    let receiver = Repo::new();
    deliver_git_objects(&repo, &receiver);
    stdout(
        receiver
            .arc(&receiver.root)
            .args(["import", bundle.to_str().unwrap()]),
    );

    // Receiver must also retain target_revision even after delivery refs are
    // removed and git gc runs.
    let delivered = git_out(
        &receiver.root,
        &["for-each-ref", "--format=%(refname)", "refs/delivered/"],
    );
    for refname in delivered.lines().filter(|line| !line.is_empty()) {
        git(&receiver.root, &["update-ref", "-d", refname]);
    }
    git(
        &receiver.root,
        &["reflog", "expire", "--expire=now", "--all"],
    );
    git(&receiver.root, &["gc", "--prune=now"]);
    assert_eq!(
        git_out(&receiver.root, &["cat-file", "-t", &rev_b]),
        "commit",
        "receiver import must retain target_revision against git gc"
    );
}
