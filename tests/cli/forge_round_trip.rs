use super::common::*;
use super::forge_custody::{digest, materialize};
use super::forge_observation::{document, setup, state};
use serde_json::{json, Value};

fn retained(body: &str) -> Value {
    json!({"provider":"fixture", "handle":"body", "sha256":digest(body), "byte_length":body.len(), "media_type":"text/plain", "capture":"pinned"})
}
fn observe(
    repo: &Repo,
    id: &str,
    read: &str,
    generation: &str,
    body: &str,
    predecessor: Option<&str>,
) -> String {
    let dir = repo.home.join("materialized");
    materialize(&dir, &[body, "{}"]);
    fs::write(
        repo.root.join("frontier.json"),
        document(read, generation, body, predecessor).to_string(),
    )
    .unwrap();
    let receipt: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "observe",
        id,
        "--file",
        "frontier.json",
        "--custody-dir",
        dir.to_str().unwrap(),
        "--json",
    ])))
    .unwrap();
    receipt["event_ids"][0].as_str().unwrap().into()
}
fn source(id: &str, observation: &str, generation: &str) -> Value {
    json!({"change_id":id,"observation":observation,"generation":generation})
}
fn outcome(standing: &str) -> Value {
    json!({"standing":standing,"detail":"adapter observation", "evidence":[], "sources":[]})
}
fn delivery() -> Value {
    json!({"schema":"arc-forge-delivery/1", "message_id":"message-1", "attempt_id":"attempt-1", "evidence_id":"outcome-1", "run":"run-1", "transport_request":"request-1", "sources":[], "delivery":outcome("uncertain"), "acknowledgement":outcome("uncertain"), "acted_on":outcome("not-observed")})
}
fn send(repo: &Repo, id: &str, document: &Value) -> AssertCommand {
    fs::write(repo.root.join("delivery.json"), document.to_string()).unwrap();
    let mut cmd = repo.arc(&repo.root);
    cmd.args([
        "message",
        id,
        "--type",
        "note",
        "--summary",
        "Rework",
        "--delivery-file",
        "delivery.json",
    ]);
    cmd
}
#[test]
fn forge_delivery_retry_preserves_uncertain_acknowledgement() {
    let (repo, id) = setup();
    let first = delivery();
    stdout(&mut send(&repo, &id, &first));
    let before = event_count(&repo, &id);
    assert!(stdout(&mut send(&repo, &id, &first)).contains("already-recorded"));
    assert_eq!(event_count(&repo, &id), before);
    let mut conflict = first.clone();
    conflict["transport_request"] = json!("other-request");
    send(&repo, &id, &conflict).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    let mut later = first.clone();
    later["evidence_id"] = json!("outcome-2");
    later["previous_evidence"] = json!("outcome-1");
    later["delivery"]["standing"] = json!("observed");
    later["delivery"]["evidence"] = json!([retained("transport receipt")]);
    stdout(&mut send(&repo, &id, &later));
    let st = state(&repo, &id);
    assert_eq!(st["messages"].as_array().unwrap().len(), 2);
    for message in st["messages"].as_array().unwrap() {
        assert_eq!(
            message["delivery"]["acknowledgement"]["standing"],
            "uncertain"
        );
        assert_eq!(message["delivery"]["acted_on"]["standing"], "not-observed");
    }
    let assertion = repo
        .arc(&repo.root)
        .args(["check", &id, "--json"])
        .assert()
        .code(3);
    let checks = String::from_utf8_lossy(&assertion.get_output().stdout);
    assert!(!checks.contains("transport receipt"));
    assert!(stdout(repo.arc(&repo.root).args(["log", &id])).contains("acknowledgement Uncertain"));
}
fn author(model: Option<&str>, effort: Option<&str>, act: &str) -> Value {
    json!({"name":null, "harness":model.map(|_|"codex"), "model":model, "effort":effort,"act":act,"model_observation":model.map(|_|format!("observation-{act}"))})
}
fn comment(authors: Vec<Value>) -> Value {
    json!({"comment_id":"comment-1", "act_id":"review-act", "authors":authors, "source_frontiers":["finite-read-1"], "source_statements":["material review statement"], "renderer_version":"fixture/1", "roles":[{"role":"transport","participant":"other-model"},{"role":"implementation","participant":"implementation-model"},{"role":"quoted","participant":"quoted-model"}], "cost":null})
}
fn disclosure(
    id: &str,
    observation: &str,
    generation: &str,
    body: &str,
    comment: Option<Value>,
) -> Value {
    json!({"schema":"arc-disclosure/1", "disclosure_id":"disclosure-1", "selected":[{"source":source(id,observation,generation),"fields":["body"],"access_basis":"accessible-forge"}], "allowlist_version":"public/1", "validator_version":"bytes/1", "author":"review-author", "reviewer":"local-reviewer", "editor":null, "discloser":"publisher", "recipient_access_basis":"public forge reader", "omissions":["private execution trace withheld"], "outgoing":retained(body), "readback":source(id,observation,generation), "comment":comment})
}
fn declare(repo: &Repo, id: &str, document: &Value) -> AssertCommand {
    fs::write(repo.root.join("disclosure.json"), document.to_string()).unwrap();
    let mut cmd = repo.arc(&repo.root);
    cmd.args([
        "context",
        "declare",
        "--subject",
        id,
        "--cites",
        "--path",
        "public-comment",
        "--disclosure-file",
        "disclosure.json",
    ]);
    cmd
}
#[test]
fn forge_disclosure_binds_actual_outgoing_bytes_and_readback() {
    let (repo, id) = setup();
    let body = "Review\nAuthor: codex:review-model#high · Review cost: unmeasured.";
    let observation = observe(&repo, &id, "read-1", "g1", body, None);
    let doc = disclosure(
        &id,
        &observation,
        "g1",
        body,
        Some(comment(vec![author(
            Some("review-model"),
            Some("high"),
            "act-1",
        )])),
    );
    assert!(stdout(&mut declare(&repo, &id, &doc))
        .contains("Author: codex:review-model#high · Review cost: unmeasured."));
    let st = state(&repo, &id);
    assert_eq!(
        st["round_trip"]["disclosures"][0]["outgoing"]["sha256"],
        digest(body)
    );
    let before = event_count(&repo, &id);
    let mut mismatch = doc.clone();
    mismatch["disclosure_id"] = json!("bad");
    mismatch["outgoing"] = retained("different bytes");
    declare(&repo, &id, &mismatch).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    fs::remove_file(repo.root.join(".git/arc/custody/sha256").join(digest(body))).unwrap();
    let mut missing = doc.clone();
    missing["disclosure_id"] = json!("missing");
    declare(&repo, &id, &missing).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    assert_eq!(
        state(&repo, &id)["round_trip"]["disclosures"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn compact_provenance_material_authors_roles_act_coordinates_and_edit_lineage() {
    let (repo, id) = setup();
    let body =
        "Synthesis\nAuthors: codex:first-model#high, codex:second-model · Review cost: unmeasured.";
    let obs = observe(&repo, &id, "read-1", "g1", body, None);
    let authors = vec![
        author(Some("first-model"), Some("high"), "act-1"),
        author(Some("second-model"), None, "act-2"),
    ];
    let doc = disclosure(&id, &obs, "g1", body, Some(comment(authors.clone())));
    let output = stdout(&mut declare(&repo, &id, &doc));
    assert!(output.contains(
        "Authors: codex:first-model#high, codex:second-model · Review cost: unmeasured."
    ));
    for excluded in ["other-model", "implementation-model", "quoted-model", "$0"] {
        assert!(!output.contains(excluded));
    }
    let edited_body = "Correction\nAuthors: codex:first-model#high, codex:second-model · Review cost: unmeasured.";
    let edited_obs = observe(&repo, &id, "read-2", "g2", edited_body, Some("g1"));
    let mut edited = disclosure(&id, &edited_obs, "g2", edited_body, Some(comment(authors)));
    edited["disclosure_id"] = json!("disclosure-2");
    edited["previous_disclosure"] = json!("disclosure-1");
    edited["editor"] = json!("correction-editor");
    stdout(&mut declare(&repo, &id, &edited));
    let before = event_count(&repo, &id);
    edited["disclosure_id"] = json!("bad-edit");
    edited["comment"]["authors"] = json!([author(Some("publisher-model"), None, "act-3")]);
    declare(&repo, &id, &edited).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    let st = state(&repo, &id);
    assert_eq!(
        st["round_trip"]["disclosures"][1]["editor"],
        "correction-editor"
    );
    assert_eq!(
        st["round_trip"]["disclosures"][0]["comment"]["authors"][0]["model"],
        "first-model"
    );
    assert_eq!(
        st["round_trip"]["disclosures"][1]["comment"]["authors"][1]["effort"],
        Value::Null
    );
}
#[test]
fn compact_provenance_absent_frontier_cost_and_model_degrade_independently() {
    let (repo, id) = setup();
    let body = "Review\nAuthor: model not recorded · Review cost: unmeasured.";
    let obs = observe(&repo, &id, "read-1", "g1", body, None);
    let mut provenance = comment(vec![author(None, None, "act-1")]);
    provenance["source_frontiers"] = json!([]);
    let mut doc = disclosure(&id, &obs, "g1", body, Some(provenance));
    assert!(stdout(&mut declare(&repo, &id, &doc))
        .contains("Author: model not recorded · Review cost: unmeasured."));
    let before = event_count(&repo, &id);
    doc["disclosure_id"] = json!("implementation-cost");
    doc["comment"]["cost"] = json!({"reference":"opaque-cost","as_of":"cost-observation-1","scope":"implementation","basis":"list-estimate","coverage":"partial","source_frontier":["usage-1"]});
    declare(&repo, &id, &doc).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    doc["disclosure_id"] = json!("scoped-cost");
    doc["comment"]["cost"]["scope"] = json!("review-exchange");
    assert!(stdout(&mut declare(&repo, &id, &doc)).contains("Review cost: unmeasured."));
    assert_eq!(
        state(&repo, &id)["round_trip"]["disclosures"][1]["comment"]["cost"]["reference"],
        "opaque-cost"
    );
}
#[test]
fn forge_round_trip_related_change_and_custody_closure_imports_offline() {
    let (repo, origin) = setup();
    let obs = observe(&repo, &origin, "read-1", "g1", "public bytes", None);
    let destination = opened_change_id(&stdout(
        repo.arc(&repo.root)
            .args(["begin", "prepared", "--target", "master"]),
    ));
    stdout(repo.arc(&repo.root).args([
        "context",
        "declare",
        "--subject",
        &destination,
        "--relies-on",
        "--path",
        "prepared-source",
        "--forge-origin",
        &format!("{origin}:{obs}:g1"),
    ]));
    let disclosure = disclosure(&origin, &obs, "g1", "public bytes", None);
    stdout(&mut declare(&repo, &destination, &disclosure));
    let exported = repo.home.join("round-trip.json");
    let sidecar = repo.home.join("sidecar");
    stdout(repo.arc(&repo.root).args([
        "export",
        &destination,
        "--output",
        exported.to_str().unwrap(),
        "--custody-dir",
        sidecar.to_str().unwrap(),
    ]));
    let bundle: Value = serde_json::from_slice(&fs::read(&exported).unwrap()).unwrap();
    assert_eq!(bundle["related_changes"][0]["change_id"], origin);
    let replica = Repo::new();
    deliver_git_objects(&repo, &replica);
    let mut malformed = bundle.clone();
    malformed["related_changes"][0]["event_count"] = json!(0);
    let bad = repo.home.join("bad-round-trip.json");
    fs::write(&bad, malformed.to_string()).unwrap();
    super::forge_custody::admitted_change_command(&replica, &bad)
        .assert()
        .failure();
    assert!(!replica.root.join(".git/arc/config.json").exists());
    stdout(
        super::forge_custody::admitted_change_command(&replica, &exported)
            .args(["--custody-dir", sidecar.to_str().unwrap()]),
    );
    assert_eq!(
        state(&replica, &origin)["forge"]["observations"],
        state(&repo, &origin)["forge"]["observations"]
    );
    assert_eq!(
        state(&replica, &destination)["round_trip"],
        state(&repo, &destination)["round_trip"]
    );
    assert_eq!(
        fs::read(
            replica
                .root
                .join(".git/arc/custody/sha256")
                .join(digest("public bytes"))
        )
        .unwrap(),
        b"public bytes"
    );
}
#[test]
fn local_integration_and_closed_unmerged_pr_remain_distinct() {
    let (repo, id) = setup();
    let head = git_out(&repo.root, &["rev-parse", "HEAD"]);
    stdout(repo.arc(&repo.root).args([
        "forge",
        "link",
        &id,
        "--pr",
        "1",
        "--url",
        "https://forge.example.invalid/sample/project/pull/1",
        "--head-sha",
        &head,
        "--head-repo",
        "sample/project",
        "--head-ref",
        "topic",
        "--base-repo",
        "sample/project",
        "--base-ref",
        "main",
    ]));
    stdout(
        repo.arc(&repo.root)
            .args(["forge", "pr-state", &id, "--state", "closed"]),
    );
    let worktree = repo.home.join(".worktrees/repo-frontier");
    repo.commit(
        &worktree,
        "prepared.txt",
        "local preparation",
        "feat: prepare locally",
    );
    let local_head = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", &id]));
    git(&repo.root, &["merge", "--ff-only", &local_head]);
    stdout(
        repo.arc(&repo.root)
            .args(["close", &id, "--assert-integrated", &local_head]),
    );
    let st = state(&repo, &id);
    assert_eq!(st["round_trip"]["local_integration_revision"], local_head);
    assert_eq!(st["forge"]["link"]["observed_head_sha"], head);
    assert_eq!(st["round_trip"]["provider_lifecycle"], "closed-unmerged");
    assert_eq!(st["round_trip"]["observed_merge_sha"], Value::Null);
    assert!(st["verdicts"].as_array().unwrap().is_empty());
}

#[test]
fn disclosure_labels_git_and_withheld_ledger_support_and_retains_commit() {
    let (repo, id) = setup();
    let observation = observe(&repo, &id, "read-1", "g1", "public bytes", None);
    let mut doc = disclosure(&id, &observation, "g1", "public bytes", None);
    let head = repo.head(&repo.root);
    doc["selected"] = json!([
        {"source":{"change_id":id,"commit":head},"fields":["tree"],"access_basis":"accessible-git"},
        {"source":{"change_id":id,"event":observation},"fields":["private source claim"],"access_basis":"withheld-private-assertion"}
    ]);
    stdout(&mut declare(&repo, &id, &doc));
    let st = state(&repo, &id);
    assert_eq!(
        st["round_trip"]["disclosures"][0]["selected"][1]["access_basis"],
        "withheld-private-assertion"
    );
    assert!(git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/arc/retained/"
        ]
    )
    .contains(&head));
    let before = event_count(&repo, &id);
    doc["disclosure_id"] = json!("missing-git");
    doc["selected"][0]["source"]["commit"] = json!("b".repeat(40));
    declare(&repo, &id, &doc).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
}
