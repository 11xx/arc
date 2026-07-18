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
