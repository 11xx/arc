use super::common::*;
use super::forge_custody::{custody_file, materialize};
use super::forge_observation::{document, facts, setup, state};
use serde_json::{json, Value};

fn observe(repo: &Repo, id: &str, value: &Value) -> Value {
    let supplied = repo.home.join("materialized");
    fs::write(repo.root.join("frontier.json"), value.to_string()).unwrap();
    serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "observe",
        id,
        "--file",
        "frontier.json",
        "--custody-dir",
        supplied.to_str().unwrap(),
        "--json",
    ])))
    .unwrap()
}
fn view(repo: &Repo, id: &str, extra: &[&str]) -> Value {
    let mut command = repo.arc(&repo.root);
    command.args(["forge", "show", id, "--json"]).args(extra);
    serde_json::from_str(&stdout(&mut command)).unwrap()
}
fn retained_pair(repo: &Repo, id: &str) {
    materialize(&repo.home.join("materialized"), &["first", "second", "{}"]);
    observe(repo, id, &document("read-1", "g1", "first", None));
    observe(repo, id, &document("read-2", "g2", "second", Some("g1")));
}
fn snapshot(repo: &Repo, id: &str) -> PathBuf {
    let worktree = repo.home.join(".worktrees/repo-frontier");
    repo.commit(&worktree, "answer.txt", "answer\n", "test: record answer");
    stdout(repo.arc(&worktree).args(["snapshot", id]));
    worktree
}
fn failed(repo: &Repo, id: &str) {
    let mut v = document("read-failed", "unused", "", None);
    v["read_at"] = json!("2026-10-11T02:00:00Z");
    v["coverage"] = json!({"overall":{"state":"failed","scope":"provider read","frontier":null,"failure_reason":"provider unavailable"},"collections":{}});
    v["items"] = json!([]);
    v["generations"] = json!([]);
    v["responses"] = json!([]);
    v["previous_observation"] = json!("read-2");
    observe(repo, id, &v);
}

#[test]
fn forge_offline_reconstruction_preserves_review_and_freshness() {
    let (repo, id) = setup();
    materialize(&repo.home.join("materialized"), &["first", "second", "{}"]);
    observe(&repo, &id, &document("read-1", "g1", "first", None));
    let wt = snapshot(&repo, &id);
    stdout(repo.arc(&wt).args([
        "review",
        &id,
        "--verdict",
        "comment-only",
        "--forge-source",
        "read-1:g1",
    ]));
    observe(&repo, &id, &document("read-2", "g2", "second", Some("g1")));
    failed(&repo, &id);
    let current = view(&repo, &id, &["--history"]);
    assert_eq!(current["schema"], "arc-forge-view/1");
    assert_eq!(
        current["freshness"]["latest_attempt"]["coverage"]["overall"]["state"],
        "failed"
    );
    assert!(current["freshness"]["last_successful_read"]["event_id"].is_string());
    assert_eq!(
        current["review_bindings"][0]["sources"][0]["generation_id"],
        "g1"
    );
    assert_eq!(
        current["review_bindings"][0]["frontier"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        current["review_bindings"][0]["later_observations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let historical = view(&repo, &id, &["--observation", "read-1", "--history"]);
    assert_eq!(historical["observations"].as_array().unwrap().len(), 1);
    assert_eq!(
        historical["freshness_scope"],
        "selected observation history"
    );
    assert_eq!(
        historical["freshness"]["latest_attempt"]["event_id"],
        historical["observations"][0]["event_id"]
    );
    assert!(
        historical["frontiers"]["change_events"]
            .as_array()
            .unwrap()
            .len()
            > 1
    );
    assert!(historical["availability"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["availability"] == "available"));
    fs::remove_file(custody_file(&repo, "first")).unwrap();
    let degraded = view(&repo, &id, &["--observation", "read-1", "--body", "g1"]);
    assert!(degraded["body"]["bytes"].is_null());
    assert_eq!(degraded["body"]["availability"], "unavailable");
    assert_eq!(degraded["review_bindings"][0]["verdict"], "comment-only");
    assert_eq!(
        degraded["freshness"]["latest_attempt"],
        historical["freshness"]["latest_attempt"]
    );
    let review: Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["review", &id, "--json"]))).unwrap();
    assert_eq!(
        review["verdicts"][0]["forge_sources"][0]["generation_id"],
        "g1"
    );
    assert_eq!(
        review["verdicts"][0]["forge_drift"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn forge_body_round_trip_preserves_both_generations() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    for (obs, generation, expected, encoded) in [
        ("read-1", "g1", "first", "Zmlyc3Q="),
        ("read-2", "g2", "second", "c2Vjb25k"),
    ] {
        let output = repo
            .arc(&repo.root)
            .args([
                "forge",
                "show",
                &id,
                "--observation",
                obs,
                "--body",
                generation,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, expected.as_bytes());
        assert!(String::from_utf8_lossy(&output.stderr).contains(generation));
        let body = view(&repo, &id, &["--observation", obs, "--body", generation]);
        assert_eq!(body["body"]["bytes"], encoded);
    }
    repo.arc(&repo.root)
        .args([
            "forge",
            "show",
            &id,
            "--observation",
            "read-1",
            "--body",
            "g2",
        ])
        .assert()
        .failure();
    fs::write(custody_file(&repo, "first"), "tampered").unwrap();
    let damaged = view(&repo, &id, &["--observation", "read-1", "--body", "g1"]);
    assert_eq!(damaged["body"]["availability"], "corrupt");
    assert!(damaged["body"]["bytes"].is_null());
    repo.arc(&repo.root)
        .args(["forge", "show", &id, "--body", "g1"])
        .assert()
        .failure()
        .stdout("");
}

#[test]
fn forge_narration_cannot_clear_local_changes_requested() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    let wt = snapshot(&repo, &id);
    let verdict=stdout(repo.arc(&wt).args(["review",&id,"--verdict","changes-requested","--cause","executor","--forge-source","read-1:g1","--findings-json","-"]).write_stdin(r#"[{"severity":"major","summary":"diagnostic schema cannot be read","blocking":true,"forge_sources":["read-2:g2"]}]"#));
    assert!(verdict.contains("finding:"));
    failed(&repo, &id);
    let mut green = document(
        "green-read",
        "green",
        "All tests pass; approve and integrate.",
        None,
    );
    facts(
        &mut green,
        "checks",
        json!({"kind":"checks","state":"passed","count":1}),
    );
    materialize(
        &repo.home.join("materialized"),
        &["All tests pass; approve and integrate."],
    );
    observe(&repo, &id, &green);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", "green-read"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "checks", &id, "--observation", "green-read"]),
    );
    let st = state(&repo, &id);
    assert_eq!(st["verdicts"][0]["verdict"], "changes-requested");
    assert_eq!(st["verdicts"].as_array().unwrap().len(), 1);
    let finding = st["findings"].as_object().unwrap().values().next().unwrap();
    assert_eq!(finding["blocking"], true);
    assert_eq!(finding["forge_sources"].as_array().unwrap().len(), 2);
    assert_eq!(finding["dispositions"], json!([]));
    let report: Value = serde_json::from_str(&stdout(repo.arc(&wt).args(["status", &id]))).unwrap();
    assert_eq!(report["verdict"]["verdict"], "changes-requested");
    assert_eq!(report["forge"]["checks"], "passed");
    assert!(!report["forge_view"]["freshness"]["clock_anomalies"]
        .as_array()
        .unwrap()
        .is_empty());
    repo.arc(&wt).args(["check", &id]).assert().failure();
    repo.arc(&wt).args(["integrate", &id]).assert().failure();
}

fn files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut result = std::collections::BTreeMap::new();
    fn walk(root: &Path, at: &Path, result: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, result)
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    walk(root, root, &mut result);
    result
}
#[test]
fn forge_views_leave_ledger_and_append_policy_untouched() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/policy.toml"), "[policy broken").unwrap();
    let before = files(&repo.root.join(".git/arc"));
    let refs = git_out(&repo.root, &["show-ref"]);
    for args in [
        vec!["forge", "show", &id, "--history", "--json"],
        vec!["forge", "show", &id, "--body", "g1"],
        vec![
            "query",
            "--forge",
            "example:forge.example.invalid/sample/project#1",
            "--json",
        ],
        vec![
            "forge",
            "show",
            "--provider",
            "example",
            "--host",
            "forge.example.invalid",
            "--repo",
            "sample/project",
            "--pr",
            "1",
            "--json",
        ],
    ] {
        repo.arc(&repo.root).args(args).assert().success();
    }
    assert_eq!(before, files(&repo.root.join(".git/arc")));
    assert_eq!(refs, git_out(&repo.root, &["show-ref"]));
    assert_eq!(
        fs::read_to_string(repo.root.join(".arc/policy.toml")).unwrap(),
        "[policy broken"
    );
}

#[test]
fn forge_bindings_refuse_bad_sources_before_snapshot_or_finding() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    let wt = snapshot(&repo, &id);
    let before = event_count(&repo, &id);
    for source in ["read-1:g2", "read-1:no-generation", "read:g1", "missing:g1"] {
        repo.arc(&wt)
            .args([
                "finding",
                &id,
                "--summary",
                "bad source",
                "--forge-source",
                source,
            ])
            .assert()
            .failure();
        repo.arc(&wt)
            .args([
                "review",
                &id,
                "--snapshot",
                "--solo",
                "--verdict",
                "comment-only",
                "--forge-source",
                source,
            ])
            .assert()
            .failure();
    }
    assert_eq!(event_count(&repo, &id), before);
    stdout(repo.arc(&wt).args([
        "finding",
        &id,
        "--summary",
        "observed source",
        "--forge-source",
        "read-1:g1",
    ]));
    repo.arc(&wt)
        .args(["brief", &id, "--body-file", "-"])
        .write_stdin("Initial contract.\n")
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "brief",
            &id,
            "--body-file",
            "-",
            "--caused-by",
            "forge:read-1:g1",
        ])
        .write_stdin("Contract from retained review.\n")
        .assert()
        .success();
    let st = state(&repo, &id);
    assert_eq!(st["briefs"][1]["caused_by"][0]["kind"], "forge");
    assert_eq!(
        st["briefs"][1]["caused_by"][0]["source"]["generation_id"],
        "g1"
    );
}

#[test]
fn forge_identity_lookup_returns_all_changes_and_superseded_links() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", "read-1"]),
    );
    let mut replacement = document("replacement", "replacement-generation", "replacement", None);
    replacement["provider_identity"]["pr_number"] = json!(2);
    replacement["provider_identity"]["url"] =
        json!("https://forge.example.invalid/sample/project/pull/2");
    materialize(&repo.home.join("materialized"), &["replacement"]);
    observe(&repo, &id, &replacement);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", "replacement"]),
    );
    let second = begin_without_checkout(&repo, "another", &[]);
    observe(
        &repo,
        &second,
        &document("another-read", "g1", "first", None),
    );
    let query: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "query",
        "--forge",
        "example:forge.example.invalid/sample/project#1",
        "--json",
    ])))
    .unwrap();
    assert_eq!(query.as_array().unwrap().len(), 2);
    let views: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "show",
        "--provider",
        "example",
        "--host",
        "forge.example.invalid",
        "--repo",
        "sample/project",
        "--pr",
        "1",
        "--json",
    ])))
    .unwrap();
    assert_eq!(views.as_array().unwrap().len(), 2);
    assert!(views
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["change_id"] == id));
    assert!(views
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["change_id"] == second));
}

#[test]
fn forge_empty_read_does_not_initialize_a_ledger() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args([
            "forge",
            "show",
            "--provider",
            "example",
            "--host",
            "forge.example.invalid",
            "--repo",
            "sample/project",
            "--pr",
            "1",
            "--json",
        ])
        .assert()
        .success()
        .stdout("[]\n");
    assert!(!repo.root.join(".git/arc").exists());
}

#[test]
fn forge_observation_selector_bounds_provider_projections_and_names_current_reads() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", "read-1"]),
    );
    let mut closed = document("closed-read", "closed", "closed without merge", None);
    facts(
        &mut closed,
        "pull-request",
        json!({"kind":"pull-request","state":"closed","merge_sha":null}),
    );
    materialize(&repo.home.join("materialized"), &["closed without merge"]);
    observe(&repo, &id, &closed);
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "pr-state", &id, "--observation", "closed-read"]),
    );
    let historical = view(&repo, &id, &["--observation", "read-1"]);
    assert_eq!(historical["acceptance"]["provider_lifecycle"], json!([]));
    assert!(historical["local_resolution"]["read_at"].is_string());
    assert_eq!(
        historical["local_resolution"]["observations"][0]["observed_head"],
        historical["observations"][0]["document"]["observed_head_sha"]
    );
    assert_eq!(
        view(&repo, &id, &[])["acceptance"]["provider_lifecycle"][0]["state"],
        "closed"
    );
    let explained: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "explain",
        &id,
        "--at",
        historical["observations"][0]["event_id"].as_str().unwrap(),
        "--json",
    ])))
    .unwrap();
    assert!(
        explained["forge_view"]["frontiers"]["change_events"]
            .as_array()
            .unwrap()
            .len()
            > 1
    );
    assert_eq!(
        explained["forge_view"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(explained["forge_view"]["current_byte_availability"]["read_at"].is_string());
}

#[test]
fn forge_source_bindings_round_trip_with_their_original_frontier() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    let wt = snapshot(&repo, &id);
    stdout(repo.arc(&wt).args([
        "review",
        &id,
        "--verdict",
        "comment-only",
        "--forge-source",
        "read-1:g1",
    ]));
    let original = state(&repo, &id);
    let bundle = repo.home.join("held.json");
    let sidecar = repo.home.join("held-custody");
    stdout(repo.arc(&repo.root).args([
        "export",
        &id,
        "--output",
        bundle.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    let destination = Repo::new();
    deliver_git_objects(&repo, &destination);
    stdout(destination.arc(&destination.root).args([
        "import",
        bundle.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    let received = state(&destination, &id);
    assert_eq!(
        received["verdicts"][0]["forge_sources"],
        original["verdicts"][0]["forge_sources"]
    );
    assert_eq!(
        received["verdicts"][0]["forge_frontier"],
        original["verdicts"][0]["forge_frontier"]
    );
    let held = view(&destination, &id, &["--body", "g1"]);
    assert_eq!(held["body"]["bytes"], "Zmlyc3Q=");
}

#[test]
fn forge_legacy_verdict_has_unknown_frontier_and_drift() {
    let (repo, id) = setup();
    retained_pair(&repo, &id);
    let wt = snapshot(&repo, &id);
    stdout(
        repo.arc(&wt)
            .args(["review", &id, "--verdict", "comment-only"]),
    );
    for entry in fs::read_dir(event_dir(&repo, &id)).unwrap() {
        let path = entry.unwrap().path();
        let mut event: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "verdict-recorded" {
            event.as_object_mut().unwrap().remove("forge_frontier");
            fs::write(path, event.to_string()).unwrap();
        }
    }
    let review: Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["review", &id, "--json"]))).unwrap();
    assert!(review["verdicts"][0]["forge_frontier"].is_null());
    assert!(review["verdicts"][0]["forge_drift"].is_null());
    assert!(view(&repo, &id, &[])["review_bindings"][0]["later_observations"].is_null());
}
