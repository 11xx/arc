//! External verdicts: a decision made outside arc, recorded with its source
//! and the exact revision it covered, and never mistaken for a verdict arc
//! witnessed.

use crate::common::*;

fn repo_with_gates(policy: Option<&str>) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"true\"\n",
    )
    .unwrap();
    let mut paths = vec![".arc/gates.toml"];
    if let Some(policy) = policy {
        fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
        paths.push(".arc/policy.toml");
    }
    let mut args = vec!["add"];
    args.extend(paths);
    git(&repo.root, &args);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

/// A change with one commit, a patchset, and green gates, and no verdict.
fn gated_change(repo: &Repo, slug: &str, file: &str) -> (String, PathBuf, String) {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    fs::write(worktree.join(file), "content\n").unwrap();
    git(&worktree, &["add", "--", file]);
    git(&worktree, &["commit", "-m", &format!("feat: {slug}")]);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["verify", slug, "--gate", "smoke"])
        .assert()
        .success();
    let head = git_out(&worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    (change_id, worktree, head)
}

fn record(repo: &Repo, cwd: &Path, slug: &str, verdict: &str, revision: &str) {
    repo.arc(cwd)
        .args([
            "external",
            "verdict",
            slug,
            "--verdict",
            verdict,
            "--decided-by",
            "Upstream Maintainer",
            "--reference",
            "https://example.invalid/pull/7",
            "--revision",
            revision,
        ])
        .assert()
        .success();
}

#[test]
fn an_external_approval_at_the_head_gates_and_is_recorded_as_external() {
    let repo = repo_with_gates(None);
    let (_, worktree, head) = gated_change(&repo, "ext-ok", "ok.txt");
    record(&repo, &worktree, "ext-ok", "approved", &head);

    let status = json_stdout(repo.arc(&worktree).args(["status", "ext-ok", "--json"]));
    let external = &status["external_verdicts"][0];
    assert_eq!(external["source"], "external", "{status}");
    assert_eq!(external["gates_current_head"], true, "{status}");
    assert_eq!(status["has_valid_approval"], true, "{status}");
    repo.arc(&worktree)
        .args(["check", "ext-ok"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["integrate", "ext-ok"])
        .assert()
        .success();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 4, "{config}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "ext-ok", "--json"]));
    assert_eq!(
        status["closure"]["authorization"]["external_verdict"]["reference"],
        "https://example.invalid/pull/7",
        "{status}"
    );
}

#[test]
fn audit_external_approval_preserves_a_local_refusal_until_local_approval() {
    for refusal in ["changes-requested", "comment-only"] {
        for external_first in [false, true] {
            let repo = repo_with_gates(None);
            let (_, worktree, head) = gated_change(&repo, "local-refusal", "ok.txt");
            if external_first {
                record(&repo, &worktree, "local-refusal", "approved", &head);
            }
            let mut review = repo.arc(&worktree);
            review.args(["review", "local-refusal", "--verdict", refusal]);
            if refusal == "changes-requested" {
                review.args(["--cause", "executor"]);
            }
            review.assert().success();
            if !external_first {
                record(&repo, &worktree, "local-refusal", "approved", &head);
            }
            let status = json_stdout(repo.arc(&worktree).args(["status", "--json"]));
            assert_eq!(status["has_valid_approval"], false, "{status}");
            assert_eq!(status["external_verdicts"][0]["gates_current_head"], false);
            repo.arc(&worktree).args(["check"]).assert().code(3);
            repo.arc(&repo.root)
                .args(["integrate", "local-refusal"])
                .assert()
                .code(3);
            repo.arc(&worktree)
                .args(["review", "--verdict", "approved"])
                .assert()
                .success();
            repo.arc(&worktree).args(["check"]).assert().success();
        }
    }
}

#[test]
fn an_external_approval_covers_only_the_revision_it_names() {
    let repo = repo_with_gates(None);
    let (_, worktree, head) = gated_change(&repo, "ext-moved", "moved.txt");
    record(&repo, &worktree, "ext-moved", "approved", &head);

    fs::write(worktree.join("moved.txt"), "changed after the decision\n").unwrap();
    git(&worktree, &["commit", "-am", "fix: after review"]);
    stdout(repo.arc(&worktree).args(["snapshot", "ext-moved"]));
    repo.arc(&worktree)
        .args(["verify", "ext-moved", "--gate", "smoke"])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&worktree).args(["status", "ext-moved", "--json"]));
    assert_eq!(status["has_valid_approval"], false, "{status}");
    assert_eq!(
        status["external_verdicts"][0]["matches_current_patchset"], false,
        "{status}"
    );
    repo.arc(&worktree)
        .args(["check", "ext-moved"])
        .assert()
        .failure();
}

#[test]
fn an_external_approval_alone_never_satisfies_a_dangerous_path() {
    let repo = repo_with_gates(Some(
        "[policy]\nforbid_self_approval = true\n\n[danger]\npaths = [\"danger.txt\"]\n",
    ));
    let (_, worktree, head) = gated_change(&repo, "ext-danger", "danger.txt");
    record(&repo, &worktree, "ext-danger", "approved", &head);

    let status = json_stdout(repo.arc(&worktree).args(["status", "ext-danger", "--json"]));
    assert_eq!(status["external_verdicts"][0]["gates_current_head"], false);
    assert_eq!(status["has_valid_approval"], false, "{status}");
    repo.arc(&worktree)
        .args(["check", "ext-danger"])
        .assert()
        .failure();
}

#[test]
fn an_external_change_request_carries_findings_and_refuses() {
    let repo = repo_with_gates(None);
    let (_, worktree, head) = gated_change(&repo, "ext-changes", "changes.txt");
    let findings = repo.home.join("findings.json");
    fs::write(
        &findings,
        r#"[{"severity":"major","summary":"Handle the empty input"}]"#,
    )
    .unwrap();
    repo.arc(&worktree)
        .args([
            "external",
            "verdict",
            "ext-changes",
            "--verdict",
            "changes-requested",
            "--decided-by",
            "Upstream Maintainer",
            "--reference",
            "https://example.invalid/pull/8",
            "--revision",
            &head,
            "--findings-json",
            findings.to_str().unwrap(),
        ])
        .assert()
        .success();

    let status = json_stdout(
        repo.arc(&worktree)
            .args(["status", "ext-changes", "--json"]),
    );
    assert_eq!(
        status["external_verdicts"][0]["findings"][0]["summary"], "Handle the empty input",
        "{status}"
    );
    repo.arc(&worktree)
        .args(["check", "ext-changes"])
        .assert()
        .failure();
}

#[test]
fn an_external_rejection_of_the_head_closes_the_change() {
    let repo = repo_with_gates(None);
    let (_, worktree, head) = gated_change(&repo, "ext-rejected", "rejected.txt");
    record(&repo, &worktree, "ext-rejected", "rejected", &head);

    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "ext-rejected", "--json"]),
    );
    assert_eq!(status["closure"]["outcome"], "abandoned", "{status}");
    assert_eq!(
        status["closure"]["external_reference"], "https://example.invalid/pull/7",
        "{status}"
    );
}
