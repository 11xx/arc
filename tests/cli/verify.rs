use super::common::*;

fn write_two_gates(repo: &Repo, first: &str, second: &str) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!("[gates.alpha]\ncommand = {first:?}\n[gates.beta]\ncommand = {second:?}\n"),
    )
    .unwrap();
}

#[test]
fn verify_all_records_every_passing_gate_and_summary() {
    let repo = Repo::new();
    write_two_gates(&repo, "true", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "all-pass", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "all-pass", "--all", "--note", "full suite"])
        .assert()
        .success()
        .stdout(predicates::str::contains("gates: 2/2 pass"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "all-pass",
        "--type",
        "verification-recorded",
    ]));
    let values = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["gate"], "alpha");
    assert_eq!(values[1]["gate"], "beta");
    assert_eq!(values[0]["note"], "full suite");
    assert_eq!(values[1]["note"], "full suite");
}

#[test]
fn verify_all_continues_after_a_failure() {
    let repo = Repo::new();
    write_two_gates(&repo, "false", "true");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "all-fail", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "all-fail", "--all"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("gates: 1/2 pass"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "all-fail",
        "--type",
        "verification-recorded",
    ]));
    assert_eq!(events.lines().count(), 2);
}

#[test]
fn verify_all_rejects_conflicting_flags_without_appending() {
    let repo = Repo::new();
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "all-flags",
        "--no-worktree",
    ])));
    let before = event_count(&repo, &change_id);

    for args in [
        vec!["verify", "all-flags", "--all", "--gate", "x"],
        vec![
            "verify",
            "all-flags",
            "--all",
            "--attest",
            "--result",
            "pass",
        ],
        vec!["verify", "all-flags", "--all", "--command", "true"],
    ] {
        repo.arc(&repo.root).args(args).assert().failure();
    }
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn verify_all_rejects_missing_or_empty_gates_without_appending() {
    for empty_file in [false, true] {
        let repo = Repo::new();
        if empty_file {
            fs::create_dir_all(repo.root.join(".arc")).unwrap();
            fs::write(repo.root.join(".arc/gates.toml"), "").unwrap();
        }
        let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
            "begin",
            "all-empty",
            "--no-worktree",
        ])));
        let before = event_count(&repo, &change_id);
        repo.arc(&repo.root)
            .args(["verify", "all-empty", "--all"])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "no gates declared for profile local",
            ));
        assert_eq!(event_count(&repo, &change_id), before);
    }
}

/// `verify --attest --result` records externally observed evidence without
/// running anything, counts it toward gate green-ness, and flags it attested
/// in both the machine status and the human render.
#[test]
fn verify_attest_records_gate_evidence_without_running() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    let poison = repo.root.join("EXECUTED");
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[gates.smoke]\ncommand = \"touch '{}' && exit 1\"\n",
            poison.display()
        ),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-att", "--title", "Attested"]),
    );
    let wt = repo.home.join(".worktrees").join("repo-feat-att");
    repo.commit(&wt, "att.txt", "att\n", "feat: add att");
    stdout(repo.arc(&wt).args(["snapshot", "feat-att"]));

    repo.arc(&wt)
        .args([
            "verify",
            "feat-att",
            "--gate",
            "smoke",
            "--attest",
            "--result",
            "pass",
            "--note",
            "ran in sandbox",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("Pass (attested)"));
    assert!(!poison.exists(), "attested verify must not run the command");

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "feat-att"]))).unwrap();
    let gate = &status["gates"][0];
    assert_eq!(gate["name"], "smoke");
    assert_eq!(gate["result"], "pass");
    assert_eq!(gate["green_at_head"], true);
    assert_eq!(gate["attested"], true);

    repo.arc(&repo.root)
        .args(["show", "feat-att"])
        .assert()
        .success()
        .stdout(predicates::str::contains("green at head (attested)"))
        .stdout(predicates::str::contains("Pass (attested)"));
}

/// --attest requires --result; --result without --attest is a usage error; and
/// a failing attestation reports exit 1 while still recording evidence.
#[test]
fn verify_attest_result_flag_pairing_is_enforced() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-pair", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["verify", "feat-pair", "--command", "true", "--attest"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--attest requires --result"));

    repo.arc(&repo.root)
        .args([
            "verify",
            "feat-pair",
            "--command",
            "true",
            "--result",
            "pass",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--result is only valid with --attest",
        ));

    // A failing attestation is recorded and mirrors the result in the exit code.
    repo.arc(&repo.root)
        .args([
            "verify",
            "feat-pair",
            "--command",
            "false",
            "--attest",
            "--result",
            "fail",
        ])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("Fail (attested)"));
}

/// A gate that advances the branch head while running triggers an advisory
/// warning; the recorded evidence stays pinned to the pre-gate revision.
#[test]
fn verify_warns_when_head_moves_during_the_gate() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "feat-move"]));
    let wt = repo.home.join(".worktrees").join("repo-feat-move");
    repo.commit(&wt, "move.txt", "move\n", "feat: add move");
    let pre = repo.head(&wt);

    repo.arc(&wt)
        .args([
            "verify",
            "feat-move",
            "--command",
            "git commit --allow-empty -m moved-during-gate",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains(format!(
            "head moved during verification ({pre}"
        )));

    let post = repo.head(&wt);
    assert_ne!(pre, post, "the gate command must have moved the head");

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "feat-move",
        "--type",
        "verification-recorded",
    ]));
    let recorded: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(recorded["revision"], pre);
}

/// Provenance comparison matches the claim actor against the git identity by
/// name or email. An actor that differs from the author name but equals its
/// email is the same identity, so no mismatch is raised.
#[test]
fn provenance_email_match_suppresses_name_only_false_positive() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "prov-eml"]));
    let wt = repo.home.join(".worktrees").join("repo-prov-eml");
    repo.arc(&wt)
        .env("ARC_ACTOR", "tester@example.invalid")
        .args(["claim", "prov-eml"])
        .assert()
        .success();
    repo.commit(&wt, "prov.txt", "prov\n", "feat: prov");
    repo.arc(&wt)
        .args(["snapshot", "prov-eml"])
        .assert()
        .success();

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "prov-eml"]))).unwrap();
    assert_eq!(status["latest_patchset"]["author"]["name"], "Tester");
    assert_eq!(
        status["latest_patchset"]["author"]["email"],
        "tester@example.invalid"
    );
    assert_eq!(
        status["latest_patchset"]["claim_actor"],
        "tester@example.invalid"
    );
    assert_eq!(status["latest_patchset"]["provenance_mismatch"], false);
    assert_eq!(status["claim"]["provenance_mismatch"], false);

    repo.arc(&repo.root)
        .args(["show", "prov-eml"])
        .assert()
        .success()
        .stdout(predicates::str::contains("PROVENANCE MISMATCH").not());
}
