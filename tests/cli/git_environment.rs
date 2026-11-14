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
