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
            .args(["begin", "cfg-c", "--worktree"]),
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
    // A scratch directory under the prefix is private to this invocation, so
    // leak detection observes only its own probe. Scanning the shared temp dir
    // would race sibling tests running theirs, and fail for a reason unrelated
    // to cleanup.
    let tmp = repo.home.join("tmp");
    let out = stdout(repo.arc(&repo.root).args(["config", "--check-writable"]));
    assert!(
        out.contains("ok: commit"),
        "expected a commit check, got {out:?}"
    );
    // The probe repository is disposable; the target must gain no commit and
    // keep a clean tree.
    assert_eq!(git_out(&repo.root, &["rev-parse", "HEAD"]), before);
    assert!(git_out(&repo.root, &["status", "--porcelain"]).is_empty());
    assert!(
        tmp.is_dir(),
        "the probe should have made its scratch repository under the prefix"
    );
    assert_eq!(
        fs::read_dir(&tmp).unwrap().count(),
        0,
        "probe repository was left behind"
    );
}

/// A project that signs nothing has nothing to report about signing, and
/// says that rather than claiming a capability it never exercised.
#[test]
fn config_check_writable_reports_signing_as_not_required_when_it_is_off() {
    let repo = Repo::new();
    let unsigned = stdout(
        repo.arc(&repo.root)
            .args(["config", "--check-writable", "--json"]),
    );
    let value: serde_json::Value = serde_json::from_str(&unsigned).unwrap();
    assert_eq!(named_check(&value, "commit")["ok"], true);
    let signing = named_check(&value, "signing");
    assert_eq!(signing["ok"], true);
    assert_eq!(signing["advisory"], true);
    assert!(
        signing["detail"].as_str().unwrap().contains("not required"),
        "{signing:?}"
    );
    repo.arc(&repo.root)
        .args(["config", "--check-writable"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ok: signing: not required"));
}

/// The exit code answers what this process can write. Signing is somebody
/// else's to supply when the work is landed, so an unreachable credential
/// warns with gpg's own reason and leaves the probe's verdict alone.
#[test]
fn config_check_writable_warns_about_unreachable_signing_without_failing() {
    let repo = Repo::new();
    git_out(&repo.root, &["config", "commit.gpgsign", "true"]);
    git_out(
        &repo.root,
        &["config", "user.signingkey", "0000000000000000"],
    );
    repo.arc(&repo.root)
        .args(["config", "--check-writable"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ok: commit"))
        .stdout(predicates::str::contains("warn: signing"))
        .stdout(predicates::str::contains(
            "the signature could not be produced",
        ))
        .stdout(predicates::str::contains("gpg"))
        .stdout(predicates::str::contains("fail:").not());

    let blocked: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "config",
        "--check-writable",
        "--json",
    ])))
    .unwrap();
    assert_eq!(named_check(&blocked, "commit")["ok"], true);
    let signing = named_check(&blocked, "signing");
    assert_eq!(signing["ok"], false);
    assert_eq!(
        signing["advisory"], true,
        "a consumer must be able to tell a warning from a blocked path: {signing:?}"
    );
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
            .success()
            .stdout(predicates::str::contains("warn: signing"))
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

#[test]
fn config_style_layers_and_constraints() {
    let repo = Repo::new();
    let ai = repo.home.join("ai");
    fs::create_dir_all(ai.join("arc")).unwrap();
    fs::write(
        ai.join("arc/config.toml"),
        "[integration]\nshape = 'no-ff'\n",
    )
    .unwrap();
    let read = || -> serde_json::Value {
        serde_json::from_str(&stdout(
            repo.arc(&repo.root).env("AI_HOME", &ai).arg("config"),
        ))
        .unwrap()
    };
    let user = read();
    assert_eq!(user["schema"], "arc-config/1");
    assert_eq!(user["style"]["integration.shape"]["value"], "no-ff");
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[defaults.integration]\nshape = 'ff-only'\n[integration]\nshapes = ['ff-only', 'no-ff']\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    assert_eq!(read()["style"]["integration.shape"]["value"], "ff-only");
    fs::create_dir_all(repo.root.join(".git/arc")).unwrap();
    fs::write(
        repo.root.join(".git/arc/operator-policy.toml"),
        "[defaults.integration]\nshape = 'no-ff'\n[integration]\nshapes = ['no-ff']\n",
    )
    .unwrap();
    let operator = read();
    assert_eq!(operator["style"]["integration.shape"]["value"], "no-ff");
    assert_eq!(
        operator["style"]["integration.shape"]["overridden"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    fs::write(
        repo.root.join(".git/arc/operator-policy.toml"),
        "[integration]\nshapes = []\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .env("AI_HOME", &ai)
        .arg("config")
        .assert()
        .failure();
}

#[test]
fn config_style_invalid_values_and_user_constraints_refuse() {
    let repo = Repo::new();
    let ai = repo.home.join("ai");
    fs::create_dir_all(ai.join("arc")).unwrap();
    for (text, key) in [
        ("[change]\ncheckout = 'bad'\n", "change.checkout"),
        ("[integration]\ncleanup = 'yes'\n", "integration.cleanup"),
        (
            "[integration]\nshapes = ['ff-only']\n",
            "integration.shapes",
        ),
        ("[integration]\nhistory = 'squash'\n", "integration.history"),
    ] {
        fs::write(ai.join("arc/config.toml"), text).unwrap();
        repo.arc(&repo.root)
            .env("AI_HOME", &ai)
            .arg("config")
            .assert()
            .failure()
            .stderr(predicates::str::contains(key))
            .stderr(predicates::str::contains("config.toml"));
    }
}

#[test]
fn config_style_outside_git_uses_user_and_builtin() {
    let repo = Repo::new();
    let ai = repo.home.join("ai");
    fs::create_dir_all(ai.join("arc")).unwrap();
    fs::write(
        ai.join("arc/config.toml"),
        "[change]\nbranch_prefix='topic/'\n[integration]\ncleanup=true\n",
    )
    .unwrap();
    let config = json_stdout(repo.arc(&repo.home).env("AI_HOME", &ai).arg("config"));
    let style = &config["style"];
    assert_eq!(config["schema"], "arc-config/1");
    assert_eq!(style["change.branch_prefix"]["value"], "topic/");
    assert!(style["change.branch_prefix"]["source"]
        .as_str()
        .unwrap()
        .starts_with("user:"));
    assert_eq!(style["integration.cleanup"]["value"], true);
    assert_eq!(style["change.checkout"]["source"], "builtin");
    assert_eq!(style["integration.shape"]["value"], "ff-only");
    for key in [
        "change.checkout",
        "change.branch_prefix",
        "integration.shape",
        "integration.cleanup",
    ] {
        assert!(style[key]["overridden"].is_array());
        assert!(style[key]["allowed"].is_null());
    }
}

#[test]
fn config_style_operator_write_validates_before_replacement() {
    let repo = Repo::new();
    let body = repo.home.join("policy.toml");
    let valid = "[defaults.change]\ncheckout='in-place'\nbranch_prefix='topic/'\n[defaults.integration]\nshape='no-ff'\ncleanup=true\n[integration]\nshapes=['no-ff']\nhistory='squash'\n";
    fs::write(&body, valid).unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["policy", "write", "--body-file"])
            .arg(&body),
    );
    let config = json_stdout(repo.arc(&repo.root).arg("config"));
    assert_eq!(
        config["style"]["integration.shape"]["allowed"]["values"],
        serde_json::json!(["no-ff"])
    );
    assert_eq!(
        config["style"]["integration.shape"]["allowed"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let before = fs::read(repo.root.join(".git/arc/operator-policy.toml")).unwrap();
    fs::write(&body, "[defaults.integration]\nshape='bad'\n").unwrap();
    repo.arc(&repo.root)
        .args(["policy", "write", "--body-file"])
        .arg(&body)
        .assert()
        .failure()
        .stderr(predicates::str::contains("operator-policy.toml"))
        .stderr(predicates::str::contains("integration.shape"));
    assert_eq!(
        fs::read(repo.root.join(".git/arc/operator-policy.toml")).unwrap(),
        before
    );
}
