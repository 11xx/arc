use crate::common::*;

const SELECTORS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_KEY_0",
    "GIT_CONFIG_VALUE_0",
    "GIT_CONFIG_KEY_99",
    "GIT_CONFIG_VALUE_99",
    "GIT_CONFIG_PARAMETERS",
];

fn poisoned(repo: &Repo, cwd: &Path, decoy: &Repo) -> AssertCommand {
    let mut command = repo.arc(cwd);
    // Discovery belongs to this repository; every other selector targets a decoy.
    command.env("GIT_DIR", repo.root.join(".git"));
    command.env("GIT_WORK_TREE", &repo.root);
    for name in SELECTORS
        .iter()
        .filter(|name| !["GIT_DIR", "GIT_WORK_TREE"].contains(name))
    {
        command.env(name, decoy.root.join(".git"));
    }
    command
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "user.email")
        .env("GIT_CONFIG_VALUE_0", "foreign@example.invalid")
        .env("GIT_CONFIG_PARAMETERS", "'user.name=Foreign'");
    command
}

fn assert_script() -> String {
    format!(
        "{}\ngit config --local guard.executed yes\n",
        SELECTORS
            .iter()
            .map(|name| format!("test -z \"${{{name}+present}}\" || exit 71"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Invariant I12. Claim: a hook-style invocation resolves its repository before discarding Git
/// selectors. Rejects discovery redirected by command configuration or an index.
#[test]
fn hook_parent_keeps_arc_on_the_resolved_repository() {
    let repo = Repo::new();
    let decoy = Repo::new();
    let before = fs::read(decoy.root.join(".git/config")).unwrap();
    let value = json_stdout(poisoned(&repo, &decoy.root, &decoy).args(["doctor", "--json"]));
    assert!(value["roots"]["ledger"]
        .as_str()
        .unwrap()
        .starts_with(repo.root.to_str().unwrap()));
    assert_eq!(fs::read(decoy.root.join(".git/config")).unwrap(), before);
    assert_eq!(git_out(&repo.root, &["config", "user.name"]), "Tester");
    fs::write(
        repo.home.join(".local/ai/arc/config.toml"),
        "[identity]\ndetect=false\n",
    )
    .unwrap();
    let opened = stdout(
        poisoned(&repo, &repo.root, &decoy)
            .env_remove("ARC_ACTOR")
            .env_remove("ARC_HARNESS")
            .env_remove("ARC_SESSION")
            .args(["begin", "hook-parent", "--no-worktree"]),
    );
    let id = opened_change_id(&opened);
    let actors: Vec<_> = fs::read_dir(event_dir(&repo, &id))
        .unwrap()
        .map(|entry| {
            let event: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.unwrap().path()).unwrap()).unwrap();
            event["actor"].as_str().unwrap().to_string()
        })
        .collect();
    assert!(actors.iter().all(|actor| actor == "Tester"), "{actors:?}");
}

/// Invariant I12. Claim: gates and environment probes receive no repository or command-config
/// selectors. Rejects a passing command that can write the parent's decoy config.
#[test]
fn gates_and_probes_clear_every_repository_selector() {
    let repo = Repo::new();
    let decoy = Repo::new();
    let before = fs::read(decoy.root.join(".git/config")).unwrap();
    let script = format!("{}\nprintf 'identity\\n'\n", assert_script());
    repo.declare_gates_locally(&format!(
        "[gates.guard]\ncommand = {script:?}\nenvironment = {script:?}\n"
    ));
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "guard",
        "--no-worktree",
    ])));
    repo.commit(&repo.root, "feature", "feature", "test: guard content");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    poisoned(&repo, &repo.root, &decoy)
        .args(["verify", &change, "--all"])
        .assert()
        .success()
        .stdout(predicates::str::contains("pass"));
    let state = json_stdout(repo.arc(&repo.root).args(["show", &change, "--json"]));
    assert!(
        state["verifications"][0]["environment"]["identity"].is_string(),
        "{state}"
    );
    assert_eq!(git_out(&repo.root, &["config", "guard.executed"]), "yes");
    assert_eq!(fs::read(decoy.root.join(".git/config")).unwrap(), before);
}

/// Claim: declared preparation and checkout hooks use the allocated checkout's
/// Git state. Rejects inherited selectors reaching the invoking or decoy repo.
#[test]
fn preparation_and_checkout_hooks_clear_repository_selectors() {
    let repo = Repo::new();
    let decoy = Repo::new();
    let before = fs::read(decoy.root.join(".git/config")).unwrap();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        format!("[prepare]\ncommand = {:?}\n", assert_script()),
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "test: declare preparation"]);
    let hook = repo.root.join(".git/hooks/post-checkout");
    fs::write(
        &hook,
        format!("#!/bin/sh\n{}", assert_script().replace("+present", "-")),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    poisoned(&repo, &repo.root, &decoy)
        .args(["begin", "guard"])
        .assert()
        .success();
    assert_eq!(fs::read(decoy.root.join(".git/config")).unwrap(), before);
}

fn doctor(repo: &Repo) -> serde_json::Value {
    let output = repo
        .arc(&repo.root)
        .env_remove("ARC_TEST_FIXTURE_IDENTITY")
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "doctor must report configuration problems"
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)))
}

/// Invariant I12. Claim: doctor, catchup and the workspace report expose leaked local identity,
/// bare checkout state and disabled global signing. Rejects a clean health report
/// for a contaminated registered project, including included config values
/// and a bare setting inherited when no local key exists.
#[test]
fn configuration_health_reaches_all_three_views() {
    let repo = Repo::new();
    let other = Repo::new();
    for project in [&repo, &other] {
        project
            .arc(&project.root)
            .env("ARC_SANDBOX", &repo.home)
            .env("HOME", &repo.home)
            .args(["journal", "log", "registered", "registered project"])
            .assert()
            .success();
        fs::write(
            project.root.join(".git/included"),
            "[tag]\n gpgsign = false\n",
        )
        .unwrap();
        git(&project.root, &["config", "include.path", "included"]);
    }
    fs::write(
        repo.home.join(".gitconfig"),
        "[commit]\n gpgsign = true\n[tag]\n gpgsign = true\n",
    )
    .unwrap();
    git(&repo.root, &["config", "core.bare", "true"]);
    let problems = doctor(&repo)["problems"].as_array().unwrap().clone();
    for (code, count) in [
        ("repository-local-identity", 2),
        ("bare-with-worktree", 1),
        ("repository-signing-disabled", 2),
    ] {
        assert_eq!(
            problems.iter().filter(|p| p["code"] == code).count(),
            count,
            "{problems:?}"
        );
    }
    git(&repo.root, &["config", "--unset", "core.bare"]);
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(repo.home.join(".gitconfig"))
        .unwrap()
        .write_all(b"[core]\n bare = true\n")
        .unwrap();
    let inherited = doctor(&repo);
    assert!(
        inherited["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "bare-with-worktree"),
        "inherited bare setting must be reported: {inherited}"
    );
    let catchup = stdout(
        repo.arc(&repo.root)
            .env_remove("ARC_TEST_FIXTURE_IDENTITY")
            .args(["catchup", "--sessions", "0"]),
    );
    assert_eq!(
        catchup
            .lines()
            .filter(|line| line.starts_with("Git config problems:"))
            .count(),
        1,
        "{catchup}"
    );
    let report = json_stdout(
        repo.arc(&repo.root)
            .env_remove("ARC_TEST_FIXTURE_IDENTITY")
            .args(["workspace", "report", "--json"]),
    );
    let projects = report["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2, "{report}");
    assert!(
        projects.iter().all(|project| project["git_health"]
            .as_array()
            .is_some_and(|findings| !findings.is_empty())),
        "{report}"
    );
}

/// Claim: a project can explicitly permit local identity without excusing bare
/// checkout state or signing overrides. Rejects a blanket health exemption.
#[test]
fn local_identity_policy_exception_is_scoped_to_identity() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[git]\nallow_local_identity = true\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .env_remove("ARC_TEST_FIXTURE_IDENTITY")
        .args(["doctor", "--json"])
        .assert()
        .success();
    git(&repo.root, &["config", "core.bare", "true"]);
    let value = doctor(&repo);
    let codes: Vec<_> = value["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"bare-with-worktree"), "{value}");
    assert!(!codes.contains(&"repository-local-identity"), "{value}");
}

/// Invariant I12. Claim: every introduced commit's raw author and committer address is checked,
/// even an interior commit, a mailmapped identity, or an overridden snapshot base.
/// Rejects each reserved domain while permitting real-looking addresses.
#[test]
fn snapshot_refuses_reserved_author_and_committer_addresses() {
    for domain in [
        "unit.invalid",
        "unit.example",
        "unit.test",
        "unit.localhost",
        "example.com",
        "example.org",
        "example.net",
        "sub.Example.COM.",
    ] {
        for role in ["author", "committer"] {
            let repo = Repo::new();
            let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
                "begin",
                "identity",
                "--no-worktree",
            ])));
            git(&repo.root, &["config", "user.email", "real@fixture.dev"]);
            let address = format!("tester@{domain}");
            let mut commit = Command::new("git");
            commit
                .current_dir(&repo.root)
                .args(["commit", "--allow-empty", "-m", "test: placeholder"])
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", repo.home.join(".gitconfig"))
                .env(
                    if role == "author" {
                        "GIT_AUTHOR_EMAIL"
                    } else {
                        "GIT_COMMITTER_EMAIL"
                    },
                    &address,
                );
            assert!(commit.output().unwrap().status.success());
            let bad = repo.head(&repo.root);
            repo.commit(&repo.root, "feature", "feature", "test: real tip");
            for args in [
                vec!["snapshot", &change],
                vec!["snapshot", &change, "--base", "HEAD"],
            ] {
                repo.arc(&repo.root)
                    .env_remove("ARC_TEST_FIXTURE_IDENTITY")
                    .args(args)
                    .assert()
                    .failure()
                    .stderr(
                        predicates::str::contains(&bad)
                            .and(predicates::str::contains(format!("reserved-domain {role}")))
                            .and(predicates::str::contains("re-author")),
                    );
            }
        }
    }
    let repo = Repo::new();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "person",
        "--no-worktree",
    ])));
    git(
        &repo.root,
        &["config", "user.email", "person@real-example.com"],
    );
    repo.commit(&repo.root, "feature", "feature", "test: real identity");
    repo.arc(&repo.root)
        .env_remove("ARC_TEST_FIXTURE_IDENTITY")
        .args(["snapshot", &change])
        .assert()
        .success();
}

/// Invariant I12. Claim: integration rechecks recorded commits, including patchsets recorded by
/// an older executable. Rejects placeholder history before any target ref moves.
#[test]
fn integrate_refuses_a_previously_recorded_placeholder_patchset() {
    let repo = Repo::new();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "identity",
        "--no-worktree",
    ])));
    repo.commit(&repo.root, "feature", "feature", "test: placeholder");
    let bad = repo.head(&repo.root);
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(
        repo.arc(&repo.root)
            .args(["review", &change, "--verdict", "approved"]),
    );
    let target = git_out(&repo.root, &["rev-parse", "master"]);
    for options in [
        vec!["--no-cleanup"],
        vec!["--dry-run"],
        vec!["--provisional"],
    ] {
        repo.arc(&repo.root)
            .env_remove("ARC_TEST_FIXTURE_IDENTITY")
            .args(["integrate", &change])
            .args(options)
            .assert()
            .failure()
            .stderr(predicates::str::contains(&bad).and(predicates::str::contains("re-author")));
    }
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), target);
    fs::remove_file(repo.root.join(".git/arc-test-fixture")).unwrap();
    repo.arc(&repo.root)
        .args(["snapshot", &change])
        .assert()
        .failure()
        .stderr(predicates::str::contains("reserved-domain"));
}

/// Claim: readiness reports what integration refuses. A change whose recorded
/// patchset holds a reserved-domain commit is not ready: `arc check` exits 21
/// listing `reserved-identity`, and check and status carry integration's own
/// refusal, naming the commit and how to re-author it, with a next step that
/// names the same remedy and a fresh snapshot. Rejects readiness that reads
/// ready, or names another remedy, for a patchset `integrate` refuses.
#[test]
fn readiness_names_a_recorded_reserved_identity_as_integration_refuses_it() {
    let repo = Repo::new();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "identity",
        "--no-worktree",
    ])));
    repo.commit(&repo.root, "feature", "feature", "test: placeholder");
    let bad = repo.head(&repo.root);
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(
        repo.arc(&repo.root)
            .args(["review", &change, "--verdict", "approved"]),
    );
    stdout(repo.arc(&repo.root).args(["check", &change]));

    let guarded = |args: &[&str]| {
        let output = repo
            .arc(&repo.root)
            .env_remove("ARC_TEST_FIXTURE_IDENTITY")
            .args(args)
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap(),
        )
    };
    let (code, refused) = guarded(&["integrate", &change, "--dry-run"]);
    assert_eq!(code, Some(1), "{refused}");
    let refusal = refused
        .lines()
        .find_map(|line| line.strip_prefix("error: "))
        .unwrap()
        .to_string();
    assert!(
        refusal.contains(&bad) && refusal.contains("re-author"),
        "{refusal}"
    );

    let (code, check) = guarded(&["check", &change]);
    assert_eq!(code, Some(21), "{check}");
    assert!(check.contains(&refusal), "{check}");
    let step = check
        .lines()
        .find_map(|line| line.strip_prefix("Next step: "))
        .unwrap();
    assert!(
        step.contains(&bad)
            && step.contains("re-author it with a real identity")
            && step.contains(&format!("arc snapshot {change}"))
            && step.ends_with(&format!("(reauthor:{bad})")),
        "{step}"
    );

    let (_, json) = guarded(&["check", &change, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["ready"], false, "{json}");
    assert!(json["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|blocker| blocker["blocker"] == "reserved-identity" && blocker["exit_code"] == 21));

    let (_, status) = guarded(&["status", &change, "--json"]);
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["reserved_identity"], refusal.as_str(), "{status}");
    assert_eq!(status["next_action"], format!("reauthor:{bad}"), "{status}");
    assert!(
        status.to_string().contains("\"reserved-identity\""),
        "{status}"
    );
}

/// Claim: selector and renderer commands receive the same scrubbed environment
/// as gates. Rejects a selector fallback hiding inherited selectors or a renderer
/// that can write the decoy's configuration.
#[test]
fn selectors_and_changelog_renderers_clear_repository_selectors() {
    let repo = Repo::new();
    let decoy = Repo::new();
    let before = fs::read(decoy.root.join(".git/config")).unwrap();
    let script = format!("{}\npython3 -c 'import json,sys; d=json.load(sys.stdin); print(json.dumps(dict(schema=\"arc-gate-selection/1\",complete=True,request=d,gates=[])))'", assert_script());
    repo.declare_gates_locally(&format!(
        "[gates.guard]\ncommand='true'\nselector={script:?}\n"
    ));
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "selector",
        "--no-worktree",
    ])));
    repo.commit(&repo.root, "feature", "one", "test: first content");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    stdout(repo.arc(&repo.root).args(["verify", &change, "--all"]));
    repo.commit(&repo.root, "feature", "two", "test: next content");
    stdout(repo.arc(&repo.root).args(["snapshot", &change]));
    poisoned(&repo, &repo.root, &decoy)
        .args(["verify", &change, "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "project selector proves unchanged scope",
        ));
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/changelog.toml"),
        "target='NEWS'\nrenderer='command'\nrenderer_command=['sh', 'render.sh']\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("render.sh"),
        format!(
            "{}\ncat >/dev/null\nprintf 'rendered\\n'\n",
            assert_script()
        ),
    )
    .unwrap();
    poisoned(&repo, &repo.root, &decoy)
        .arg("changelog")
        .assert()
        .success()
        .stdout("rendered\n");
    assert_eq!(fs::read(decoy.root.join(".git/config")).unwrap(), before);
}

/// Claim: slot identity and warm commands discard inherited Git selectors.
/// Rejects a successful warm that configures a repository outside its checkout.
#[test]
fn slot_identity_and_warming_clear_repository_selectors() {
    let repo = Repo::new();
    let decoy = Repo::new();
    let before = fs::read(decoy.root.join(".git/config")).unwrap();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    let script = format!("{}\nprintf 'identity\\n'\n", assert_script());
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        format!("[slots]\nsize=1\nidentity={script:?}\nwarm={script:?}\n"),
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "test: declare slot"]);
    poisoned(&repo, &repo.root, &decoy)
        .args(["slots", "warm"])
        .assert()
        .success()
        .stdout(predicates::str::contains("warmed"));
    assert_eq!(fs::read(decoy.root.join(".git/config")).unwrap(), before);
}

/// Claim: publication executes gates in its scratch checkout with no inherited
/// Git selectors. Rejects a gate that reads or writes the caller's repository.
#[test]
fn publication_scratch_commands_clear_repository_selectors() {
    let repo = Repo::new();
    let decoy = Repo::new();
    let before = fs::read(decoy.root.join(".git/config")).unwrap();
    let script = format!(
        "{}\ntest \"$(git rev-parse --show-toplevel)\" = \"$PWD\"\n",
        assert_script()
    );
    repo.declare_gates_locally(&format!(
        "[gates.guard]\ncommand={script:?}\nevents=['publication']\n"
    ));
    poisoned(&repo, &repo.root, &decoy)
        .args(["publication", "--target", "master"])
        .assert()
        .success()
        .stdout(predicates::str::contains("publication gate guard: pass"));
    assert_eq!(fs::read(decoy.root.join(".git/config")).unwrap(), before);
}
