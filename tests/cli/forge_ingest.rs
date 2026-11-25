//! Offline provider payload ingest (`arc forge ingest`) and schema discovery
//! (`arc forge observe --example`).

use super::common::*;
use serde_json::{json, Value};

fn setup_github_change() -> (Repo, String) {
    let repo = Repo::new();
    let id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "gh-pr",
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
        "github.com",
        "--base-repo",
        "owner/repo",
        "--head-repo",
        "owner/repo",
        "--base-ref",
        "main",
        "--head-ref",
        "feature",
    ]));
    (repo, id)
}

#[test]
fn forge_observe_example_emits_valid_admissible_manifest_without_a_repo() {
    let outside = tempfile::tempdir().unwrap();
    let output = Command::new(arc_binary())
        .current_dir(outside.path())
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .args(["forge", "observe", "--example"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let example: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(example["schema"], "arc-forge-observation/1");
    assert_eq!(example["items"].as_array().unwrap().len(), 3);

    let (repo, id) = setup_github_change();
    let path = repo.home.join("example.json");
    fs::write(&path, &output.stdout).unwrap();
    let receipt: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "observe",
        &id,
        "--file",
        path.to_str().unwrap(),
        "--json",
    ])))
    .unwrap();
    assert_eq!(receipt["schema"], "arc-forge-receipt/1");
    assert_eq!(receipt["outcome"], "recorded");
    assert_eq!(receipt["generation_ids"].as_array().unwrap().len(), 3);
}

#[test]
fn forge_ingest_converts_gh_payloads_across_multi_read_updates_with_custody() {
    let (repo, id) = setup_github_change();
    let head1 = "1111111111111111111111111111111111111111";
    let base1 = "2222222222222222222222222222222222222222";
    let head2 = "3333333333333333333333333333333333333333";
    let merge_sha = "4444444444444444444444444444444444444444";

    let read1 = json!({
        "number": 42,
        "id": "PR_kwDO_42",
        "url": "https://github.com/owner/repo/pull/42",
        "state": "OPEN",
        "title": "Add widget",
        "body": "First draft",
        "baseRefName": "main",
        "baseRefOid": base1,
        "headRefName": "feature",
        "headRefOid": head1,
        "headRepository": { "name": "repo", "nameWithOwner": "owner/repo" },
        "updatedAt": "2026-11-25T10:00:00Z",
        "comments": [
            { "id": "IC_100", "author": { "login": "alice" }, "body": "Fix null case" }
        ],
        "statusCheckRollup": [
            { "__typename": "CheckRun", "name": "tests", "status": "COMPLETED", "conclusion": "FAILURE" }
        ]
    });
    let r1_path = repo.home.join("read1.json");
    fs::write(&r1_path, serde_json::to_vec_pretty(&read1).unwrap()).unwrap();

    let receipt1: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "ingest",
        &id,
        "--file",
        r1_path.to_str().unwrap(),
        "--read-id",
        "gh-read-1",
        "--read-at",
        "2026-11-25T10:01:00Z",
        "--json",
    ])))
    .unwrap();
    assert_eq!(receipt1["outcome"], "recorded");
    assert!(receipt1["availability"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["availability"] == "available"));
    let obs1 = receipt1["observation_id"].as_str().unwrap().to_string();

    // Idempotent retry with the same read_id + read_at returns already-recorded.
    let retry: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "ingest",
        &id,
        "--file",
        r1_path.to_str().unwrap(),
        "--read-id",
        "gh-read-1",
        "--read-at",
        "2026-11-25T10:01:00Z",
        "--json",
    ])))
    .unwrap();
    assert_eq!(retry["outcome"], "already-recorded");
    assert_eq!(retry["observation_id"], obs1);

    // Reusing the same read_id with a different payload refuses as a conflict.
    let mut read1_conflict = read1.clone();
    read1_conflict["title"] = json!("Different title");
    let r1_conflict_path = repo.home.join("read1-conflict.json");
    fs::write(
        &r1_conflict_path,
        serde_json::to_vec_pretty(&read1_conflict).unwrap(),
    )
    .unwrap();
    let conflict_out = repo
        .arc(&repo.root)
        .args([
            "forge",
            "ingest",
            &id,
            "--file",
            r1_conflict_path.to_str().unwrap(),
            "--read-id",
            "gh-read-1",
            "--read-at",
            "2026-11-25T10:01:00Z",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let conflict_err = String::from_utf8_lossy(&conflict_out);
    assert!(
        conflict_err.contains("read_id conflicts with recorded observation"),
        "{conflict_err}"
    );

    // Projections bind cleanly to the ingested observation.
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "link", &id, "--observation", &obs1]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "checks", &id, "--observation", &obs1]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "pr-state", &id, "--observation", &obs1]),
    );

    let view1: Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["forge", "show", &id, "--json"]),
    ))
    .unwrap();
    let comment_gen1 = view1["observations"][0]["document"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["node_id"] == "IC_100")
        .unwrap()["generation_id"]
        .as_str()
        .unwrap()
        .to_string();
    let checks_gen1 = view1["observations"][0]["document"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "checks")
        .unwrap()["generation_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Retained comment body is immediately readable via `--body`.
    let body_out =
        stdout(
            repo.arc(&repo.root)
                .args(["forge", "show", &id, "--body", &comment_gen1]),
        );
    assert!(body_out.contains("Fix null case"), "{body_out}");

    // Read 2 via stdin: unchanged comment, green check, merged PR.
    let read2 = json!({
        "number": 42,
        "id": "PR_kwDO_42",
        "url": "https://github.com/owner/repo/pull/42",
        "state": "MERGED",
        "title": "Add widget",
        "body": "First draft",
        "baseRefName": "main",
        "baseRefOid": base1,
        "headRefName": "feature",
        "headRefOid": head2,
        "headRepository": { "name": "repo", "nameWithOwner": "owner/repo" },
        "mergeCommit": { "oid": merge_sha },
        "updatedAt": "2026-11-25T11:00:00Z",
        "comments": [
            { "id": "IC_100", "author": { "login": "alice" }, "body": "Fix null case" }
        ],
        "statusCheckRollup": [
            { "__typename": "CheckRun", "name": "tests", "status": "COMPLETED", "conclusion": "SUCCESS" }
        ]
    });
    let receipt2: Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["forge", "ingest", &id, "--file", "-", "--json"])
            .write_stdin(serde_json::to_string(&read2).unwrap()),
    ))
    .unwrap();
    assert_eq!(receipt2["outcome"], "recorded");

    let view2: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "show",
        &id,
        "--history",
        "--json",
    ])))
    .unwrap();
    let obs_list = view2["observations"].as_array().unwrap();
    assert_eq!(obs_list.len(), 2);
    let doc2 = &obs_list[1]["document"];
    assert_eq!(doc2["previous_observation"], obs1);
    let comment_gen2 = doc2["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["node_id"] == "IC_100")
        .unwrap()["generation_id"]
        .as_str()
        .unwrap();
    assert_eq!(comment_gen2, comment_gen1);

    let checks_gen2 = doc2["generations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["kind"] == "checks")
        .unwrap();
    assert_ne!(checks_gen2["generation_id"], checks_gen1);
    assert_eq!(checks_gen2["predecessor"], checks_gen1);
}

#[test]
fn forge_ingest_dry_run_writes_manifest_and_custody_dir_for_mcp_and_rest() {
    let (repo, id) = setup_github_change();
    let before = event_count(&repo, &id);

    let mcp_payload = json!({
        "number": 42,
        "url": "https://github.com/owner/repo/pull/42",
        "state": "open",
        "title": "MCP PR",
        "body": "From pull_request_read",
        "baseBranch": "main",
        "headBranch": "feature",
        "headSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "checks": { "total": 2, "passed": 2, "failed": 0, "pending": 0 },
        "threads": [
            {
                "id": "PRRT_1",
                "path": "src/lib.rs",
                "line": 12,
                "isResolved": false,
                "comments": [
                    { "id": "PRRC_1", "author": "reviewer", "body": "Handle empty input" }
                ]
            }
        ]
    });
    let mcp_path = repo.home.join("mcp.json");
    fs::write(&mcp_path, serde_json::to_vec_pretty(&mcp_payload).unwrap()).unwrap();
    let out_manifest = repo.home.join("built-manifest.json");
    let out_custody = repo.home.join("built-custody");

    stdout(repo.arc(&repo.root).args([
        "forge",
        "ingest",
        &id,
        "--file",
        mcp_path.to_str().unwrap(),
        "--format",
        "mcp",
        "--base-sha",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "--out-manifest",
        out_manifest.to_str().unwrap(),
        "--out-custody-dir",
        out_custody.to_str().unwrap(),
        "--dry-run",
    ]));
    // Dry-run records nothing on the ledger.
    assert_eq!(event_count(&repo, &id), before);
    assert!(out_manifest.exists());
    assert!(out_custody.join("sha256").is_dir());

    // The emitted manifest + custody-dir admit directly via `forge observe`.
    let receipt: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "observe",
        &id,
        "--file",
        out_manifest.to_str().unwrap(),
        "--custody-dir",
        out_custody.to_str().unwrap(),
        "--json",
    ])))
    .unwrap();
    assert_eq!(receipt["outcome"], "recorded");
    assert!(receipt["availability"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["availability"] == "available"));

    // GitHub REST payload + supplemental checks file with truncation.
    let rest_pr = json!({
        "number": 42,
        "html_url": "https://github.com/owner/repo/pull/42",
        "state": "open",
        "merged": false,
        "merge_commit_sha": "ffffffffffffffffffffffffffffffffffffffff",
        "title": "REST PR",
        "body": "Body",
        "base": { "ref": "main", "sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "repo": { "full_name": "owner/repo" } },
        "head": { "ref": "feature", "sha": "cccccccccccccccccccccccccccccccccccccccc", "repo": { "full_name": "owner/repo" } }
    });
    let rest_checks = json!({
        "total_count": 3,
        "check_runs": [
            { "name": "ci", "status": "completed", "conclusion": "success" }
        ]
    });
    let pr_path = repo.home.join("rest-pr.json");
    let checks_path = repo.home.join("rest-checks.json");
    fs::write(&pr_path, serde_json::to_vec(&rest_pr).unwrap()).unwrap();
    fs::write(&checks_path, serde_json::to_vec(&rest_checks).unwrap()).unwrap();
    let rest_receipt: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "ingest",
        &id,
        "--file",
        pr_path.to_str().unwrap(),
        "--checks-file",
        checks_path.to_str().unwrap(),
        "--json",
    ])))
    .unwrap();
    assert_eq!(rest_receipt["coverage"]["overall"]["state"], "truncated");
    assert_eq!(
        rest_receipt["coverage"]["collections"]["checks"]["state"],
        "truncated"
    );
}
