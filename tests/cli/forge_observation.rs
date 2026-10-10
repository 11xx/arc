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
    let before = state(&repo, &id)["forge"]["forge_ready"].clone();
    let receipt = observe(&repo, &id, &document("r1", "g1", "first", None));
    assert_eq!(receipt["schema"], "arc-forge-receipt/1");
    assert_eq!(receipt["availability"][0]["availability"], "unmaterialized");
    let original = state(&repo, &id);
    assert_eq!(original["forge"]["forge_ready"], before);
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
