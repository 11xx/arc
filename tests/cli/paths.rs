use super::common::*;

/// ARC_WORKTREES_DIR and ARC_DATA_ROOT relocate paths (sandboxing).
#[test]
fn path_overrides_relocate_worktrees_and_ledger() {
    let repo = Repo::new();
    let sandbox_wts = repo.home.join("sandbox-wts");
    let sandbox_data = repo.home.join("sandbox-data");

    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_WORKTREES_DIR", &sandbox_wts)
            .env("ARC_DATA_ROOT", &sandbox_data)
            .args(["begin", "boxed-p"]),
    );
    assert!(out.contains("change: boxed-p-"));
    assert!(sandbox_wts.join("repo-boxed-p").is_dir());

    // Ledger landed under the slugged repo path inside the data root,
    // and the default (in-repo) store does not know the change.
    let slug_dirs: Vec<_> = fs::read_dir(&sandbox_data).unwrap().collect();
    assert_eq!(slug_dirs.len(), 1);
    repo.arc(&repo.root)
        .env("ARC_DATA_ROOT", &sandbox_data)
        .args(["show", "boxed-p"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["show", "boxed-p"])
        .assert()
        .failure();
}

/// The config file under AI_HOME drives the same overrides.
#[test]
fn config_file_under_ai_home() {
    let repo = Repo::new();
    let ai_home = repo.home.join("ai");
    fs::create_dir_all(ai_home.join("arc")).unwrap();
    fs::write(
        ai_home.join("arc/config.toml"),
        format!(
            "worktrees_dir = \"{}\"\n",
            repo.home.join("cfg-wts").display()
        ),
    )
    .unwrap();

    stdout(
        repo.arc(&repo.root)
            .env("AI_HOME", &ai_home)
            .args(["begin", "cfg-c"]),
    );
    assert!(repo.home.join("cfg-wts").join("repo-cfg-c").is_dir());
}

#[test]
fn config_check_writable_leaves_no_events_or_probe_refs() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "probe-clean",
        "--no-worktree",
    ])));
    let before = event_count(&repo, &change_id);

    repo.arc(&repo.root)
        .args(["config", "--check-writable"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ok: store-root"))
        .stdout(predicates::str::contains("ok: lock"))
        .stdout(predicates::str::contains("ok: events"))
        .stdout(predicates::str::contains("ok: git-ref"));

    assert_eq!(event_count(&repo, &change_id), before);
    assert!(git_out(&repo.root, &["for-each-ref", "refs/arc/probe/"]).is_empty());
}

#[test]
fn config_check_writable_reports_store_failure_and_json_schema() {
    let repo = Repo::new();
    let store = repo.root.join("blocked-store");
    fs::create_dir_all(&store).unwrap();
    fs::set_permissions(&store, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
    let failed = repo
        .arc(&repo.root)
        .env("ARC_DATA_DIR", &store)
        .args(["config", "--check-writable"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("fail: store-root"))
        .stdout(predicates::str::contains(store.to_string_lossy().as_ref()));
    drop(failed);
    fs::set_permissions(&store, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();

    let json = stdout(
        repo.arc(&repo.root)
            .args(["config", "--check-writable", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema"], "arc-writability/1");
    assert!(value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|check| check["ok"] == true));
}

#[test]
fn config_check_writable_probes_commit_without_touching_the_repository() {
    let repo = Repo::new();
    let before = git_out(&repo.root, &["rev-parse", "HEAD"]);
    // Give the probe a private temp dir so leak detection observes only this
    // invocation. Scanning the shared temp dir races sibling tests running
    // their own probes, and fails for a reason unrelated to cleanup.
    let tmp = repo.root.join("probe-tmp");
    fs::create_dir_all(&tmp).unwrap();
    let out = stdout(
        repo.arc(&repo.root)
            .env("TMPDIR", &tmp)
            .args(["config", "--check-writable"]),
    );
    assert!(
        out.contains("ok: commit"),
        "expected a commit check, got {out:?}"
    );
    // The probe repository is disposable; the target must gain no commit and
    // keep a clean tree.
    assert_eq!(git_out(&repo.root, &["rev-parse", "HEAD"]), before);
    assert!(git_out(&repo.root, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        fs::read_dir(&tmp).unwrap().count(),
        0,
        "probe repository was left behind"
    );
}

#[test]
fn config_check_writable_commit_probe_follows_repository_signing_policy() {
    let repo = Repo::new();
    // Unset: the probe proves committing works and says signing was not tried.
    let unsigned = stdout(
        repo.arc(&repo.root)
            .args(["config", "--check-writable", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&unsigned).unwrap();
    let commit = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "commit")
        .expect("commit check present");
    assert_eq!(commit["ok"], true);
    assert!(
        commit["detail"].as_str().unwrap().contains("unsigned"),
        "detail should say signing was not exercised: {commit:?}"
    );

    // Required but unsatisfiable: an unusable key must fail the check and the
    // detail must carry gpg's own reason, not just the outer context.
    git_out(&repo.root, &["config", "commit.gpgsign", "true"]);
    git_out(
        &repo.root,
        &["config", "user.signingkey", "0000000000000000"],
    );
    repo.arc(&repo.root)
        .args(["config", "--check-writable"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("fail: commit"))
        .stdout(predicates::str::contains("gpg"));
}
