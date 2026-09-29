//! Integration judges a change by the target branch's declarations, wherever
//! the command is typed.

use crate::common::*;

const TARGET_GATES: &str = "[gates.smoke]\ncommand = \"test -f README.md\"\n\n\
                            [gates.guard]\ncommand = \"test -f MARKER\"\n";

fn repo_declaring(gates: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), gates).unwrap();
    fs::write(repo.root.join("MARKER"), "marker\n").unwrap();
    git(&repo.root, &["add", ".arc/gates.toml", "MARKER"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

fn open_change(repo: &Repo, slug: &str) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    repo.home.join(".worktrees").join(format!("repo-{slug}"))
}

fn commit_file(worktree: &Path, file: &str, content: &str) {
    fs::create_dir_all(worktree.join(file).parent().unwrap()).unwrap();
    fs::write(worktree.join(file), content).unwrap();
    git(worktree, &["add", "-f", "--", file]);
    git(worktree, &["commit", "-m", &format!("feat: {file}")]);
}

fn snapshot_and_approve(repo: &Repo, worktree: &Path, slug: &str) {
    stdout(repo.arc(worktree).args(["snapshot", slug]));
    repo.arc(worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
}

fn verify(repo: &Repo, worktree: &Path, slug: &str, gate: &str) {
    repo.arc(worktree)
        .args(["verify", slug, "--gate", gate])
        .assert()
        .success();
}

fn status(repo: &Repo, cwd: &Path, slug: &str) -> serde_json::Value {
    serde_json::from_str(&stdout(repo.arc(cwd).args(["status", slug, "--json"]))).unwrap()
}

#[test]
fn a_deleted_gate_is_still_owed_from_the_change_worktree_and_the_target() {
    let repo = repo_declaring(TARGET_GATES);
    let worktree = open_change(&repo, "drops-guard");
    commit_file(
        &worktree,
        ".arc/gates.toml",
        "[gates.smoke]\ncommand = \"test -f README.md\"\n",
    );
    stdout(repo.arc(&worktree).args(["snapshot", "drops-guard"]));
    verify(&repo, &worktree, "drops-guard", "smoke");
    repo.arc(&worktree)
        .args(["review", "drops-guard", "--verdict", "approved"])
        .assert()
        .success();

    for cwd in [&worktree, &repo.root] {
        repo.arc(cwd)
            .args(["integrate", "drops-guard"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("Gate `guard` is not green"));
        repo.arc(cwd)
            .args(["check", "drops-guard"])
            .assert()
            .failure()
            .stdout(predicates::str::contains("Gate `guard` is not green"));
    }

    let ready = |cwd: &Path| status(&repo, cwd, "drops-guard")["integrate_ready"].clone();
    assert_eq!(ready(&worktree), false);
    assert_eq!(ready(&repo.root), false);
}

#[test]
fn status_names_where_the_checkout_and_the_target_disagree() {
    let repo = repo_declaring(TARGET_GATES);
    let worktree = open_change(&repo, "drops-guard");
    commit_file(
        &worktree,
        ".arc/gates.toml",
        "[gates.smoke]\ncommand = \"test -f README.md\"\n",
    );
    stdout(repo.arc(&worktree).args(["snapshot", "drops-guard"]));

    let here = status(&repo, &worktree, "drops-guard");
    let notes: Vec<&str> = here["declaration_notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note.as_str().unwrap())
        .collect();
    assert!(
        notes
            .iter()
            .any(|note| note.contains("differs from master's")),
        "{notes:?}"
    );
    assert!(
        notes.iter().any(|note| note.contains(
            "gate \"guard\" is declared by master and absent from this checkout; it is still required"
        )),
        "{notes:?}"
    );

    let there = status(&repo, &repo.root, "drops-guard");
    assert!(there.get("declaration_notes").is_none(), "{there}");
    let owed = |report: &serde_json::Value| -> Vec<String> {
        report["gates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|gate| gate["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(owed(&here), owed(&there));
    assert!(owed(&here).contains(&"guard".to_string()));
}

#[test]
fn brief_warns_for_target_gate_despite_local_gate_edits() {
    let repo = repo_declaring(TARGET_GATES);
    let worktree = open_change(&repo, "target-probe-warning");
    let local_gates = "[gates.local]\ncommand = \"false\"\n";
    fs::write(worktree.join(".arc/gates.toml"), local_gates).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), local_gates).unwrap();

    for (cwd, cause) in [(&worktree, None), (&repo.root, Some("second brief"))] {
        let mut command = repo.arc(cwd);
        command.args(["brief", "target-probe-warning", "--body-file", "-"]);
        if let Some(cause) = cause {
            command.args(["--cause-note", cause]);
        }
        command
            .args([
                "--probes-json",
                r#"[{"name":"guard-probe","command":"test -f MARKER"}]"#,
            ])
            .write_stdin("acceptance\n")
            .assert()
            .success()
            .stderr(predicates::str::contains("runs gate \"guard\""));
    }
}

#[test]
fn show_uses_target_review_checklist_despite_local_policy_edits() {
    let repo = repo_declaring(TARGET_GATES);
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[review]\nchecklist = [\"review target rule\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: declare review checklist"],
    );
    let worktree = open_change(&repo, "target-checklist");
    let local_policy = "[review]\nchecklist = [\"review local rule\"]\n";
    fs::write(worktree.join(".arc/policy.toml"), local_policy).unwrap();
    fs::write(repo.root.join(".arc/policy.toml"), local_policy).unwrap();

    for cwd in [&worktree, &repo.root] {
        repo.arc(cwd)
            .env("ARC_ROLE", "lead")
            .args(["show", "target-checklist"])
            .assert()
            .success()
            .stdout(predicates::str::contains("review target rule"))
            .stdout(predicates::str::contains("review local rule").not());
    }
}

#[test]
fn a_gate_the_change_adds_is_owed_beside_the_targets() {
    let repo = repo_declaring(TARGET_GATES);
    let worktree = open_change(&repo, "adds-gate");
    commit_file(
        &worktree,
        ".arc/gates.toml",
        &format!("{TARGET_GATES}\n[gates.extra]\ncommand = \"test -f EXTRA\"\n"),
    );
    snapshot_and_approve(&repo, &worktree, "adds-gate");
    verify(&repo, &worktree, "adds-gate", "smoke");
    verify(&repo, &worktree, "adds-gate", "guard");

    for cwd in [&worktree, &repo.root] {
        repo.arc(cwd)
            .args(["integrate", "adds-gate"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("Gate `extra` is not green"));
    }

    repo.arc(&worktree)
        .args(["verify", "adds-gate", "--gate", "extra"])
        .assert()
        .failure();
    commit_file(&worktree, "EXTRA", "extra\n");
    snapshot_and_approve(&repo, &worktree, "adds-gate");
    repo.arc(&worktree)
        .args(["verify", "adds-gate", "--all"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "adds-gate"])
        .assert()
        .success();
}

#[test]
fn an_edited_gate_command_is_judged_by_the_targets_command() {
    let repo = repo_declaring(TARGET_GATES);
    let worktree = open_change(&repo, "edits-command");
    commit_file(
        &worktree,
        ".arc/gates.toml",
        "[gates.smoke]\ncommand = \"test -f README.md\"\n\n\
         [gates.guard]\ncommand = \"test -f MARKER && true\"\n",
    );
    snapshot_and_approve(&repo, &worktree, "edits-command");
    repo.arc(&worktree)
        .args(["verify", "edits-command", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["integrate", "edits-command"])
        .assert()
        .success();
}

#[test]
fn a_target_command_that_fails_refuses_and_names_the_declaration_evaluated() {
    let repo = repo_declaring(TARGET_GATES);
    let worktree = open_change(&repo, "weakens-command");
    git(&worktree, &["rm", "-q", "MARKER"]);
    commit_file(
        &worktree,
        ".arc/gates.toml",
        "[gates.smoke]\ncommand = \"test -f README.md\"\n\n\
         [gates.guard]\ncommand = \"true\"\n",
    );
    stdout(repo.arc(&worktree).args(["snapshot", "weakens-command"]));
    verify(&repo, &worktree, "weakens-command", "smoke");
    repo.arc(&worktree)
        .args(["verify", "weakens-command", "--gate", "guard"])
        .assert()
        .failure();
    repo.arc(&worktree)
        .args(["review", "weakens-command", "--verdict", "approved"])
        .assert()
        .success();

    for cwd in [&worktree, &repo.root] {
        repo.arc(cwd)
            .args(["integrate", "weakens-command"])
            .assert()
            .failure()
            .stderr(
                predicates::str::contains("Gate `guard`")
                    .and(predicates::str::contains("Gates evaluated: those master's"))
                    .and(predicates::str::contains("the target's is evaluated")),
            );
    }
}

#[test]
fn a_change_cannot_loosen_the_targets_review_policy() {
    let repo = repo_declaring("[gates.smoke]\ncommand = \"test -f README.md\"\n");
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n\n[danger]\npaths = [\"risky.txt\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare policy"]);

    let worktree = open_change(&repo, "loosens-policy");
    git(&worktree, &["rm", "-q", ".arc/policy.toml"]);
    commit_file(&worktree, "risky.txt", "risky\n");
    stdout(repo.arc(&worktree).args(["snapshot", "loosens-policy"]));
    verify(&repo, &worktree, "loosens-policy", "smoke");
    repo.arc(&worktree)
        .args(["review", "loosens-policy", "--verdict", "approved"])
        .assert()
        .success();

    for cwd in [&worktree, &repo.root] {
        repo.arc(cwd)
            .args(["integrate", "loosens-policy"])
            .assert()
            .failure();
        assert_eq!(
            status(&repo, cwd, "loosens-policy")["integrate_ready"],
            false
        );
    }
}

#[test]
fn a_change_whose_target_branch_is_gone_is_blocked_and_refused() {
    let repo = repo_declaring(TARGET_GATES);
    git(&repo.root, &["branch", "staging"]);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "orphaned", "--target", "staging"]),
    );
    let worktree = repo.home.join(".worktrees/repo-orphaned");
    commit_file(&worktree, "work.txt", "work\n");
    snapshot_and_approve(&repo, &worktree, "orphaned");
    verify(&repo, &worktree, "orphaned", "smoke");
    verify(&repo, &worktree, "orphaned", "guard");
    git(&repo.root, &["branch", "-D", "staging"]);

    for cwd in [&worktree, &repo.root] {
        let report = status(&repo, cwd, "orphaned");
        assert_eq!(report["integrate_ready"], false, "{report}");
        assert!(
            report["blockers"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("target-unreadable")),
            "{report}"
        );
        assert_eq!(report["next_action"], "restore_target:staging", "{report}");

        repo.arc(cwd)
            .args(["check", "orphaned"])
            .assert()
            .code(6)
            .stdout(predicates::str::contains("`staging` cannot be resolved"));
        repo.arc(cwd)
            .args(["integrate", "orphaned"])
            .assert()
            .code(6)
            .stderr(
                predicates::str::contains("Target branch `staging` cannot be resolved")
                    .and(predicates::str::contains("could not be read")),
            );
        repo.arc(cwd)
            .args(["verify", "orphaned", "--gate", "smoke"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("\"staging\""));
    }
}
