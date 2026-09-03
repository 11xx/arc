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

/// Whether a commit can be written and whether it can be signed are separate
/// findings, so an unreachable signing credential leaves writability intact
/// and is reported on its own line — without reading as reassuring.
#[test]
fn config_check_writable_separates_committing_from_signing() {
    let repo = Repo::new();
    // Signing off: committing is proven and the signing line says why nothing
    // was exercised.
    let unsigned = stdout(
        repo.arc(&repo.root)
            .args(["config", "--check-writable", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&unsigned).unwrap();
    assert_eq!(named_check(&value, "commit")["ok"], true);
    let signing = named_check(&value, "signing");
    assert_eq!(signing["ok"], true);
    assert!(
        signing["detail"].as_str().unwrap().contains("not required"),
        "{signing:?}"
    );

    // Required but unsatisfiable: writability still passes, and the signing
    // line carries gpg's own reason rather than the outer context alone.
    git_out(&repo.root, &["config", "commit.gpgsign", "true"]);
    git_out(
        &repo.root,
        &["config", "user.signingkey", "0000000000000000"],
    );
    repo.arc(&repo.root)
        .args(["config", "--check-writable"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("ok: commit"))
        .stdout(predicates::str::contains("fail: signing"))
        .stdout(predicates::str::contains(
            "the signature could not be produced",
        ))
        .stdout(predicates::str::contains("gpg"));
    let blocked: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "config",
        "--check-writable",
        "--json",
    ])))
    .unwrap();
    assert_eq!(named_check(&blocked, "commit")["ok"], true);
    assert_eq!(named_check(&blocked, "signing")["ok"], false);
}

/// Git resolves every boolean spelling, so reading the raw string would report
/// a signing repository as unsigned — the one case this probe exists to catch.
#[test]
fn config_check_writable_honours_every_git_boolean_spelling_for_signing() {
    for spelling in ["yes", "on", "1", "True"] {
        let repo = Repo::new();
        git_out(&repo.root, &["config", "commit.gpgsign", spelling]);
        git_out(
            &repo.root,
            &["config", "user.signingkey", "0000000000000000"],
        );
        repo.arc(&repo.root)
            .args(["config", "--check-writable"])
            .assert()
            .failure()
            .stdout(predicates::str::contains("fail: signing"))
            .stdout(predicates::str::contains("gpg"));
    }
}

/// The probe repository is created fresh and would otherwise inherit a global
/// signing policy, failing on a credential the target repository never uses.
#[test]
fn config_check_writable_probe_ignores_global_signing_the_repository_overrides() {
    let repo = Repo::new();
    fs::write(
        repo.home.join(".gitconfig"),
        "[commit]\n\tgpgsign = true\n[user]\n\tsigningkey = 0000000000000000\n",
    )
    .unwrap();
    // Repo::new pins commit.gpgsign false locally, so the repository resolves to
    // unsigned and the probe must too.
    let out = stdout(
        repo.arc(&repo.root)
            .args(["config", "--check-writable", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    let commit = named_check(&value, "commit");
    assert_eq!(
        commit["ok"], true,
        "probe should follow the repository, not the global config: {commit:?}"
    );
    let signing = named_check(&value, "signing");
    assert!(
        signing["detail"].as_str().unwrap().contains("not required"),
        "probe should not have exercised the global signing key: {signing:?}"
    );
}

fn named_check<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("{name} check present in {report}"))
}
