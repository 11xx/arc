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
    let explain = stdout(repo.arc(&repo.root).env("ARC_TEST_ENV", "B").args([
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
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: declare a probe-less gate"],
    );
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
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\nenvironment = \"true\"\n",
    )
    .unwrap();
    let state = json_stdout(repo.arc(&repo.root).args(["status", "env-gate"]));
    assert_eq!(state["gates"][0]["green_at_head"], false, "{state}");
    let explain = stdout(
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
