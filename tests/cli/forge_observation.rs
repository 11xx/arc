use super::common::*;
use serde_json::{json, Value};

fn setup() -> (Repo, String) {
    let repo = Repo::new();
    let id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "frontier",
        "--profile",
        "forge",
        "--target",
        "master",
    ])));
    stdout(repo.arc(&repo.root).args([
        "forge",
        "declare",
        &id,
        "--host",
        "forge.example.invalid",
        "--base-repo",
        "sample/project",
        "--head-repo",
        "sample/project",
        "--base-ref",
        "main",
        "--head-ref",
        "topic",
    ]));
    (repo, id)
}
fn document(read: &str, generation: &str, body: &str, predecessor: Option<&str>) -> Value {
    let mut v: Value =
        serde_json::from_str(include_str!("../fixtures/forge/frontier.json")).unwrap();
    let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
    v["read_id"] = json!(read);
    v["items"][0]["generation_id"] = json!(generation);
    v["items"][0]["sha256"] = json!(digest);
    v["items"][0]["byte_length"] = json!(body.len());
    v["items"][0]["content"]["sha256"] = json!(digest);
    v["items"][0]["content"]["byte_length"] = json!(body.len());
    v["generations"][0]["generation_id"] = json!(generation);
    v["generations"][0]["sha256"] = json!(digest);
    v["generations"][0]["predecessor"] = json!(predecessor);
    v
}
fn facts(v: &mut Value, kind: &str, value: Value) {
    v["items"][0]["kind"] = json!(kind);
    v["items"][0]["node_id"] = json!(format!("{kind}-1"));
    v["generations"][0]["kind"] = json!(kind);
    v["generations"][0]["node_id"] = json!(format!("{kind}-1"));
    v["items"][0]["facts"] = value;
}
fn observe(repo: &Repo, id: &str, v: &Value) -> Value {
    fs::write(repo.root.join("frontier.json"), v.to_string()).unwrap();
    serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "observe",
        id,
        "--file",
        "frontier.json",
        "--json",
    ])))
    .unwrap()
}
fn state(repo: &Repo, id: &str) -> Value {
    serde_json::from_str(&stdout(repo.arc(&repo.root).args(["show", id, "--json"]))).unwrap()
}
fn refuses(repo: &Repo, id: &str, v: &Value) {
    let before = event_count(repo, id);
    fs::write(repo.root.join("frontier.json"), v.to_string()).unwrap();
    repo.arc(&repo.root)
        .args(["forge", "observe", id, "--file", "frontier.json"])
        .assert()
        .failure();
    assert_eq!(event_count(repo, id), before);
}
#[test]
fn forge_amended_body_retains_both_generations() {
    let (repo, id) = setup();
    let first = document("read-1", "g1", "first", None);
    observe(&repo, &id, &first);
    observe(&repo, &id, &document("read-2", "g2", "second", Some("g1")));
    let st = state(&repo, &id);
    assert_eq!(st["forge"]["observations"].as_array().unwrap().len(), 2);
    assert_eq!(
        st["forge"]["observations"][0]["document"]["generations"][0]["sha256"],
        first["generations"][0]["sha256"]
    );
}
#[test]
fn forge_duplicate_read_is_idempotent_but_new_read_is_recorded() {
    let (repo, id) = setup();
    let first = document("read-1", "g1", "first", None);
    let receipt = observe(&repo, &id, &first);
    let before = event_count(&repo, &id);
    let retry = observe(&repo, &id, &first);
    assert_eq!(retry["outcome"], "already-recorded");
    assert_eq!(retry["event_ids"], receipt["event_ids"]);
    assert_eq!(event_count(&repo, &id), before);
    observe(&repo, &id, &document("read-2", "g1", "first", None));
    assert_eq!(event_count(&repo, &id), before + 1);
    refuses(&repo, &id, &document("read-1", "g2", "changed", Some("g1")));
    let mut invalid_schema = document("bad-schema", "g3", "changed", Some("g1"));
    invalid_schema["schema"] = json!("arc-forge-observation/99");
    refuses(&repo, &id, &invalid_schema);
    let mut bad = document("read-3", "g3", "changed", Some("g1"));
    bad["items"][0]["content"]["sha256"] = json!("invalid");
    refuses(&repo, &id, &bad);
}
#[test]
fn forge_generation_reversion_and_concurrent_predecessors_remain_visible() {
    let (repo, id) = setup();
    for (mut v, previous) in [
        (document("r1", "g1", "a", None), None),
        (document("r2", "g2", "b", Some("g1")), Some("r1")),
        (document("r3", "g3", "a", Some("g2")), Some("r2")),
        (document("r4", "g4", "c", Some("g1")), Some("r1")),
    ] {
        v["previous_observation"] = json!(previous);
        observe(&repo, &id, &v);
    }
    let st = state(&repo, &id);
    let observations = st["forge"]["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 4);
    let generations: Vec<_> = observations
        .iter()
        .map(|r| &r["document"]["generations"][0])
        .collect();
    assert_eq!(generations[0]["sha256"], generations[2]["sha256"]);
    assert_ne!(
        generations[0]["generation_id"],
        generations[2]["generation_id"]
    );
    assert_eq!(generations[2]["predecessor"], "g2");
    assert_eq!(generations[3]["predecessor"], "g1");
    let mut invalid = document("r5", "g1", "a", None);
    invalid["previous_observation"] = json!("r2");
    refuses(&repo, &id, &invalid);
}
#[test]
fn forge_partial_frontier_cannot_claim_absence_or_passed_zero_checks() {
    let (repo, id) = setup();
    let mut v = document("r1", "g1", "a", None);
    v["coverage"]["overall"]["state"] = json!("truncated");
    v["coverage"]["collections"]["comments"]["state"] = json!("truncated");
    v["coverage"]["collections"]["checks"]["state"] = json!("truncated");
    observe(&repo, &id, &v);
    let mut failed = document("failed", "g1", "a", None);
    failed["coverage"]["overall"]["state"] = json!("failed");
    failed["coverage"]["overall"]["failure_reason"] = json!("timeout");
    failed["observed_head_sha"] = Value::Null;
    failed["observed_base_sha"] = Value::Null;
    observe(&repo, &id, &failed);
    assert_eq!(
        state(&repo, &id)["forge"]["observations"][1]["document"]["observed_head_sha"],
        Value::Null
    );
    v = document("r2", "g2", "checks", None);
    v["coverage"]["overall"]["state"] = json!("truncated");
    v["coverage"]["collections"]["comments"]["state"] = json!("truncated");
    v["coverage"]["collections"]["checks"]["state"] = json!("truncated");
    facts(
        &mut v,
        "checks",
        json!({"kind":"checks", "state":"passed", "count":0}),
    );
    refuses(&repo, &id, &v);
    facts(
        &mut v,
        "absence",
        json!({"kind":"absence", "collection":"comments"}),
    );
    refuses(&repo, &id, &v);
}
#[test]
fn forge_checks_and_lifecycle_require_current_pr_and_raw_head() {
    let (repo, id) = setup();
    let mut v = document("r1", "g1", "a", None);
    facts(
        &mut v,
        "pull-request",
        json!({"kind":"pull-request", "state":"open", "merge_sha":null}),
    );
    let receipt = observe(&repo, &id, &v);
    let obs = receipt["observation_id"].as_str().unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", obs]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "pr-state", &id, "--observation", obs]),
    );
    let mut checks = document("r2", "g2", "checks", None);
    facts(
        &mut checks,
        "checks",
        json!({"kind":"checks", "state":"passed", "count":1}),
    );
    let r = observe(&repo, &id, &checks);
    stdout(repo.arc(&repo.root).args([
        "forge",
        "checks",
        &id,
        "--observation",
        r["observation_id"].as_str().unwrap(),
    ]));
    let mut other = document("r3", "g3", "other", Some("g2"));
    other["observed_head_sha"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    facts(&mut other, "checks", checks["items"][0]["facts"].clone());
    let r = observe(&repo, &id, &other);
    repo.arc(&repo.root)
        .args([
            "forge",
            "checks",
            &id,
            "--observation",
            r["observation_id"].as_str().unwrap(),
        ])
        .assert()
        .failure();
    let mut wrong_pr = document("wrong-pr", "new-pr", "different PR", None);
    wrong_pr["provider_identity"]["pr_number"] = json!(2);
    wrong_pr["provider_identity"]["url"] =
        json!("https://forge.example.invalid/sample/project/pull/2");
    facts(
        &mut wrong_pr,
        "pull-request",
        json!({"kind":"pull-request", "state":"closed", "merge_sha":null}),
    );
    let r = observe(&repo, &id, &wrong_pr);
    let before = event_count(&repo, &id);
    repo.arc(&repo.root)
        .args([
            "forge",
            "pr-state",
            &id,
            "--observation",
            r["observation_id"].as_str().unwrap(),
        ])
        .assert()
        .failure();
    let mut wrong_head = document("wrong-head", "new-head", "different head", Some("g1"));
    wrong_head["observed_head_sha"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    facts(
        &mut wrong_head,
        "pull-request",
        json!({"kind":"pull-request", "state":"closed", "merge_sha":null}),
    );
    let r = observe(&repo, &id, &wrong_head);
    repo.arc(&repo.root)
        .args([
            "forge",
            "pr-state",
            &id,
            "--observation",
            r["observation_id"].as_str().unwrap(),
        ])
        .assert()
        .failure();
    assert_eq!(event_count(&repo, &id), before + 1);
    repo.arc(&repo.root)
        .args(["forge", "link", &id, "--observation", obs, "--pr", "999"])
        .assert()
        .failure();
}
#[test]
fn forge_commands_never_fetch_or_invoke_provider() {
    let (repo, id) = setup();
    let bin = repo.home.join("bin");
    fs::create_dir(&bin).unwrap();
    let real_git = stdout(AssertCommand::new("sh").args(["-c", "command -v git"]))
        .trim()
        .to_string();
    for name in ["git", "gh", "curl"] {
        let script = if name == "git" {
            format!("#!/bin/sh\n[ \"$GIT_NO_LAZY_FETCH\" = 1 ] || exit 88\nfor arg do [ \"$arg\" != fetch ] || exit 89; done\nexec '{real_git}' \"$@\"\n")
        } else {
            "#!/bin/sh\nexit 90\n".into()
        };
        fs::write(bin.join(name), script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let run = |args: &[&str]| {
        repo.arc(&repo.root)
            .env("PATH", &path)
            .args(args)
            .assert()
            .success();
    };
    let mut v = document("r1", "g1", "a", None);
    facts(
        &mut v,
        "pull-request",
        json!({"kind":"pull-request", "state":"open", "merge_sha":null}),
    );
    fs::write(repo.root.join("frontier.json"), v.to_string()).unwrap();
    run(&["forge", "observe", &id, "--file", "frontier.json"]);
    run(&[
        "forge",
        "declare",
        &id,
        "--host",
        "forge.example.invalid",
        "--base-repo",
        "sample/project",
        "--head-repo",
        "sample/project",
        "--base-ref",
        "main",
        "--head-ref",
        "topic",
    ]);
    run(&["forge", "link", &id, "--observation", "r1"]);
    run(&["forge", "pr-state", &id, "--observation", "r1"]);
    let mut v = document("r2", "g2", "b", None);
    facts(
        &mut v,
        "checks",
        json!({"kind":"checks", "state":"passed", "count":1}),
    );
    fs::write(repo.root.join("frontier.json"), v.to_string()).unwrap();
    run(&["forge", "observe", &id, "--file", "frontier.json"]);
    run(&["forge", "checks", &id, "--observation", "r2"]);
}

#[test]
fn forge_observations_round_trip_unchanged_and_do_not_change_readiness() {
    let (repo, id) = setup();
    let before = status(&repo, &id)["forge"]["forge_ready"].clone();
    assert!(before.is_boolean(), "readiness must be a Boolean: {before}");
    let receipt = observe(&repo, &id, &document("r1", "g1", "first", None));
    assert_eq!(receipt["schema"], "arc-forge-receipt/1");
    assert_eq!(receipt["availability"][0]["availability"], "unmaterialized");
    let original = state(&repo, &id);
    let after = status(&repo, &id)["forge"]["forge_ready"].clone();
    assert!(after.is_boolean());
    assert_eq!(after, before);
    let bundle = repo.home.join("forge.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    let destination = Repo::new();
    deliver_git_objects(&repo, &destination);
    stdout(
        destination
            .arc(&destination.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    assert_eq!(
        state(&destination, &id)["forge"]["observations"],
        original["forge"]["observations"]
    );
    let retry = observe(&destination, &id, &document("r1", "g1", "first", None));
    assert_eq!(retry["event_ids"], receipt["event_ids"]);
    assert_eq!(retry["outcome"], "already-recorded");
}

#[test]
fn forge_complete_collection_survives_an_incomplete_read() {
    let (repo, id) = setup();
    let mut v = document("partial", "checks-g1", "checks", None);
    facts(
        &mut v,
        "checks",
        json!({"kind":"checks", "state":"passed", "count":1}),
    );
    v["coverage"]["overall"]["state"] = json!("truncated");
    v["coverage"]["collections"]["comments"]["state"] = json!("truncated");
    observe(&repo, &id, &v);
    v["read_id"] = json!("failed-comments");
    v["coverage"]["overall"]["state"] = json!("failed");
    v["coverage"]["overall"]["failure_reason"] = json!("comments unavailable");
    v["coverage"]["collections"]["comments"]["state"] = json!("failed");
    v["coverage"]["collections"]["comments"]["failure_reason"] = json!("timeout");
    observe(&repo, &id, &v);
    assert_eq!(
        state(&repo, &id)["forge"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

fn exchange_read(conflicting: bool) {
    let (source, id) = setup();
    let seed = source.home.join("seed.json");
    stdout(
        source
            .arc(&source.root)
            .args(["export", &id, "--output", seed.to_str().unwrap()]),
    );
    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", seed.to_str().unwrap()]),
    );
    let mut first = document("shared-read", "shared-generation", "first", None);
    first["observed_head_sha"] = json!(source.head(&source.root));
    first["observed_base_sha"] = first["observed_head_sha"].clone();
    let a = observe(&source, &id, &first);
    let mut second = if conflicting {
        document("shared-read", "shared-generation", "other", None)
    } else {
        first.clone()
    };
    second["observed_head_sha"] = first["observed_head_sha"].clone();
    second["observed_base_sha"] = first["observed_base_sha"].clone();
    let b = observe(&replica, &id, &second);
    stdout(replica.arc(&replica.root).args([
        "forge",
        "link",
        &id,
        "--observation",
        b["observation_id"].as_str().unwrap(),
    ]));
    let bundle = replica.home.join("replica.json");
    stdout(
        replica
            .arc(&replica.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    stdout(
        source
            .arc(&source.root)
            .args(["import", bundle.to_str().unwrap()]),
    );
    let shown = state(&source, &id);
    let observations = shown["forge"]["observations"].as_array().unwrap();
    if conflicting {
        assert_eq!(observations.len(), 2);
        assert!(
            observations.iter().all(|r| r["conflict"] == true),
            "{shown}"
        );
        assert_eq!(shown["forge"]["link"]["unresolved_binding"], true);
        let status: Value =
            serde_json::from_str(&stdout(source.arc(&source.root).args(["status", &id]))).unwrap();
        assert!(!status["forge"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty());
        source
            .arc(&source.root)
            .args([
                "forge",
                "link",
                &id,
                "--observation",
                a["observation_id"].as_str().unwrap(),
            ])
            .assert()
            .failure();
    } else {
        assert_eq!(observations.len(), 1);
        assert_eq!(shown["forge"]["link"]["unresolved_binding"], false);
        assert_eq!(observations[0]["aliases"][0], b["observation_id"]);
    }
    stdout(
        source
            .arc(&source.root)
            .args(["export", &id, "--output", seed.to_str().unwrap()]),
    );
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", seed.to_str().unwrap()]),
    );
    assert_eq!(
        state(&replica, &id)["forge"]["observations"],
        shown["forge"]["observations"]
    );
}

#[test]
fn forge_identical_replica_reads_coalesce_without_blocking_replay() {
    exchange_read(false);
}
#[test]
fn forge_conflicting_replica_reads_remain_readable_and_flagged() {
    exchange_read(true);
}
#[test]
fn forge_invalid_stored_observation_remains_readable_and_unresolved() {
    let (repo, id) = setup();
    let mut document = document("r1", "g1", "first", None);
    document["observed_head_sha"] = json!(repo.head(&repo.root));
    document["observed_base_sha"] = document["observed_head_sha"].clone();
    let r = observe(&repo, &id, &document);
    stdout(repo.arc(&repo.root).args([
        "forge",
        "link",
        &id,
        "--observation",
        r["observation_id"].as_str().unwrap(),
    ]));
    rewrite_event(&repo, &id, "forge-observation-recorded", |e| {
        e["observation"]["items"][0]["sha256"] = json!("bad-digest");
    });
    let shown = state(&repo, &id);
    assert!(
        shown["forge"]["observations"][0]["invalid"].is_string(),
        "{shown}"
    );
    assert_eq!(shown["forge"]["link"]["unresolved_binding"], true);
    let text = stdout(repo.arc(&repo.root).args(["show", &id]));
    assert!(text.contains("invalid"), "{text}");
    repo.arc(&repo.root)
        .args(["status", &id])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["forge", "link", &id, "--observation", "r1"])
        .assert()
        .failure();
    let bundle = repo.home.join("invalid.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    let receiving = Repo::new();
    deliver_git_objects(&repo, &receiving);
    receiving
        .arc(&receiving.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .failure();
    assert!(!event_dir(&receiving, &id).exists());
}

#[test]
fn forge_invalid_stored_projection_is_unresolved_without_poisoning_state() {
    let (repo, id) = setup();
    let r = observe(&repo, &id, &document("r1", "g1", "first", None));
    stdout(repo.arc(&repo.root).args([
        "forge",
        "link",
        &id,
        "--observation",
        r["observation_id"].as_str().unwrap(),
    ]));
    rewrite_event(&repo, &id, "forge-link", |e| {
        e["head_sha"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    });
    let shown = state(&repo, &id);
    assert_eq!(shown["forge"]["link"]["unresolved_binding"], true);
    assert_eq!(shown["forge"]["observations"][0]["invalid"], Value::Null);
    let status: Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &id]))).unwrap();
    assert_eq!(status["forge"]["observed_head_match"], false);
    assert_eq!(status["forge"]["forge_ready"], false);
    assert_eq!(
        status["forge"]["diagnostics"][0]["kind"],
        "projection-unresolved"
    );
}

fn status(repo: &Repo, id: &str) -> Value {
    serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", id, "--json"]))).unwrap()
}
fn native_identity(v: &mut Value, repo_id: Option<&str>, pr_id: Option<&str>) {
    v["provider_identity"]["repository_id"] = json!(repo_id);
    v["provider_identity"]["native_id"] = json!(pr_id);
}
fn ready_change() -> (Repo, String, String) {
    let (repo, id) = setup();
    let wt = repo.home.join(".worktrees/repo-frontier");
    repo.commit(&wt, "change.txt", "synthetic\n", "test: synthetic change");
    stdout(repo.arc(&wt).args(["snapshot", &id]));
    let head = repo.head(&wt);
    let mut pr = document("initial-pr", "pr-generation", "open PR", None);
    native_identity(&mut pr, Some("repository-a"), Some("native-pr-a"));
    pr["observed_head_sha"] = json!(head);
    facts(
        &mut pr,
        "pull-request",
        json!({"kind":"pull-request", "state":"open", "merge_sha":null}),
    );
    observe(&repo, &id, &pr);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", "initial-pr"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "pr-state", &id, "--observation", "initial-pr"]),
    );
    let mut checks = document("initial-checks", "check-generation", "passed checks", None);
    native_identity(&mut checks, Some("repository-a"), Some("native-pr-a"));
    checks["observed_head_sha"] = json!(head);
    facts(
        &mut checks,
        "checks",
        json!({"kind":"checks", "state":"passed", "count":1}),
    );
    observe(&repo, &id, &checks);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "checks", &id, "--observation", "initial-checks"]),
    );
    assert_eq!(status(&repo, &id)["forge"]["forge_ready"], true);
    (repo, id, head)
}
fn retarget_generation(edited: bool) {
    let (repo, id) = setup();
    let mut first = document("before-retarget", "g1", "first", None);
    native_identity(&mut first, Some("repository-a"), Some("native-pr-a"));
    observe(&repo, &id, &first);
    let mut next = if edited {
        document("after-retarget", "g2", "edited", Some("g1"))
    } else {
        document("after-retarget", "g1", "first", None)
    };
    native_identity(&mut next, Some("repository-a"), Some("native-pr-a"));
    next["provider_identity"]["base_ref"] = json!("release");
    next["previous_observation"] = json!("before-retarget");
    observe(&repo, &id, &next);
    let shown = state(&repo, &id);
    assert_eq!(
        shown["forge"]["observations"][0]["document"]["provider_identity"]["base_ref"],
        "main"
    );
    assert_eq!(
        shown["forge"]["observations"][1]["document"]["provider_identity"]["base_ref"],
        "release"
    );
    assert_eq!(shown["forge"]["observations"][1]["invalid"], Value::Null);
}
#[test]
fn forge_retargeted_native_pr_reuses_unchanged_generation() {
    retarget_generation(false);
}
#[test]
fn forge_retargeted_native_pr_edits_existing_generation() {
    retarget_generation(true);
}
#[test]
fn forge_retargeted_native_pr_cannot_fork_an_existing_item() {
    let (repo, id) = setup();
    let mut first = document("before-retarget", "g1", "first", None);
    native_identity(&mut first, Some("repository-a"), Some("native-pr-a"));
    observe(&repo, &id, &first);
    let mut fork = document("after-retarget", "g2", "edited", None);
    native_identity(&mut fork, Some("repository-a"), Some("native-pr-a"));
    fork["provider_identity"]["base_ref"] = json!("release");
    refuses(&repo, &id, &fork);
}
#[test]
fn forge_generation_identity_is_per_native_pr() {
    let (repo, id) = setup();
    let mut first = document("read-a", "g1", "first", None);
    native_identity(&mut first, Some("repository-a"), Some("native-pr-a"));
    observe(&repo, &id, &first);
    let mut reuse = document("reuse-b", "g1", "first", None);
    native_identity(&mut reuse, Some("repository-a"), Some("native-pr-b"));
    refuses(&repo, &id, &reuse);
    let mut extend = document("extend-b", "g2", "edited", Some("g1"));
    native_identity(&mut extend, Some("repository-a"), Some("native-pr-b"));
    refuses(&repo, &id, &extend);
    let mut own = document("own-b", "g3", "first", None);
    native_identity(&mut own, Some("repository-a"), Some("native-pr-b"));
    observe(&repo, &id, &own);
    let mut unreported = document("unreported", "g4", "first", None);
    native_identity(&mut unreported, None, None);
    observe(&repo, &id, &unreported);
}
fn assert_inherits_nothing(s: &Value) {
    let forge = &s["forge"];
    assert_eq!(forge["checks_applicable"], false, "{s}");
    assert_eq!(forge["checks"], "stale", "{s}");
    assert!(forge["pr_state"].is_null(), "{s}");
    assert!(
        forge["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().starts_with("pr-state unknown")),
        "{s}"
    );
    assert_eq!(forge["forge_ready"], false, "{s}");
}
#[test]
fn forge_replacement_native_pr_cannot_inherit_checks_or_lifecycle() {
    for (repository_id, native_id) in [
        (Some("repository-b"), Some("native-pr-b")),
        (Some("repository-a"), Some("native-pr-b")),
        (Some("repository-b"), Some("native-pr-a")),
        (Some("repository-a"), None),
        (None, None),
    ] {
        let (repo, id, head) = ready_change();
        let mut replacement = document(
            "replacement-pr",
            "replacement-generation",
            "replacement PR",
            None,
        );
        native_identity(&mut replacement, repository_id, native_id);
        replacement["observed_head_sha"] = json!(head);
        facts(
            &mut replacement,
            "pull-request",
            json!({"kind":"pull-request", "state":"open", "merge_sha":null}),
        );
        observe(&repo, &id, &replacement);
        stdout(repo.arc(&repo.root).args([
            "forge",
            "link",
            &id,
            "--observation",
            "replacement-pr",
        ]));
        assert_inherits_nothing(&status(&repo, &id));
        // Legacy unbound facts must not bridge the two native identities either.
        rewrite_event(&repo, &id, "forge-checks", |e| {
            for field in ["observation", "link_event_id", "binding_recorded"] {
                e.as_object_mut().unwrap().remove(field);
            }
        });
        rewrite_event(&repo, &id, "forge-pr-state", |e| {
            for field in ["observation", "link_event_id", "pr_head"] {
                e.as_object_mut().unwrap().remove(field);
            }
        });
        assert_inherits_nothing(&status(&repo, &id));
    }
}
#[test]
fn forge_admission_preserves_initially_ready_boolean_readiness() {
    let (repo, id, _) = ready_change();
    let before = status(&repo, &id)["forge"]["forge_ready"]
        .as_bool()
        .expect("Boolean readiness");
    assert!(before);
    observe(
        &repo,
        &id,
        &document("unrelated-read", "comment-generation", "comment", None),
    );
    let after = status(&repo, &id)["forge"]["forge_ready"]
        .as_bool()
        .expect("Boolean readiness");
    assert_eq!(after, before);
}
