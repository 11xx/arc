use super::common::{git, git_out, Repo};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn upstream_clone(repo: &Repo, policy: &str, gates: &str) -> PathBuf {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
    fs::write(repo.root.join(".arc/gates.toml"), gates).unwrap();
    git(&repo.root, &["add", ".arc/policy.toml", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: declare upstream policy"],
    );

    let clone = repo.home.join("upstream-clone");
    git(
        &repo.root,
        &[
            "clone",
            repo.root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["config", "user.name", "Tester"]);
    git(&clone, &["config", "user.email", "tester@example.invalid"]);
    git(&clone, &["config", "commit.gpgsign", "false"]);
    clone
}

fn arc_output(repo: &Repo, cwd: &std::path::Path, args: &[&str]) -> String {
    let output = repo.arc(cwd).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "arc {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn write_operator_policy(repo: &Repo, clone: &std::path::Path, text: &str) {
    let source = repo.home.join("operator-policy.toml");
    fs::write(&source, text).unwrap();
    arc_output(
        repo,
        clone,
        &["policy", "write", "--body-file", source.to_str().unwrap()],
    );
}

#[test]
fn audit_policy_show_includes_effective_environment_probe() {
    let repo = Repo::new();
    write_operator_policy(
        &repo,
        &repo.root,
        "[gates.build]\ncommand = \"true\"\nenvironment = \"printf host\"\n",
    );
    let view = arc_output(&repo, &repo.root, &["policy", "show"]);
    let gate = view
        .lines()
        .find(|line| line.starts_with("gate build:"))
        .unwrap();
    assert!(gate.contains("environment = \"printf host\""), "{view}");
    assert!(
        gate.contains("<git-common-dir>/arc/operator-policy.toml"),
        "{view}"
    );
}

pub(super) fn operator_policy_unions_with_project_policy_without_tracked_changes() {
    let repo = Repo::new();
    let clone = upstream_clone(
        &repo,
        "[policy]\nforbid_self_approval = false\n\n[danger]\npaths = [\"project-danger.rs\"]\n",
        "[gates.project]\ncommand = \"true\"\n\n[gates.shared]\ncommand = \"true\"\nprofiles = [\"local\"]\ntimeout = \"30s\"\n",
    );
    write_operator_policy(
        &repo,
        &clone,
        "[policy]\nforbid_self_approval = true\nrequire_declared_actor = true\n\n[danger]\npaths = [\"operator-danger.rs\"]\n\n[gates.operator]\ncommand = \"true\"\n\n[gates.shared]\ncommand = \"true\"\nprofiles = [\"release\"]\ntimeout = \"2m\"\n",
    );

    let operator_path = clone.join(".git/arc/operator-policy.toml");
    assert!(operator_path.is_file());
    assert_eq!(
        arc_output(&repo, &clone, &["policy", "path"]).trim(),
        operator_path.to_str().unwrap()
    );
    assert_eq!(git_out(&clone, &["status", "--porcelain"]), "");
    assert!(!git_out(&clone, &["ls-files"])
        .lines()
        .any(|path| path.contains("operator-policy")));
    let undeclared = repo
        .arc(&clone)
        .env_remove("ARC_ACTOR")
        .args(["begin", "operator-requires-actor"])
        .output()
        .unwrap();
    assert!(!undeclared.status.success());
    assert!(String::from_utf8_lossy(&undeclared.stderr)
        .contains("<git-common-dir>/arc/operator-policy.toml"));
    assert_eq!(git_out(&clone, &["status", "--porcelain"]), "");

    let policy_view = arc_output(&repo, &clone, &["policy", "show"]);
    assert!(policy_view.contains(".arc/policy.toml"));
    assert!(policy_view.contains("<git-common-dir>/arc/operator-policy.toml"));
    assert!(policy_view.contains("profiles = local, release; timeout = 30"));
    assert_eq!(git_out(&clone, &["status", "--porcelain"]), "");

    let begin = arc_output(&repo, &clone, &["begin", "policy-union"]);
    assert_eq!(git_out(&clone, &["status", "--porcelain"]), "");
    let worktree = PathBuf::from(
        begin
            .lines()
            .find_map(|line| line.strip_prefix("worktree: "))
            .unwrap(),
    );
    repo.commit(
        &worktree,
        "operator-danger.rs",
        "dangerous operator path\n",
        "test: touch operator danger path",
    );
    arc_output(&repo, &worktree, &["snapshot", "policy-union"]);

    let verification = arc_output(
        &repo,
        &worktree,
        &["verify", "policy-union", "--gate", "operator"],
    );
    assert!(verification
        .contains("gate: operator (declared by <git-common-dir>/arc/operator-policy.toml)"));
    let markdown = arc_output(&repo, &worktree, &["show", "policy-union"]);
    assert!(markdown.contains("## Policy declarations"));
    assert!(markdown.contains("danger.paths[\"operator-danger.rs\"]"));
    assert!(markdown.contains("<git-common-dir>/arc/operator-policy.toml"));

    let status = arc_output(&repo, &worktree, &["status", "policy-union"]);
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["schema"], "arc-status/26");
    assert_eq!(status["danger"]["dangerous"], true);
    let gates = status["gates"].as_array().unwrap();
    for name in ["project", "operator", "shared"] {
        assert!(gates.iter().any(|gate| gate["name"] == name));
    }
    let shared = gates.iter().find(|gate| gate["name"] == "shared").unwrap();
    let declared_by = shared["declared_by"].as_array().unwrap();
    assert!(declared_by.contains(&Value::String(".arc/gates.toml".into())));
    assert!(declared_by.contains(&Value::String(
        "<git-common-dir>/arc/operator-policy.toml".into()
    )));
    let danger_sources = status["policy_sources"]["danger.paths[\"operator-danger.rs\"]"]
        .as_array()
        .unwrap();
    assert!(danger_sources.contains(&Value::String(
        "<git-common-dir>/arc/operator-policy.toml".into()
    )));
    let enabled_sources = status["policy_sources"]["policy.forbid_self_approval=true"]
        .as_array()
        .unwrap();
    assert_eq!(
        enabled_sources,
        &[Value::String(
            "<git-common-dir>/arc/operator-policy.toml".into()
        )]
    );
    let actor_sources = status["policy_sources"]["policy.require_declared_actor=true"]
        .as_array()
        .unwrap();
    assert_eq!(actor_sources, enabled_sources);
    assert_eq!(git_out(&clone, &["status", "--porcelain"]), "");
    assert_eq!(git_out(&worktree, &["status", "--porcelain"]), "");
}

pub(super) fn conflicting_gate_commands_are_reported_and_refused() {
    let repo = Repo::new();
    let clone = upstream_clone(&repo, "", "[gates.build]\ncommand = \"true\"\n");
    let begin = arc_output(&repo, &clone, &["begin", "policy-conflict"]);
    let change = begin
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    write_operator_policy(&repo, &clone, "[gates.build]\ncommand = \"false\"\n");

    let doctor = repo
        .arc(&clone)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(doctor.status.code(), Some(1));
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let conflict = doctor["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|problem| problem["code"] == "gate-declaration-conflict")
        .unwrap();
    let detail = conflict["detail"].as_str().unwrap();
    assert!(detail.contains(".arc/gates.toml"));
    assert!(detail.contains("<git-common-dir>/arc/operator-policy.toml"));

    repo.arc(&clone)
        .args(["check", &change])
        .assert()
        .failure()
        .stderr(predicates::str::contains(".arc/gates.toml"))
        .stderr(predicates::str::contains(
            "<git-common-dir>/arc/operator-policy.toml",
        ));
    assert_eq!(git_out(&clone, &["status", "--porcelain"]), "");
}

pub(super) fn layered_gates_take_the_stricter_timeout_and_refuse_a_different_environment() {
    let repo = Repo::new();
    let clone = upstream_clone(
        &repo,
        "",
        "[gates.build]\ncommand = \"true\"\ntimeout = \"5m\"\n\n[gates.probed]\ncommand = \"true\"\nenvironment = \"echo project\"\n",
    );
    let begin = arc_output(&repo, &clone, &["begin", "policy-layers"]);
    let change = begin
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    write_operator_policy(
        &repo,
        &clone,
        "[gates.build]\ncommand = \"true\"\ntimeout = \"1m\"\n\n[gates.probed]\ncommand = \"true\"\nenvironment = \"echo operator\"\n",
    );

    let view = arc_output(&repo, &clone, &["policy", "show"]);
    assert!(
        view.contains("gate build: command = \"true\"; profiles = all; timeout = 60;"),
        "{view}"
    );
    assert!(
        view.contains("environment probe \"echo operator\""),
        "{view}"
    );
    repo.arc(&clone)
        .args(["check", &change])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "environment probe \"echo project\"",
        ));
}

#[test]
fn operator_policy_is_written_owner_only_whatever_the_umask() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let source = repo.home.join("operator-policy.toml");
    fs::write(&source, "[gates.build]\ncommand = \"true\"\n").unwrap();
    let path = PathBuf::from(arc_output(&repo, &repo.root, &["policy", "path"]).trim());
    for _ in 0..2 {
        repo.arc_under_umask(&repo.root, "000")
            .args(["policy", "write", "--body-file", source.to_str().unwrap()])
            .assert()
            .success();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }
}
