use super::common::*;

/// Declare one gate whose environment probe reports `$ARC_TEST_ENV`.
fn write_env_gate(repo: &Repo) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\nenvironment = \"printf %s \\\"$ARC_TEST_ENV\\\"\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: declare an environment gate"],
    );
}

/// Declare a gate for every change without touching any tree.
fn write_gate(repo: &Repo, command: &str, environment: &str, timeout: Option<&str>) {
    let mut text = format!("[gates.build]\ncommand = {command:?}\nenvironment = {environment:?}\n");
    if let Some(timeout) = timeout {
        text.push_str(&format!("timeout = {timeout:?}\n"));
    }
    repo.declare_gates_locally(&text);
}

fn status(repo: &Repo, env: &str) -> serde_json::Value {
    json_stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_ENV", env)
            .args(["status", "env-gate"]),
    )
}

fn verification_events(repo: &Repo) -> Vec<serde_json::Value> {
    stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "env-gate",
        "--type",
        "verification-recorded",
    ]))
    .lines()
    .filter(|line| !line.trim().is_empty())
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

fn equal_tree_change(repo: &Repo, pass_here: bool, run_elsewhere: bool) -> (PathBuf, String) {
    repo.declare_gates_locally(
        "[gates.build]\ncommand = \"true\"\nenvironment = \"printf %s \\\"$ARC_TEST_ENV\\\"\"\n",
    );
    stdout(repo.arc(&repo.root).args(["begin", "env-gate"]));
    let worktree = repo.home.join(".worktrees/repo-env-gate");
    repo.commit(&worktree, "work.txt", "a\n", "test: first tree");
    stdout(repo.arc(&worktree).args(["snapshot", "env-gate"]));
    let first_tree = git_out(&worktree, &["rev-parse", "HEAD^{tree}"]);
    let first_head = repo.head(&worktree);
    if pass_here {
        repo.arc(&worktree)
            .env("ARC_TEST_ENV", "here")
            .args(["verify", "env-gate", "--gate", "build"])
            .assert()
            .success();
    }
    repo.commit(&worktree, "work.txt", "a\nb\n", "test: second tree");
    stdout(repo.arc(&worktree).args(["snapshot", "env-gate"]));
    git(&worktree, &["revert", "--no-edit", "HEAD"]);
    stdout(repo.arc(&worktree).args(["snapshot", "env-gate"]));
    assert_eq!(
        git_out(&worktree, &["rev-parse", "HEAD^{tree}"]),
        first_tree
    );
    if run_elsewhere {
        repo.arc(&worktree)
            .env("ARC_TEST_ENV", "elsewhere")
            .args(["verify", "env-gate", "--gate", "build"])
            .assert()
            .success();
    }
    repo.arc(&worktree)
        .env("ARC_ACTOR", "reviewer")
        .args([
            "review",
            "env-gate",
            "--verdict",
            "approved",
            "--body",
            "ok",
        ])
        .assert()
        .success();
    (worktree, first_head)
}

#[test]
fn newer_other_environment_does_not_hide_an_inherited_pass() {
    let repo = Repo::new();
    let (worktree, first_head) = equal_tree_change(&repo, true, true);
    let ready = status(&repo, "here");
    assert_eq!(ready["ready_to_integrate"], true, "{ready}");
    assert_eq!(ready["gates"][0]["inherited_from"], first_head);
    let pass_id = verification_events(&repo)[0]["event_id"].clone();
    assert_eq!(ready["gates"][0]["evidence_event_id"], pass_id);
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["check", "env-gate", "--json"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_TEST_ENV", "here")
        .args(["integrate", "env-gate"])
        .assert()
        .success();
}

#[test]
fn skip_green_reuses_the_applicable_run_at_the_same_head() {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[gates.build]\ncommand = \"true\"\nenvironment = \"printf %s \\\"$ARC_TEST_ENV\\\"\"\n",
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );
    for environment in ["here", "elsewhere"] {
        repo.arc(&repo.root)
            .env("ARC_TEST_ENV", environment)
            .args(["verify", "env-gate", "--gate", "build"])
            .assert()
            .success();
    }
    repo.arc(&repo.root)
        .env("ARC_TEST_ENV", "here")
        .args(["verify", "env-gate", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("skipped (green at head"));
    assert_eq!(verification_events(&repo).len(), 2);
}

#[test]
fn an_inherited_pass_is_ready_without_the_other_environment_run() {
    let repo = Repo::new();
    let (worktree, first_head) = equal_tree_change(&repo, true, false);
    let ready = status(&repo, "here");
    assert_eq!(ready["ready_to_integrate"], true, "{ready}");
    assert_eq!(ready["gates"][0]["inherited_from"], first_head);
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["check", "env-gate"])
        .assert()
        .success();
}

#[test]
fn another_environment_is_inapplicable_without_a_local_pass() {
    let repo = Repo::new();
    let (worktree, _) = equal_tree_change(&repo, false, true);
    let refused = status(&repo, "here");
    assert_eq!(refused["ready_to_integrate"], false, "{refused}");
    assert_eq!(refused["gates"][0]["environment"]["inapplicable"], true);
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["check", "env-gate"])
        .assert()
        .code(5);
    repo.arc(&repo.root)
        .env("ARC_TEST_ENV", "here")
        .args(["integrate", "env-gate"])
        .assert()
        .code(5);
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["verify", "env-gate", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("verification: Pass"));
    assert_eq!(verification_events(&repo).len(), 2);
}

#[test]
fn a_newer_run_under_another_declaration_does_not_hide_the_pass() {
    let repo = Repo::new();
    let (worktree, _) = equal_tree_change(&repo, true, false);
    repo.declare_gates_locally(
        "[gates.build]\ncommand = \"test -f README.md\"\nenvironment = \"printf %s \\\"$ARC_TEST_ENV\\\"\"\n",
    );
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success();
    repo.declare_gates_locally(
        "[gates.build]\ncommand = \"true\"\nenvironment = \"printf %s \\\"$ARC_TEST_ENV\\\"\"\n",
    );
    let ready = status(&repo, "here");
    assert_eq!(ready["ready_to_integrate"], true, "{ready}");
    assert_eq!(
        ready["gates"][0]["evidence_event_id"],
        verification_events(&repo)[0]["event_id"]
    );
}

#[test]
fn the_newest_run_within_the_same_key_decides() {
    let repo = Repo::new();
    let (worktree, _) = equal_tree_change(&repo, true, false);
    let identity = status(&repo, "here")["gates"][0]["environment"]["current"]
        .as_str()
        .unwrap()
        .to_string();
    repo.arc(&worktree)
        .args([
            "verify",
            "env-gate",
            "--attest",
            "--gate",
            "build",
            "--result",
            "fail",
            "--tested-revision",
            "HEAD",
            "--execution-host",
            "here",
            "--runner",
            "job-1",
            "--environment",
            &identity,
        ])
        .assert()
        .code(1);
    let refused = status(&repo, "here");
    assert_eq!(refused["ready_to_integrate"], false, "{refused}");
    assert_eq!(refused["gates"][0]["result"], "fail", "{refused}");
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["check", "env-gate"])
        .assert()
        .code(5);
}

#[test]
fn verify_against_reuses_the_applicable_run_at_the_merged_tree() {
    let repo = Repo::new();
    repo.declare_gates_locally(
        "[gates.build]\ncommand = \"true\"\nenvironment = \"printf %s \\\"$ARC_TEST_ENV\\\"\"\n",
    );
    stdout(repo.arc(&repo.root).args(["begin", "env-gate"]));
    let worktree = repo.home.join(".worktrees/repo-env-gate");
    repo.commit(&worktree, "work.txt", "a\n", "test: change content");
    stdout(repo.arc(&worktree).args(["snapshot", "env-gate"]));
    repo.commit(&repo.root, "other.txt", "b\n", "test: target content");
    for environment in ["here", "elsewhere"] {
        repo.arc(&worktree)
            .env("ARC_TEST_ENV", environment)
            .args(["verify", "env-gate", "--against", "master"])
            .assert()
            .success();
    }
    repo.arc(&worktree)
        .env("ARC_TEST_ENV", "here")
        .args(["verify", "env-gate", "--against", "master", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "skipped (green at the merged tree",
        ));
    assert_eq!(verification_events(&repo).len(), 2);
}

#[test]
fn a_gate_counts_evidence_only_in_the_environment_its_probe_reports() {
    let repo = Repo::new();
    write_env_gate(&repo);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .env("ARC_TEST_ENV", "A")
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success();

    // Under the environment that produced it, the evidence is green and the
    // probe's digest is on the record.
    let state = status(&repo, "A");
    let gate = &state["gates"][0];
    assert_eq!(gate["green_at_head"], true, "{state}");
    let identity_a = gate["environment"]["current"].as_str().unwrap().to_string();
    assert_eq!(gate["environment"]["evidence"], identity_a, "{state}");

    // The recorded event names the digest and the store that produced it.
    let events = verification_events(&repo);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["environment"]["identity"], identity_a);
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(
        events[0]["environment"]["producing_store"],
        config["repository_id"]
    );

    // Elsewhere the same evidence is inapplicable, not missing: the gate says
    // both identities rather than claiming nothing ran.
    let state = status(&repo, "B");
    let gate = &state["gates"][0];
    assert_eq!(gate["green_at_head"], false, "{state}");
    assert_eq!(gate["environment"]["inapplicable"], true, "{state}");
    assert_eq!(gate["environment"]["evidence"], identity_a, "{state}");
    let identity_b = gate["environment"]["current"].as_str().unwrap().to_string();
    assert_ne!(identity_a, identity_b);
    let explain = stdout_any_status(repo.arc(&repo.root).env("ARC_TEST_ENV", "B").args([
        "check",
        "env-gate",
        "--explain",
    ]));
    assert!(explain.contains(&identity_a), "{explain}");
    assert!(explain.contains(&identity_b), "{explain}");

    // Reuse is reuse of a run in this environment, so skip-green reruns.
    repo.arc(&repo.root)
        .env("ARC_TEST_ENV", "B")
        .args(["verify", "env-gate", "--all", "--skip-green"])
        .assert()
        .success()
        .stdout(predicates::str::contains("verification: Pass"));
    let state = status(&repo, "B");
    assert_eq!(state["gates"][0]["green_at_head"], true, "{state}");
    assert_eq!(state["gates"][0]["environment"]["current"], identity_b);

    // Attested evidence names the environment itself, and stays attested.
    repo.arc(&repo.root)
        .env("ARC_TEST_ENV", "A")
        .args([
            "verify",
            "env-gate",
            "--attest",
            "--gate",
            "build",
            "--result",
            "pass",
            "--tested-revision",
            "HEAD",
            "--execution-host",
            "elsewhere",
            "--runner",
            "job-1",
            "--environment",
            &identity_b,
        ])
        .assert()
        .success();
    let state = status(&repo, "B");
    assert_eq!(state["gates"][0]["green_at_head"], true, "{state}");
    assert_eq!(state["gates"][0]["attested"], true, "{state}");

    // An attested run of a probe gate must name the environment it applies
    // to, and an identity without attestation applies to nothing arc can
    // compare it against.
    repo.arc(&repo.root)
        .args([
            "verify",
            "env-gate",
            "--attest",
            "--gate",
            "build",
            "--result",
            "pass",
            "--tested-revision",
            "HEAD",
            "--execution-host",
            "elsewhere",
            "--runner",
            "job-2",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires --environment"));
    repo.arc(&repo.root)
        .args([
            "verify",
            "env-gate",
            "--gate",
            "build",
            "--environment",
            identity_b.as_str(),
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("only valid with --attest"));
}

#[test]
fn evidence_recorded_without_an_identity_satisfies_only_probe_less_gates() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.build]\ncommand = \"true\"\n");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success();

    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    assert_eq!(state["gates"][0]["green_at_head"], true, "{state}");
    assert!(state["gates"][0]["environment"].is_null(), "{state}");

    // Declaring a probe afterwards does not retroactively qualify the run.
    repo.declare_gates_locally("[gates.build]\ncommand = \"true\"\nenvironment = \"true\"\n");
    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    assert_eq!(state["gates"][0]["green_at_head"], false, "{state}");
    let explain = stdout_any_status(
        repo.arc(&repo.root)
            .args(["check", "env-gate", "--explain"]),
    );
    assert!(explain.contains("no environment identity"), "{explain}");
}

#[test]
fn gates_sharing_a_probe_run_it_once_per_evaluation() {
    let repo = Repo::new();
    let counter = repo.home.join("probe-runs");
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[gates.alpha]\ncommand = \"true\"\nenvironment = {probe:?}\n\
             [gates.beta]\ncommand = \"true\"\nenvironment = {probe:?}\n",
            probe = "printf %s A && echo tick >> \"$ARC_TEST_COUNTER\""
        ),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: two gates share a probe"],
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );

    let arc = |args: &[&str]| {
        stdout(
            repo.arc(&repo.root)
                .env("ARC_TEST_COUNTER", &counter)
                .args(args),
        )
    };
    arc(&["verify", "env-gate", "--all"]);
    let after_verify = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(after_verify, 1, "one probe run answers both gates");

    arc(&["status", "env-gate"]);
    let after_status = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(
        after_status, 2,
        "status reads the probe once per evaluation"
    );
}

#[test]
fn a_failed_probe_is_not_an_identity() {
    let repo = Repo::new();
    write_gate(&repo, "true", "command -v arc-absent-probe-tool", None);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );
    let home_a = repo.home.join("home-a");
    let home_b = repo.home.join("home-b");
    fs::create_dir_all(&home_a).unwrap();
    fs::create_dir_all(&home_b).unwrap();

    // In both environments the probe fails the same way and prints nothing.
    // A digest of that empty output would collide, making evidence from one
    // environment answer for the other; a failed run yields no identity.
    repo.arc(&repo.root)
        .env("HOME", &home_a)
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success()
        .stderr(predicates::str::contains("yielded no identity"));

    let events = verification_events(&repo);
    let evidence = events.last().unwrap();
    assert!(evidence["environment"].is_null(), "{evidence}");

    for home in [&home_a, &home_b] {
        let state = json_stdout(
            repo.arc(&repo.root)
                .env("HOME", home)
                .args(["status", "env-gate"]),
        );
        let gate = &state["gates"][0];
        assert_eq!(gate["green_at_head"], false, "{state}");
        assert_eq!(gate["environment"]["unknown"], true, "{state}");
    }

    // Recorded evidence that names some environment still cannot count when
    // the probe fails here: the failure is named, not the difference.
    repo.arc(&repo.root)
        .env("HOME", &home_a)
        .args([
            "verify",
            "env-gate",
            "--attest",
            "--gate",
            "build",
            "--result",
            "pass",
            "--tested-revision",
            "HEAD",
            "--execution-host",
            "elsewhere",
            "--runner",
            "job-1",
            "--environment",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ])
        .assert()
        .success();
    for home in [&home_a, &home_b] {
        let state = json_stdout(
            repo.arc(&repo.root)
                .env("HOME", home)
                .args(["status", "env-gate"]),
        );
        let gate = &state["gates"][0];
        assert_eq!(gate["green_at_head"], false, "{state}");
        assert!(
            gate["environment"]["probe_failed"]
                .as_str()
                .is_some_and(|reason| reason.contains("exited")),
            "{state}"
        );
    }
}

#[test]
fn a_probe_that_prints_nothing_or_fails_yields_no_identity() {
    let repo = Repo::new();
    write_gate(&repo, "true", "true", None);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success()
        .stderr(predicates::str::contains("yielded no identity"));
    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    assert_eq!(state["gates"][0]["green_at_head"], false, "{state}");
    assert_eq!(state["gates"][0]["environment"]["unknown"], true, "{state}");

    // An identity on stdout is not one when the run failed.
    write_gate(&repo, "true", "printf %s A; exit 3", None);
    repo.arc(&repo.root)
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success()
        .stderr(predicates::str::contains("exited 3"));
    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    assert_eq!(state["gates"][0]["green_at_head"], false, "{state}");
    assert_eq!(state["gates"][0]["environment"]["unknown"], true, "{state}");
}

#[test]
fn probe_stderr_does_not_change_the_identity() {
    let repo = Repo::new();
    write_gate(&repo, "true", "printf %s A", None);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success();

    // The same stdout with incidental stderr still names the same
    // environment. The change's head tree carrying the evidence is unchanged.
    write_gate(&repo, "true", "printf %s A; echo warning >&2", None);
    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    let gate = &state["gates"][0];
    assert_eq!(gate["green_at_head"], true, "{state}");
    let expected = format!("sha256:{}", hex::encode(Sha256::digest(b"A")));
    assert_eq!(gate["environment"]["current"], expected, "{state}");
    assert_eq!(gate["environment"]["evidence"], expected, "{state}");
}

#[test]
fn an_overrunning_probe_is_bounded_and_yields_no_identity() {
    let repo = Repo::new();
    write_gate(&repo, "true", "sleep 30; printf %s A", Some("1s"));
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "env-gate", "--no-worktree"]),
    );

    let started = Instant::now();
    repo.arc(&repo.root)
        .args(["verify", "env-gate", "--gate", "build"])
        .assert()
        .success()
        .stderr(predicates::str::contains("overran"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the probe should be killed at its bound"
    );

    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    let gate = &state["gates"][0];
    assert_eq!(gate["green_at_head"], false, "{state}");
    assert_eq!(gate["environment"]["unknown"], true, "{state}");

    // An attested identity does not save it: the probe fails here, so no
    // receipt can be shown to describe this environment.
    repo.arc(&repo.root)
        .args([
            "verify",
            "env-gate",
            "--attest",
            "--gate",
            "build",
            "--result",
            "pass",
            "--tested-revision",
            "HEAD",
            "--execution-host",
            "elsewhere",
            "--runner",
            "job-1",
            "--environment",
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        ])
        .assert()
        .success();
    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    let gate = &state["gates"][0];
    assert_eq!(gate["green_at_head"], false, "{state}");
    assert!(
        gate["environment"]["probe_failed"]
            .as_str()
            .is_some_and(|reason| reason.contains("overran")),
        "{state}"
    );
}
