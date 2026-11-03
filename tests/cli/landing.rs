//! Must-land-together relations and `arc land`: changes that reach their
//! target in one guarded transition, or not at all.

use crate::common::*;
use serde_json::Value;

/// A repository declaring one gate every change passes.
fn gated_repo() -> Repo {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.smoke]\ncommand = \"test -f README.md\"\n");
    repo
}

/// Open a change on `base` (the target tip when absent), commit one file,
/// snapshot, and approve it. Gates are left to the caller.
fn reviewed_change(repo: &Repo, slug: &str, base: Option<&str>) -> (String, PathBuf, String) {
    let mut args = vec!["begin", slug];
    if let Some(base) = base {
        args.extend(["--base", base]);
    }
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(&args)));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    let head = repo.head(&worktree);
    (change_id, worktree, head)
}

/// A reviewed change with its gates green at its own head.
fn ready_change(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
    let (change_id, worktree, head) = reviewed_change(repo, slug, None);
    repo.arc(&worktree)
        .args(["verify", slug, "--all"])
        .assert()
        .success();
    (change_id, worktree, head)
}

fn relate(repo: &Repo, a: &str, b: &str) {
    repo.arc(&repo.root)
        .args(["metadata", a, "--lands-with", b])
        .assert()
        .success();
}

fn status(repo: &Repo, change: &str) -> Value {
    json_stdout_any_status(repo.arc(&repo.root).args(["status", change]))
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Claim: a change whose must-land-together partner is still open cannot
/// integrate alone by any path, and each refusal names `arc land` with every
/// open member. Rejects: `integrate`, a queue, `check` or an asserted
/// closure treating the relation as advisory.
#[test]
fn a_change_with_an_open_partner_integrates_only_together() {
    let repo = gated_repo();
    let (a, _, _) = ready_change(&repo, "alpha");
    let (b, _, _) = ready_change(&repo, "beta");
    relate(&repo, &a, &b);

    let check = repo.arc(&repo.root).args(["check", &a]).output().unwrap();
    assert_eq!(check.status.code(), Some(19), "{}", stderr_of(&check));

    let report = status(&repo, &a);
    assert_eq!(report["lands_with"]["members"][0]["change_id"], b.as_str());
    assert_eq!(report["lands_with"]["members"][0]["status"], "open");
    assert_eq!(report["next_action"], "land_together");

    let alone = repo
        .arc(&repo.root)
        .args(["integrate", &a])
        .output()
        .unwrap();
    assert_eq!(alone.status.code(), Some(19), "{}", stderr_of(&alone));
    let mut pair = [a.clone(), b.clone()];
    pair.sort();
    let remedy = format!("arc land {} {}", pair[0], pair[1]);
    assert!(
        String::from_utf8_lossy(&alone.stderr).contains(&remedy),
        "{}",
        stderr_of(&alone)
    );

    let queue = repo
        .arc(&repo.root)
        .args(["integrate", &a, &b])
        .output()
        .unwrap();
    assert_eq!(queue.status.code(), Some(19), "{}", stderr_of(&queue));

    let target = git_out(&repo.root, &["rev-parse", "master"]);
    let head = status(&repo, &b)["current_head"]
        .as_str()
        .unwrap()
        .to_string();
    let asserted = repo
        .arc(&repo.root)
        .args(["close", &b, "--assert-integrated", &head])
        .output()
        .unwrap();
    assert!(!asserted.status.success());
    assert!(
        stderr_of(&asserted).contains(&remedy),
        "{}",
        stderr_of(&asserted)
    );

    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), target);
    assert!(status(&repo, &a)["closure"].is_null());
    assert!(status(&repo, &b)["closure"].is_null());
}

/// Claim: components are the transitive closure of relations, and a member
/// closed without integrating wedges the rest until the relation is
/// withdrawn with a reason. Rejects: a direct-edges-only reading, and a
/// wedged component that lets a member land as if the abandoned one had.
#[test]
fn an_abandoned_member_wedges_its_component_until_the_relation_is_withdrawn() {
    let repo = gated_repo();
    let (a, _, _) = ready_change(&repo, "alpha");
    let (b, _, _) = ready_change(&repo, "beta");
    let (c, _, _) = ready_change(&repo, "gamma");
    relate(&repo, &a, &b);
    relate(&repo, &b, &c);

    let members: Vec<String> = status(&repo, &a)["lands_with"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["change_id"].as_str().unwrap().to_string())
        .collect();
    let mut expected = vec![b.clone(), c.clone()];
    expected.sort();
    assert_eq!(members, expected);

    repo.arc(&repo.root)
        .args(["close", &c, "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", &a, "--remove-lands-with", &b])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--reason"));
    repo.arc(&repo.root)
        .args([
            "metadata",
            &b,
            "--remove-lands-with",
            &a,
            "--reason",
            "a ships first",
        ])
        .assert()
        .success();

    let wedged = repo
        .arc(&repo.root)
        .args(["integrate", &b])
        .output()
        .unwrap();
    assert_eq!(wedged.status.code(), Some(19), "{}", stderr_of(&wedged));
    assert!(
        stderr_of(&wedged).contains(&format!("--remove-lands-with {c}")),
        "{}",
        stderr_of(&wedged)
    );

    repo.arc(&repo.root)
        .args(["integrate", &a])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "metadata",
            &b,
            "--remove-lands-with",
            &c,
            "--reason",
            "gamma was dropped",
        ])
        .assert()
        .success();
    // Free of its component, beta answers to the next blocker: alpha moved
    // the fast-forward target under it.
    let freed = repo.arc(&repo.root).args(["check", &b]).output().unwrap();
    assert_eq!(freed.status.code(), Some(11), "{}", stderr_of(&freed));
}

/// Claim: a relation names two open changes with one target, and is one
/// repository event. Rejects: relating a closed change or a change to
/// itself, and a relation stored on only one change's ledger.
#[test]
fn a_relation_is_one_repository_event_between_open_changes() {
    let repo = gated_repo();
    let (a, _, _) = ready_change(&repo, "alpha");
    let (b, _, _) = ready_change(&repo, "beta");
    let (c, _, _) = ready_change(&repo, "gamma");
    repo.arc(&repo.root)
        .args(["metadata", &a, "--lands-with", &a])
        .assert()
        .failure();
    repo.arc(&repo.root)
        .args(["close", &c, "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", &a, "--lands-with", &c])
        .assert()
        .failure()
        .stderr(predicates::str::contains("closed"));
    relate(&repo, &a, &b);
    repo.arc(&repo.root)
        .args(["metadata", &b, "--lands-with", &a])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already lands with"));

    let events: Vec<Value> = stdout(repo.arc(&repo.root).args(["events", "--repository"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|event: &Value| event["event_type"] == "lands-with-declared")
        .collect();
    assert_eq!(events.len(), 1);
    let mut pair = vec![a.clone(), b.clone()];
    pair.sort();
    assert_eq!(events[0]["changes"], serde_json::json!(pair));
    assert_eq!(
        status(&repo, &b)["lands_with"]["members"][0]["change_id"],
        a.as_str()
    );
}

/// A two-member stack: `lower` on the target tip, `upper` on `lower`'s head,
/// each reviewed at its own patchset with no gate run anywhere yet.
fn stack(repo: &Repo) -> ((String, PathBuf, String), (String, PathBuf, String)) {
    let lower = reviewed_change(repo, "lower", None);
    let upper = reviewed_change(repo, "upper", Some(&lower.2));
    (lower, upper)
}

fn repository_events(repo: &Repo) -> Vec<Value> {
    stdout(repo.arc(&repo.root).args(["events", "--repository"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn change_events(repo: &Repo, change: &str) -> Vec<Value> {
    stdout(repo.arc(&repo.root).args(["events", "--change", change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Claim: a stack lands with one gate run at the top head; both members read
/// integrated via the landing, each keeping its own approval and its own
/// integrated range, and the round-trip statistics list both. Rejects: a gate
/// run per member, a member closed through another's verdict, and a member
/// whose integrated diff shows the whole landing.
#[test]
fn a_stack_lands_with_one_gate_run_and_each_member_keeps_its_own_range() {
    let repo = gated_repo();
    let ((lower, _, lower_head), (upper, _, upper_head)) = stack(&repo);
    let before = git_out(&repo.root, &["rev-parse", "master"]);

    let landed = repo
        .arc(&repo.root)
        .args(["land", &lower, &upper])
        .output()
        .unwrap();
    assert!(landed.status.success(), "{}", stderr_of(&landed));
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), upper_head);
    assert_eq!(git_out(&repo.root, &["rev-parse", "HEAD"]), upper_head);

    // One gate run, recorded on the top member and naming the attempt.
    let runs: Vec<Value> = [&lower, &upper]
        .iter()
        .flat_map(|change| change_events(&repo, change))
        .filter(|event| event["event_type"] == "verification-recorded")
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let attempt = runs[0]["landing"].as_str().unwrap().to_string();
    assert_eq!(runs[0]["revision"], upper_head.as_str());

    let completions: Vec<Value> = repository_events(&repo)
        .into_iter()
        .filter(|event| event["event_type"] == "landing-completed")
        .collect();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0]["attempt"], attempt.as_str());

    for (change, base, head) in [
        (&lower, &before, &lower_head),
        (&upper, &lower_head, &upper_head),
    ] {
        let report = status(&repo, change);
        let closure = &report["closure"];
        assert_eq!(closure["outcome"], "integrated", "{report}");
        assert_eq!(closure["integration"], "guarded");
        assert_eq!(closure["landing"]["attempt"], attempt.as_str());
        assert_eq!(closure["target_before"], before.as_str());
        assert_eq!(closure["range_base"], base.as_str());
        assert_eq!(closure["integrated_commit"], head.as_str());
        let verdict = closure["authorization"]["verdict_event_id"]
            .as_str()
            .unwrap();
        assert!(
            change_events(&repo, change)
                .iter()
                .any(|event| event["event_id"] == verdict
                    && event["event_type"] == "verdict-recorded")
        );

        let diff = stdout(
            repo.arc(&repo.root)
                .args(["diff", change, "--integrated", "--stat"]),
        );
        assert!(
            diff.contains(&format!("{}.txt", report["slug"].as_str().unwrap())),
            "{diff}"
        );
        assert_eq!(diff.matches(".txt").count(), 1, "{change}: {diff}");
    }

    let stats = json_stdout(
        repo.arc(&repo.root)
            .args(["stats", "--round-trip", "--json"]),
    );
    let listed = stats.to_string();
    assert!(
        listed.contains(&lower) && listed.contains(&upper),
        "{stats}"
    );
}

const LANDER: &str = "019a0000-0000-7000-8000-0000000000d4";

/// A Claude recording with one request, so the lander's session reads.
fn lander_recording(repo: &Repo) {
    let path = repo
        .home
        .join(".claude/projects/-fixture-project")
        .join(format!("{LANDER}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lines = [
        serde_json::json!({"type": "user", "sessionId": LANDER, "uuid": "prompt-1", "timestamp": "2026-01-01T10:00:00Z", "cwd": "/fixture/project", "message": {"role": "user", "content": "Land."}}),
        serde_json::json!({"type": "assistant", "sessionId": LANDER, "uuid": "assistant-1", "requestId": "request-1", "timestamp": "2026-01-01T10:00:01Z", "cwd": "/fixture/project",
            "message": {"role": "assistant", "model": "claude-opus-5-5", "content": [{"type": "text", "text": "Landed."}],
                "usage": {"input_tokens": 9, "output_tokens": 3, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}}),
    ];
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

/// Claim: the session that ran a landing is each member's integrator in cost
/// attribution, as the session that ran `integrate` is. Rejects a reader that
/// looks only at a member's own events, where the lander appears on no
/// member at all.
#[test]
fn the_lander_is_each_members_integrator_in_cost_attribution() {
    let repo = gated_repo();
    lander_recording(&repo);
    let ((lower, _, _), (upper, _, _)) = stack(&repo);
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", LANDER)
        .env_remove("ARC_ACTOR")
        .args(["land", &lower, &upper])
        .assert()
        .success();
    let key = format!("claude:{LANDER}");
    for change in [&lower, &upper] {
        let view = json_stdout(
            repo.arc(&repo.root)
                .args(["cost", "show", "--json", change]),
        );
        let row = view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == key.as_str())
            .unwrap_or_else(|| panic!("{change}: no lander row: {view:#}"));
        assert!(
            row["roles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|role| role == "integrator"),
            "{change}: {row:#}"
        );
    }
}

/// Claim: every member's declarations are read at the revision the landing
/// started from, so a member that weakens policy does not govern the members
/// landed beside it. Rejects a closure that records a member's own range
/// base as its policy anchor, under which the later member's author could
/// approve their own audit.
#[test]
fn a_member_weakening_policy_does_not_govern_the_audits_of_a_later_member() {
    let repo = gated_repo();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(
        &repo.root,
        &["commit", "-q", "-m", "policy: forbid self-approval"],
    );
    let reviewer = |cwd: &Path, slug: &str| {
        repo.arc(cwd)
            .env("ARC_ACTOR", "reviewer")
            .env("ARC_SESSION", "session-r")
            .args(["review", slug, "--verdict", "approved"])
            .assert()
            .success();
    };

    let lower = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "lower"])));
    let lower_tree = repo.home.join(".worktrees/repo-lower");
    fs::write(
        lower_tree.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = false\n",
    )
    .unwrap();
    git(
        &lower_tree,
        &["commit", "-q", "-am", "policy: allow self-approval"],
    );
    stdout(repo.arc(&lower_tree).args(["snapshot", "lower"]));
    reviewer(&lower_tree, "lower");
    let lower_head = repo.head(&lower_tree);

    let upper = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "upper",
        "--base",
        &lower_head,
    ])));
    let upper_tree = repo.home.join(".worktrees/repo-upper");
    repo.commit(&upper_tree, "upper.txt", "upper\n", "feat: upper");
    stdout(repo.arc(&upper_tree).args(["snapshot", "upper"]));
    reviewer(&upper_tree, "upper");

    let landed = repo
        .arc(&repo.root)
        .args(["land", &lower, &upper])
        .output()
        .unwrap();
    assert!(landed.status.success(), "{}", stderr_of(&landed));

    let audit = repo
        .arc(&repo.root)
        .args(["audit", &upper, "--verdict", "approved"])
        .output()
        .unwrap();
    assert!(
        !audit.status.success(),
        "the upper author approved its own audit"
    );
    assert!(
        stderr_of(&audit).contains("policy declared by .arc/policy.toml"),
        "{}",
        stderr_of(&audit)
    );

    let diff = stdout(
        repo.arc(&repo.root)
            .args(["diff", &upper, "--integrated", "--stat"]),
    );
    assert!(
        diff.contains("upper.txt") && !diff.contains("policy.toml"),
        "{diff}"
    );

    // An independent audit of the member's own landed revision records.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "reviewer")
        .env("ARC_SESSION", "session-r")
        .args(["audit", &upper, "--verdict", "approved"])
        .assert()
        .success()
        .stdout(predicates::str::contains(repo.head(&upper_tree)));
}

/// Claim: members related as must-land-together integrate only in one landing
/// naming both. Rejects `integrate` of one member and a landing of one
/// member alone.
#[test]
fn related_members_land_only_together() {
    let repo = gated_repo();
    let ((lower, _, _), (upper, _, upper_head)) = stack(&repo);
    relate(&repo, &lower, &upper);
    let before = git_out(&repo.root, &["rev-parse", "master"]);

    let alone = repo
        .arc(&repo.root)
        .args(["integrate", &lower])
        .output()
        .unwrap();
    assert_eq!(alone.status.code(), Some(19), "{}", stderr_of(&alone));
    let single = repo
        .arc(&repo.root)
        .args(["land", &lower])
        .output()
        .unwrap();
    assert_eq!(single.status.code(), Some(19), "{}", stderr_of(&single));
    assert!(
        stderr_of(&single).contains(&upper),
        "{}",
        stderr_of(&single)
    );
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);

    repo.arc(&repo.root)
        .args(["land", &lower, &upper])
        .assert()
        .success();
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), upper_head);
    for change in [&lower, &upper] {
        assert_eq!(status(&repo, change)["closure"]["outcome"], "integrated");
    }
}

/// Claim: a gate only the top member declares runs at the top head, and its
/// failure closes nothing and leaves no member pending. Rejects a landing
/// that runs only the gates the first member declares, and an attempt left
/// holding its members after a failed gate.
#[test]
fn a_gate_the_top_member_adds_fails_and_nothing_closes() {
    let repo = gated_repo();
    let (lower, _, lower_head) = reviewed_change(&repo, "lower", None);
    let upper = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "upper",
        "--base",
        &lower_head,
    ])));
    let upper_tree = repo.home.join(".worktrees/repo-upper");
    fs::write(
        upper_tree.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"test -f README.md\"\n\n[gates.strict]\ncommand = \"false\"\n",
    )
    .unwrap();
    git(
        &upper_tree,
        &["commit", "-q", "-am", "feat: require strict"],
    );
    stdout(repo.arc(&upper_tree).args(["snapshot", "upper"]));
    repo.arc(&upper_tree)
        .args(["review", "upper", "--verdict", "approved"])
        .assert()
        .success();
    let before = git_out(&repo.root, &["rev-parse", "master"]);

    let plan =
        json_stdout(
            repo.arc(&repo.root)
                .args(["land", &lower, &upper, "--dry-run", "--json"]),
        );
    assert_eq!(plan["schema"], "arc-landing-plan/1");
    assert_eq!(
        plan["gates"]["strict"]["required_by"],
        serde_json::json!([upper])
    );

    let landed = repo
        .arc(&repo.root)
        .args(["land", &lower, &upper])
        .output()
        .unwrap();
    assert_eq!(landed.status.code(), Some(5), "{}", stderr_of(&landed));
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    for change in [&lower, &upper] {
        let report = status(&repo, change);
        assert!(report["closure"].is_null(), "{report}");
        assert!(report.get("pending_landing").is_none(), "{report}");
    }
    let failed: Vec<Value> = change_events(&repo, &upper)
        .into_iter()
        .filter(|event| event["event_type"] == "verification-recorded" && event["gate"] == "strict")
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["result"], "fail");
    assert!(repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "landing-abandoned"));
}

/// Claim: a membership that does not form one chain on the target refuses
/// before anything is written, naming the member that does not stand on its
/// predecessor. Rejects a landing that replays, or that moves the target to
/// the last member while skipping another.
#[test]
fn members_that_do_not_chain_refuse_naming_them() {
    let repo = gated_repo();
    let (first, _, _) = reviewed_change(&repo, "first", None);
    let (second, _, _) = reviewed_change(&repo, "second", None);
    let before = git_out(&repo.root, &["rev-parse", "master"]);
    let refused = repo
        .arc(&repo.root)
        .args(["land", &first, &second])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(11), "{}", stderr_of(&refused));
    assert!(
        stderr_of(&refused).contains(&second),
        "{}",
        stderr_of(&refused)
    );
    assert!(
        !stderr_of(&refused).contains(&format!("{first} stands")),
        "{}",
        stderr_of(&refused)
    );
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    assert!(!repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "landing-prepared"));
}

/// Claim: a prerequisite that is an earlier member of the same landing is
/// satisfied by it, and the authorization basis says so. Rejects reading the
/// prerequisite as unintegrated, and a basis that names no landing.
#[test]
fn an_earlier_member_satisfies_a_prerequisite_inside_the_landing() {
    let repo = gated_repo();
    let (lower, _, lower_head) = reviewed_change(&repo, "lower", None);
    let upper = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "upper",
        "--base",
        &lower_head,
        "--blocked-by",
        &lower,
    ])));
    let upper_tree = repo.home.join(".worktrees/repo-upper");
    repo.commit(&upper_tree, "upper.txt", "upper\n", "feat: upper");
    stdout(repo.arc(&upper_tree).args(["snapshot", "upper"]));
    repo.arc(&upper_tree)
        .args(["review", "upper", "--verdict", "approved"])
        .assert()
        .success();

    let reversed = repo
        .arc(&repo.root)
        .args(["land", &upper, &lower])
        .output()
        .unwrap();
    assert!(!reversed.status.success());
    repo.arc(&repo.root)
        .args(["land", &lower, &upper])
        .assert()
        .success();
    let report = status(&repo, &upper);
    let prerequisite = &report["closure"]["authorization"]["prerequisites"][0];
    assert_eq!(prerequisite["change_id"], lower.as_str());
    assert_eq!(prerequisite["integrated_commit"], lower_head.as_str());
    assert_eq!(
        prerequisite["landing"],
        report["closure"]["landing"]["attempt"]
    );
    assert_eq!(
        prerequisite["closure_event_id"],
        status(&repo, &lower)["closure"]["event_id"]
    );
}

/// Claim: `--debt` lands a member no approval covers and records the debt
/// `integrate --debt` would, derived from the completion and owed once the
/// member integrates. Rejects a landing that drops the unapproved member's
/// obligation, and one that lands it without `--debt`.
#[test]
fn debt_lands_an_unapproved_member_and_keeps_its_obligation() {
    let repo = gated_repo();
    let (lower, _, lower_head) = reviewed_change(&repo, "lower", None);
    let upper = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "upper",
        "--base",
        &lower_head,
    ])));
    let upper_tree = repo.home.join(".worktrees/repo-upper");
    repo.commit(&upper_tree, "upper.txt", "upper\n", "feat: upper");
    stdout(repo.arc(&upper_tree).args(["snapshot", "upper"]));

    let refused = repo
        .arc(&repo.root)
        .args(["land", &lower, &upper])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(3), "{}", stderr_of(&refused));

    repo.arc(&repo.root)
        .args([
            "land",
            &lower,
            &upper,
            "--debt",
            "nobody read the upper slice",
        ])
        .assert()
        .success();
    let report = status(&repo, &upper);
    assert_eq!(report["closure"]["outcome"], "integrated");
    assert_eq!(report["debt"]["missing"], "nothing-read", "{report}");
    assert_eq!(report["debt_standing"], "owed", "{report}");
    assert_eq!(
        report["closure"]["authorization"]["audit_debt_event_id"],
        report["debt"]["event_id"]
    );
    assert!(status(&repo, &lower)["debt"].is_null());
}
