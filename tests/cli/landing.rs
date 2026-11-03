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
/// attribution, as the session that ran `integrate` is, and its footprint
/// selects every member it landed. Rejects a reader that looks only at a
/// member's own events, where the lander appears on no member but the top
/// one, whose gate run it recorded.
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
    let footprint =
        json_stdout(
            repo.arc(&repo.root)
                .args(["cost", "show", "--json", "--footprint", &key]),
        );
    let selected = footprint["work"].to_string();
    assert!(selected.contains(&lower), "{footprint:#}");
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

/// Claim: a prerequisite that is an earlier member of the same landing is
/// satisfied by it, and the authorization basis says so; members land in
/// dependency order whatever order they are named in. Rejects reading the
/// prerequisite as unintegrated, a basis that names no landing, and landing
/// a member before its prerequisite.
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

    repo.arc(&repo.root)
        .args(["land", &upper, &lower])
        .assert()
        .success();
    let report = status(&repo, &upper);
    assert_eq!(report["closure"]["range_base"], lower_head.as_str());
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

/// Wait until the landing's single gate run at the top head is recorded.
fn wait_for_gate_run(repo: &Repo, top: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !change_events(repo, top)
        .iter()
        .any(|event| event["event_type"] == "verification-recorded" && event["landing"].is_string())
    {
        assert!(Instant::now() < deadline, "the landing never gated");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Claim: whatever moves between the gate run and commit refuses the landing
/// at commit, naming the moved field, and closes nothing. Rejects a commit
/// that trusts the plan it gated: a moved target, a new commit on a member,
/// a changes-requested verdict, a hold, or a relation to a change outside
/// the landing.
#[test]
fn a_fact_that_moves_during_the_gate_refuses_commit_naming_it() {
    type Mutate = fn(&Repo, &str, &Path);
    let cases: [(&str, Mutate, &str); 5] = [
        (
            "target",
            |repo, _, _| repo.commit(&repo.root, "moved.txt", "moved\n", "chore: move target"),
            "target_before",
        ),
        (
            "commit",
            |repo, _, lower_tree| repo.commit(lower_tree, "late.txt", "late\n", "feat: late"),
            ".branch_head",
        ),
        (
            "verdict",
            |repo, lower, _| {
                repo.arc(&repo.root)
                    .env("ARC_ACTOR", "reviewer")
                    .env("ARC_SESSION", "session-r")
                    .args([
                        "review",
                        lower,
                        "--verdict",
                        "changes-requested",
                        "--cause",
                        "executor",
                        "--body",
                        "not yet",
                    ])
                    .assert()
                    .success();
            },
            ".verdicts",
        ),
        (
            "hold",
            |repo, lower, _| {
                repo.arc(&repo.root)
                    .args(["hold", lower, "--reason", "wait"])
                    .assert()
                    .success();
            },
            ".holds",
        ),
        (
            "relation",
            |repo, lower, _| {
                let (outside, _, _) = reviewed_change(repo, "outside", None);
                relate(repo, lower, &outside);
            },
            ".lands_with",
        ),
    ];
    for (name, mutate, field) in cases {
        let repo = gated_repo();
        let ((lower, lower_tree, _), (upper, _, _)) = stack(&repo);
        let before = git_out(&repo.root, &["rev-parse", "master"]);
        let release = repo.home.join("release-land");
        let mut child = spawn_arc_keeping_stderr_with_env(
            &repo,
            &repo.root,
            &["land", &lower, &upper],
            &[("ARC_LAND_PAUSE", release.to_str().unwrap())],
        );
        wait_for_gate_run(&repo, &upper);
        mutate(&repo, &lower, &lower_tree);
        fs::write(&release, "").unwrap();
        let status_code = wait_for_exit(&mut child);
        let stderr = child_stderr(&mut child);
        assert!(!status_code.success(), "{name}: landed anyway");
        let named = if field.starts_with('.') {
            format!("{lower}{field}")
        } else {
            field.to_string()
        };
        assert!(
            stderr.contains(&format!("{named} moved")),
            "{name}: {stderr}"
        );
        if name != "target" {
            assert_eq!(
                git_out(&repo.root, &["rev-parse", "master"]),
                before,
                "{name}"
            );
        }
        for change in [&lower, &upper] {
            let report = status(&repo, change);
            assert!(report["closure"].is_null(), "{name}: {report}");
            assert!(report.get("pending_landing").is_none(), "{name}: {report}");
        }
    }
}

/// The landing attempt the repository's prepared event names.
fn prepared_attempt(repo: &Repo) -> String {
    repository_events(repo)
        .into_iter()
        .find(|event| event["event_type"] == "landing-prepared")
        .and_then(|event| event["attempt"].as_str().map(str::to_string))
        .expect("a landing was prepared")
}

/// Claim: an attempt interrupted anywhere is finished or abandoned by
/// `--recover`, run twice, so that no member reads integrated unless the
/// target holds the landing and none stays open when it does. Rejects
/// recovery that publishes before arc's own ref transaction, that leaves a
/// moved target unclosed, or that changes anything on a second run.
#[test]
fn recovery_after_every_crash_point_closes_exactly_what_landed() {
    for point in ["prepared", "gate", "authorized", "moved", "applied"] {
        let repo = Repo::new();
        repo.redeclare_gates(
            "[gates.smoke]\ncommand = \"if test -e \\\"$HOME/kill-lander\\\"; then kill -9 $PPID; fi; test -f README.md\"\n",
        );
        let ((lower, _, _), (upper, _, upper_head)) = stack(&repo);
        let before = git_out(&repo.root, &["rev-parse", "master"]);
        let kill = repo.home.join("kill-lander");
        let mut land = repo.arc(&repo.root);
        land.args(["land", &lower, &upper]);
        if point == "gate" {
            fs::write(&kill, "").unwrap();
        } else {
            land.env("ARC_TEST_LANDING_CRASH", point);
        }
        let crashed = land.output().unwrap();
        assert!(
            !crashed.status.success(),
            "{point}: {}",
            stderr_of(&crashed)
        );
        let _ = fs::remove_file(&kill);
        let attempt = prepared_attempt(&repo);
        for change in [&lower, &upper] {
            assert_eq!(
                status(&repo, change)["pending_landing"],
                attempt.as_str(),
                "{point}"
            );
            let integrate = repo
                .arc(&repo.root)
                .args(["integrate", change])
                .output()
                .unwrap();
            assert_eq!(
                integrate.status.code(),
                Some(20),
                "{point}: {}",
                stderr_of(&integrate)
            );
        }

        for run in 0..2 {
            let recovered = repo
                .arc(&repo.root)
                .args(["land", "--recover", &attempt])
                .output()
                .unwrap();
            assert!(
                recovered.status.success(),
                "{point} run {run}: {}",
                stderr_of(&recovered)
            );
        }
        let landed = matches!(point, "moved" | "applied");
        let target = git_out(&repo.root, &["rev-parse", "master"]);
        assert_eq!(
            &target,
            if landed { &upper_head } else { &before },
            "{point}"
        );
        for change in [&lower, &upper] {
            let report = status(&repo, change);
            assert!(report.get("pending_landing").is_none(), "{point}: {report}");
            if landed {
                assert_eq!(
                    report["closure"]["landing"]["attempt"],
                    attempt.as_str(),
                    "{point}"
                );
            } else {
                assert!(report["closure"].is_null(), "{point}: {report}");
            }
        }
        // The target checkout holds exactly what the target does.
        assert_eq!(
            git_out(
                &repo.root,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            "",
            "{point}"
        );
        assert_eq!(repo.root.join("upper.txt").exists(), landed, "{point}");
        assert_eq!(
            git_out(&repo.root, &["for-each-ref", "refs/arc/landing-applied/"]),
            "",
            "{point}"
        );
    }
}

/// Claim: a target someone else moved to the top head before arc's own ref
/// transaction is not a landing, and recovery closes nothing; only an
/// operator's abandonment releases the members. Rejects recovery that infers
/// authorization from where the target stands.
#[test]
fn a_target_moved_by_someone_else_is_never_published_as_the_landing() {
    let repo = gated_repo();
    let ((lower, _, _), (upper, _, upper_head)) = stack(&repo);
    let crashed = repo
        .arc(&repo.root)
        .env("ARC_TEST_LANDING_CRASH", "authorized")
        .args(["land", &lower, &upper])
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(86), "{}", stderr_of(&crashed));
    let attempt = prepared_attempt(&repo);
    git(
        &repo.root,
        &["update-ref", "refs/heads/master", &upper_head],
    );

    for _ in 0..2 {
        let recovered = repo
            .arc(&repo.root)
            .args(["land", "--recover", &attempt])
            .output()
            .unwrap();
        assert_eq!(
            recovered.status.code(),
            Some(1),
            "{}",
            stderr_of(&recovered)
        );
        assert!(
            stderr_of(&recovered).contains("did not authorize"),
            "{}",
            stderr_of(&recovered)
        );
    }
    assert!(!repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "landing-completed"));
    for change in [&lower, &upper] {
        let report = status(&repo, change);
        assert!(report["closure"].is_null(), "{report}");
        assert_eq!(report["pending_landing"], attempt.as_str());
    }

    let close = repo
        .arc(&repo.root)
        .args(["close", &lower, "--abandoned"])
        .output()
        .unwrap();
    assert!(!close.status.success());
    assert!(
        stderr_of(&close).contains(&format!("arc land --recover {attempt}")),
        "{}",
        stderr_of(&close)
    );

    repo.arc(&repo.root)
        .args([
            "land",
            "--abandon",
            &attempt,
            "--reason",
            "the target moved outside arc",
        ])
        .assert()
        .success();
    for change in [&lower, &upper] {
        let report = status(&repo, change);
        assert!(report["closure"].is_null(), "{report}");
        assert!(report.get("pending_landing").is_none(), "{report}");
    }
}

/// Claim: a commit-bound gate that passed at another commit holding the top
/// head's tree runs again at the top head, while a tree-bound gate's pass at
/// that tree is reused. Rejects reuse by tree alone, under which a history
/// the gate would refuse lands on another commit's pass.
#[test]
fn a_commit_bound_gate_passed_at_a_same_tree_commit_runs_again() {
    let repo = Repo::new();
    repo.redeclare_gates(
        "[gates.smoke]\ncommand = \"test -f README.md\"\n\n[gates.history]\ncommand = \"git log -1 --format=%s | grep -q good\"\nbinds = \"commit\"\n",
    );
    let (lower, _, lower_head) = reviewed_change(&repo, "lower", None);
    let upper = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "upper",
        "--base",
        &lower_head,
    ])));
    let upper_tree = repo.home.join(".worktrees/repo-upper");
    repo.commit(&upper_tree, "upper.txt", "upper\n", "feat: upper good");
    stdout(repo.arc(&upper_tree).args(["snapshot", "upper"]));
    repo.arc(&upper_tree)
        .args(["verify", "upper", "--all"])
        .assert()
        .success();
    git(
        &upper_tree,
        &["commit", "-q", "--amend", "-m", "feat: upper"],
    );
    stdout(repo.arc(&upper_tree).args(["snapshot", "upper"]));
    repo.arc(&upper_tree)
        .args(["review", "upper", "--verdict", "approved"])
        .assert()
        .success();
    let before = git_out(&repo.root, &["rev-parse", "master"]);

    let landed = repo
        .arc(&repo.root)
        .args(["land", &lower, &upper])
        .output()
        .unwrap();
    assert_eq!(landed.status.code(), Some(5), "{}", stderr_of(&landed));
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    let attempt = prepared_attempt(&repo);
    let events = change_events(&repo, &upper);
    let history: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["event_type"] == "verification-recorded"
                && event["gate"] == "history"
                && event["landing"] == attempt.as_str()
        })
        .collect();
    assert_eq!(history.len(), 1, "{events:?}");
    assert_eq!(history[0]["result"], "fail");
    assert!(events
        .iter()
        .any(|event| event["event_type"] == "verification-reused" && event["gate"] == "smoke"));
    assert!(!events
        .iter()
        .any(|event| event["event_type"] == "verification-recorded"
            && event["gate"] == "smoke"
            && event["landing"] == attempt.as_str()));
}

/// Claim: one member exported alone carries the completion that closed it,
/// and reads integrated via the landing where it is imported, with its own
/// range; an attempt still pending is not exported. Rejects an export that
/// leaves the member open on arrival, and one that ships a pending attempt
/// another store could never finish.
#[test]
fn an_exported_member_arrives_integrated_via_its_landing() {
    let repo = gated_repo();
    let ((lower, _, lower_head), (upper, _, _)) = stack(&repo);
    let before = git_out(&repo.root, &["rev-parse", "master"]);
    repo.arc(&repo.root)
        .args(["land", &lower, &upper])
        .assert()
        .success();
    let bundle = repo.home.join("lower.json");
    repo.arc(&repo.root)
        .args(["export", &lower, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let recipient = Repo::new();
    deliver_git_objects(&repo, &recipient);
    let imported = recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(imported.status.success(), "{}", stderr_of(&imported));
    let closure = json_stdout_any_status(recipient.arc(&recipient.root).args(["status", &lower]))
        ["closure"]
        .clone();
    assert_eq!(closure["outcome"], "integrated", "{closure}");
    assert!(closure["landing"]["attempt"].is_string(), "{closure}");
    assert_eq!(closure["range_base"], before.as_str());
    assert_eq!(closure["integrated_commit"], lower_head.as_str());

    // A pending attempt stays local.
    let other = gated_repo();
    let ((low, _, _), (up, _, _)) = stack(&other);
    other
        .arc(&other.root)
        .env("ARC_TEST_LANDING_CRASH", "prepared")
        .args(["land", &low, &up])
        .assert()
        .failure();
    let pending = other.home.join("pending.json");
    other
        .arc(&other.root)
        .args(["export", &low, "--output", pending.to_str().unwrap()])
        .assert()
        .success();
    let shipped: Value = serde_json::from_slice(&fs::read(&pending).unwrap()).unwrap();
    assert!(!shipped["repository_events"]
        .as_array()
        .is_some_and(|events| events
            .iter()
            .any(|event| event["event_type"] == "landing-prepared")));
}

/// Claim: a target whose history is squashed refuses a landing of more than
/// one commit, and a target that integrates with merge commits refuses the
/// fast-forward a stack landing makes. Rejects installing several commits
/// where one is required, or moving a merge-shaped target as a fast-forward.
#[test]
fn a_squash_or_merge_target_refuses_a_multi_commit_landing() {
    for (policy, needle) in [
        ("[integration]\nhistory = 'squash'\n", "squash"),
        ("[defaults.integration]\nshape = 'no-ff'\n", "merge commit"),
    ] {
        let repo = gated_repo();
        fs::create_dir_all(repo.root.join(".arc")).unwrap();
        fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
        git(&repo.root, &["add", ".arc/policy.toml"]);
        git(&repo.root, &["commit", "-q", "-m", "policy: history"]);
        let ((lower, _, _), (upper, _, _)) = stack(&repo);
        let before = git_out(&repo.root, &["rev-parse", "master"]);
        let refused = repo
            .arc(&repo.root)
            .args(["land", &lower, &upper])
            .output()
            .unwrap();
        assert!(!refused.status.success(), "{needle}");
        assert!(
            stderr_of(&refused).contains(needle),
            "{}",
            stderr_of(&refused)
        );
        assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    }
}

/// Claim: `--target` lands every member on the named branch, as `integrate
/// --into` does for one change, and their closures name it. Rejects a flag
/// that can only repeat the recorded target, and a landing that moves the
/// recorded target instead of the named one.
#[test]
fn target_lands_every_member_on_the_named_branch() {
    let repo = gated_repo();
    git(&repo.root, &["branch", "release", "master"]);
    let ((lower, _, _), (upper, _, upper_head)) = stack(&repo);
    let master = git_out(&repo.root, &["rev-parse", "master"]);
    repo.arc(&repo.root)
        .args(["land", &lower, &upper, "--target", "release"])
        .assert()
        .success();
    assert_eq!(git_out(&repo.root, &["rev-parse", "release"]), upper_head);
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), master);
    for change in [&lower, &upper] {
        assert_eq!(status(&repo, change)["closure"]["target_branch"], "release");
    }
}

/// The member's own events recorded before its landing completed: the last
/// one is a point `--at` reads the member still open at.
fn last_event_before_landing(repo: &Repo, change: &str) -> String {
    let completion = repository_events(repo)
        .into_iter()
        .find(|event| event["event_type"] == "landing-completed")
        .expect("a completed landing")["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    change_events(repo, change)
        .into_iter()
        .map(|event| event["event_id"].as_str().unwrap().to_string())
        .rfind(|id| *id < completion)
        .expect("events before the landing")
}

/// Claim: every reader of a change's closure sees a landed member as
/// integrated through its landing: `doctor` judges it as closed, a receipt
/// after integration binds to its landed commit, and `--at` a point before
/// the completion shows it open. Rejects readers that reduce the member's
/// own events alone, where `doctor` reports its removed branch as a missing
/// open branch and the receipt refuses the member as open, and an `--at`
/// replay that joins a completion recorded after the point it reads.
#[test]
fn a_landed_member_reads_integrated_to_every_reader_and_open_before_it() {
    let repo = gated_repo();
    let ((lower, lower_tree, lower_head), (upper, _, _)) = stack(&repo);
    repo.arc(&repo.root)
        .args(["land", &lower, &upper])
        .assert()
        .success();

    // `--at` an event before the completion: open, and not pending either.
    let before = last_event_before_landing(&repo, &lower);
    let then = json_stdout_any_status(
        repo.arc(&repo.root)
            .args(["status", &lower, "--at", &before]),
    );
    assert!(then["closure"].is_null(), "{then}");
    assert!(then["pending_landing"].is_null(), "{then}");
    let shown = json_stdout(
        repo.arc(&repo.root)
            .args(["show", &lower, "--at", &before, "--json"]),
    );
    assert!(
        shown["round_trip"]["local_integration_revision"].is_null(),
        "{shown}"
    );
    let shown = json_stdout(repo.arc(&repo.root).args(["show", &lower, "--json"]));
    assert_eq!(
        shown["round_trip"]["local_integration_revision"],
        lower_head.as_str(),
        "{shown}"
    );

    // `doctor` judges the member closed: its branch may go.
    let branch = status(&repo, &lower)["branch"]
        .as_str()
        .unwrap()
        .to_string();
    git(
        &repo.root,
        &[
            "worktree",
            "remove",
            "--force",
            lower_tree.to_str().unwrap(),
        ],
    );
    git(&repo.root, &["branch", "-D", &branch]);
    let doctor = json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]));
    assert!(
        !doctor["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "missing-open-branch"),
        "{doctor:#}"
    );

    // A receipt after integration binds to the member's landed commit.
    let receipt = repo
        .arc(&repo.root)
        .args([
            "verify",
            &lower,
            "--gate",
            "smoke",
            "--attest",
            "--after-integration",
            "--result",
            "pass",
            "--tested-revision",
            &lower_head,
            "--execution-host",
            "ci-host-1",
            "--runner",
            "watch-job-1",
        ])
        .output()
        .unwrap();
    assert!(receipt.status.success(), "{}", stderr_of(&receipt));
    assert!(
        String::from_utf8_lossy(&receipt.stdout)
            .contains(&format!("for integrated commit {lower_head}")),
        "{}",
        String::from_utf8_lossy(&receipt.stdout)
    );
}

/// Claim: a landed member's export carrying work recorded after its landing
/// imports, judged as following the closure its landing derived. Rejects an
/// import that reads the member's own events alone, where an audit of what
/// shipped reads as post-integration work before the change integrated.
#[test]
fn an_audited_landed_member_imports_with_its_audit() {
    let repo = gated_repo();
    let ((lower, _, _), (upper, _, _)) = stack(&repo);
    repo.arc(&repo.root)
        .args(["land", &lower, &upper])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "auditor")
        .args([
            "audit",
            &lower,
            "--verdict",
            "approved",
            "--body",
            "read it",
        ])
        .assert()
        .success();
    let bundle = repo.home.join("lower.json");
    repo.arc(&repo.root)
        .args(["export", &lower, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let recipient = Repo::new();
    deliver_git_objects(&repo, &recipient);
    let imported = recipient
        .arc(&recipient.root)
        .args(["import", bundle.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(imported.status.success(), "{}", stderr_of(&imported));
    let report = json_stdout_any_status(recipient.arc(&recipient.root).args(["status", &lower]));
    assert_eq!(report["closure"]["outcome"], "integrated", "{report}");
}

/// Claim: `replayed-unread` is derived by a landing and never declared.
/// Rejects a caller asserting the kind by hand, which would name a replay
/// arc never performed.
#[test]
fn replayed_unread_is_never_declared() {
    let repo = gated_repo();
    let (change, _, _) = reviewed_change(&repo, "solo", None);
    for args in [
        vec![
            "debt",
            change.as_str(),
            "--reason",
            "r",
            "--kind",
            "replayed-unread",
        ],
        vec![
            "integrate",
            change.as_str(),
            "--debt",
            "r",
            "--kind",
            "replayed-unread",
        ],
    ] {
        let refused = repo.arc(&repo.root).args(&args).output().unwrap();
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            stderr_of(&refused).contains("replayed-unread is derived, never declared"),
            "{args:?}: {}",
            stderr_of(&refused)
        );
    }
    assert!(status(&repo, &change)["debt"].is_null());
}

/// Two approved changes, each on the target as it is now, both touching
/// their own file.
fn independent(repo: &Repo) -> ((String, PathBuf, String), (String, PathBuf, String)) {
    let first = reviewed_change(repo, "first", None);
    let second = reviewed_change(repo, "second", None);
    (first, second)
}

fn verdict_of(repo: &Repo, change: &str) -> String {
    change_events(repo, change)
        .into_iter()
        .rev()
        .find(|event| event["event_type"] == "verdict-recorded")
        .expect("a verdict")["event_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Claim: members that do not stack land composed: each is replayed in
/// landing order onto the head the members before it left, and a member
/// whose exact patchset was approved on another base lands only under
/// `--debt`, carrying `replayed-unread` naming that approval, while a member
/// that stands where it lands carries none. Each member's closure is its own
/// range in the composed history. Rejects landing the replayed member on an
/// approval that never read its new base, debt on a member that was not
/// replayed, a rewrite map naming trial commits, and a member whose
/// integrated diff shows another member's work.
#[test]
fn independent_approved_members_land_composed_and_the_replayed_one_owes_its_context() {
    let repo = gated_repo();
    let ((first, _, first_head), (second, _, second_head)) = independent(&repo);
    let before = git_out(&repo.root, &["rev-parse", "master"]);

    let refused = repo
        .arc(&repo.root)
        .args(["land", &first, &second])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(3), "{}", stderr_of(&refused));
    assert!(
        stderr_of(&refused).contains(&second),
        "{}",
        stderr_of(&refused)
    );
    assert!(
        stderr_of(&refused).contains("replayed-unread"),
        "{}",
        stderr_of(&refused)
    );
    assert!(!stderr_of(&refused).contains(&format!("{first}'s")));
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    assert!(!repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "landing-prepared"));

    let plan = json_stdout(repo.arc(&repo.root).args([
        "land",
        &first,
        &second,
        "--debt",
        "context unread",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["shape"], "composed", "{plan:#}");
    assert_eq!(plan["members"][0]["replayed"], false);
    assert_eq!(plan["members"][1]["replayed"], true);
    assert!(plan["target_after"].is_null(), "{plan:#}");

    let landed = repo
        .arc(&repo.root)
        .args(["land", &first, &second, "--debt", "context unread"])
        .output()
        .unwrap();
    assert!(landed.status.success(), "{}", stderr_of(&landed));
    let after = git_out(&repo.root, &["rev-parse", "master"]);
    assert_ne!(after, second_head);
    assert_eq!(git_out(&repo.root, &["rev-parse", "master^"]), first_head);
    assert_eq!(git_out(&repo.root, &["rev-parse", "HEAD"]), after);

    let first_report = status(&repo, &first);
    assert_eq!(first_report["closure"]["outcome"], "integrated");
    assert_eq!(first_report["closure"]["range_base"], before.as_str());
    assert_eq!(
        first_report["closure"]["integrated_commit"],
        first_head.as_str()
    );
    assert!(first_report["debt"].is_null(), "{first_report}");

    let second_report = status(&repo, &second);
    let closure = &second_report["closure"];
    assert_eq!(closure["outcome"], "integrated", "{second_report}");
    assert_eq!(closure["target_before"], before.as_str());
    assert_eq!(closure["range_base"], first_head.as_str());
    assert_eq!(closure["integrated_commit"], after.as_str());
    assert_eq!(closure["source_head"], second_head.as_str());
    assert_eq!(
        closure["landing"]["attempt"],
        first_report["closure"]["landing"]["attempt"]
    );
    let debt = &second_report["debt"];
    assert_eq!(debt["missing"], "replayed-unread", "{second_report}");
    assert_eq!(
        debt["replayed"]["approval"],
        verdict_of(&repo, &second).as_str()
    );
    assert_eq!(debt["replayed"]["base"], before.as_str());
    assert_eq!(debt["replayed"]["parent"], first_head.as_str());
    assert_eq!(second_report["debt_standing"], "owed", "{second_report}");

    let completion = repository_events(&repo)
        .into_iter()
        .find(|event| event["event_type"] == "landing-completed")
        .unwrap();
    assert_eq!(
        completion["members"][1]["debt"]["replayed"]["approval"],
        verdict_of(&repo, &second).as_str()
    );
    assert!(completion["members"][0]["debt"].is_null());
    assert!(!repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "history-rewritten"));
    assert_eq!(
        git_out(&repo.root, &["for-each-ref", "refs/arc/landing"]),
        ""
    );
    let diff = stdout(
        repo.arc(&repo.root)
            .args(["diff", &second, "--integrated", "--stat"]),
    );
    assert!(
        diff.contains("second.txt") && !diff.contains("first.txt"),
        "{diff}"
    );
}

/// Claim: a `replayed-unread` debt is discharged by a clearing audit of the
/// revision that landed. Rejects an obligation an audit of what landed
/// leaves owed.
#[test]
fn an_audit_of_the_landed_revision_discharges_replayed_unread() {
    let repo = gated_repo();
    let ((first, _, _), (second, _, _)) = independent(&repo);
    repo.arc(&repo.root)
        .args(["land", &first, &second, "--debt", "context unread"])
        .assert()
        .success();
    assert_eq!(status(&repo, &second)["debt_standing"], "owed");
    let audited = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "auditor")
        .args([
            "audit",
            &second,
            "--verdict",
            "approved",
            "--body",
            "read it landed",
        ])
        .output()
        .unwrap();
    assert!(audited.status.success(), "{}", stderr_of(&audited));
    let report = status(&repo, &second);
    assert_eq!(report["debt_standing"], "discharged", "{report}");
    assert_eq!(
        report["debt"]["discharged_by_event_id"]
            .as_str()
            .map(|id| change_events(&repo, &second)
                .iter()
                .any(|event| event["event_id"] == id
                    && event["event_type"] == "audit-verdict-recorded")),
        Some(true),
        "{report}"
    );
}

/// Claim: a member that conflicts when replayed stops the attempt: nothing
/// closes, the target does not move, and the refusal names the member with
/// `arc rebase`. Rejects resolving a conflict inside a landing, and an
/// attempt that closes the members before the conflict.
#[test]
fn a_member_that_conflicts_on_replay_closes_nothing_and_names_the_rebase() {
    let repo = gated_repo();
    let mut changes = Vec::new();
    for slug in ["left", "right"] {
        let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.commit(
            &worktree,
            "shared.txt",
            &format!("{slug}\n"),
            &format!("feat: {slug}"),
        );
        stdout(repo.arc(&worktree).args(["snapshot", slug]));
        repo.arc(&worktree)
            .args(["review", slug, "--verdict", "approved"])
            .assert()
            .success();
        changes.push(change_id);
    }
    let before = git_out(&repo.root, &["rev-parse", "master"]);
    let refused = repo
        .arc(&repo.root)
        .args(["land", &changes[0], &changes[1], "--debt", "context unread"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(11), "{}", stderr_of(&refused));
    assert!(
        stderr_of(&refused).contains(&format!("arc rebase {}", changes[1])),
        "{}",
        stderr_of(&refused)
    );
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), before);
    for change in &changes {
        let report = status(&repo, change);
        assert!(report["closure"].is_null(), "{report}");
        assert!(report["pending_landing"].is_null(), "{report}");
    }
    assert!(repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "landing-abandoned"));
    assert_eq!(
        git_out(&repo.root, &["for-each-ref", "refs/arc/landing"]),
        ""
    );
}

/// Claim: a composed attempt whose gate fails at the composed head closes
/// nothing and leaves no trace in revision resolution: each member's
/// patchsets read as before, no rewrite map names a trial commit, and
/// nothing pins the trial head. Rejects publishing the replay's
/// correspondence as a rewrite, which would make every reader follow a
/// member's head to commits that never landed.
#[test]
fn a_failed_composed_attempt_leaves_no_trial_commit_in_revision_resolution() {
    let repo = Repo::new();
    repo.redeclare_gates(
        "[gates.alone]\ncommand = \"test ! -f first.txt || test ! -f second.txt\"\n",
    );
    let ((first, _, _), (second, _, _)) = independent(&repo);
    let shown = |change: &str| {
        json_stdout(repo.arc(&repo.root).args(["show", change, "--json"]))["patchsets"].clone()
    };
    let before = [shown(&first), shown(&second)];
    let master = git_out(&repo.root, &["rev-parse", "master"]);
    let failed = repo
        .arc(&repo.root)
        .args(["land", &first, &second, "--debt", "context unread"])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(5), "{}", stderr_of(&failed));
    let trial = String::from_utf8_lossy(&failed.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("composed: ").map(str::to_string))
        .expect("the attempt composed a head");
    assert_eq!(git_out(&repo.root, &["rev-parse", "master"]), master);
    assert_eq!([shown(&first), shown(&second)], before);
    for change in [&first, &second] {
        assert!(status(&repo, change)["closure"].is_null());
    }
    assert!(!repository_events(&repo)
        .iter()
        .any(|event| event["event_type"] == "history-rewritten"));
    assert_eq!(
        git_out(&repo.root, &["for-each-ref", "refs/arc/landing"]),
        ""
    );
    assert!(
        !git_out(&repo.root, &["for-each-ref", "--contains", &trial]).contains("refs/"),
        "a ref still holds the trial head {trial}"
    );
}

/// Claim: a composed attempt gates in a checkout it owns: a slot it leases
/// when the repository declares `[slots]`, released once the gates ran,
/// and otherwise a worktree of its own, removed afterwards. Rejects gating
/// in a member's checkout, which holds no composed head, and an attempt
/// that keeps its slot or leaves its worktree behind.
#[test]
fn a_composed_attempt_gates_in_a_checkout_it_owns_and_gives_it_back() {
    let repo = gated_repo();
    let ((first, _, _), (second, _, _)) = independent(&repo);
    repo.arc(&repo.root)
        .args(["land", &first, &second, "--debt", "context unread"])
        .assert()
        .success();
    let leftovers: Vec<String> = fs::read_dir(repo.home.join(".worktrees"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("landing-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let repo = gated_repo();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/worktrees.toml"),
        "[slots]\nsize = 1\nretain = [\"target\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", "-A"]);
    git(&repo.root, &["commit", "-m", "test: declare a slot pool"]);
    let ((first, _, _), (second, _, _)) = independent(&repo);
    let landed = repo
        .arc(&repo.root)
        .args(["land", &first, &second, "--debt", "context unread"])
        .output()
        .unwrap();
    assert!(landed.status.success(), "{}", stderr_of(&landed));
    let slot = repo.home.join(".worktrees/repo-slots/slot-01");
    assert!(
        String::from_utf8_lossy(&landed.stdout)
            .contains(&format!("slot: slot-01 at {}", slot.display())),
        "{}",
        String::from_utf8_lossy(&landed.stdout)
    );
    let run = change_events(&repo, &second)
        .into_iter()
        .rev()
        .find(|event| event["event_type"] == "verification-recorded")
        .unwrap();
    assert_eq!(
        run["revision"],
        git_out(&repo.root, &["rev-parse", "master"]).as_str()
    );
    let slots = json_stdout(repo.arc(&repo.root).args(["slots", "status", "--json"]));
    assert!(slots["slots"][0]["lease"].is_null(), "{slots:#}");
    assert_eq!(
        git_out(&slot, &["rev-parse", "HEAD"]),
        git_out(&repo.root, &["rev-parse", "master"])
    );
}
