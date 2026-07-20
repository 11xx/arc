use super::common::*;

#[test]
fn findings_sarif_and_reviewer_checklist_are_role_scoped() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[review]\nchecklist = [\"exercise the failure path\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: add review checklist"]);
    stdout(repo.arc(&repo.root).args(["begin", "sarif-view"]));
    let wt = repo.home.join(".worktrees/repo-sarif-view");
    repo.commit(&wt, "bug.rs", "broken\n", "feat: add reviewed file");
    stdout(repo.arc(&wt).args(["snapshot", "sarif-view"]));
    repo.arc(&wt)
        .args([
            "finding",
            "sarif-view",
            "--summary",
            "broken path",
            "--blocking",
            "--severity",
            "major",
            "--path",
            "bug.rs",
            "--line",
            "1",
        ])
        .assert()
        .success();
    let output = repo
        .arc(&wt)
        .args(["findings", "sarif-view", "--format", "sarif"])
        .output()
        .unwrap();
    let sarif: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "arc");
    assert_eq!(sarif["runs"][0]["results"][0]["level"], "error");
    repo.arc(&wt)
        .env("ARC_ROLE", "implementer")
        .args(["show", "sarif-view"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Review checklist").not());
    repo.arc(&wt)
        .env("ARC_ROLE", "lead")
        .args(["show", "sarif-view"])
        .assert()
        .success()
        .stdout(predicates::str::contains("exercise the failure path"));
}
