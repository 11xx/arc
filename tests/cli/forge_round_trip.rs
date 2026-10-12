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
    let view: Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["forge", "show", &id, "--json"]),
    ))
    .unwrap();
    assert_eq!(view["disclosures"][0]["outgoing"]["sha256"], digest(body));
    assert!(view["source_availability"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["availability"] == "unavailable" && r["source_status"] == "valid"));
    assert_eq!(view["byte_references"][0]["availability"], "unavailable");
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
    provenance
        .as_object_mut()
        .unwrap()
        .remove("source_frontiers");
    let mut doc = disclosure(&id, &obs, "g1", body, Some(provenance));
    assert!(stdout(&mut declare(&repo, &id, &doc))
        .contains("Author: model not recorded · Review cost: unmeasured."));
    let before = event_count(&repo, &id);
    doc["disclosure_id"] = json!("implementation-cost");
    doc["comment"]["cost"] = json!({"reference":"opaque-cost","as_of":"cost-observation-1","scope":"implementation","basis":"list-estimate","coverage":"partial","source_frontier":["usage-1"]});
    declare(&repo, &id, &doc).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    doc["disclosure_id"] = json!("scoped-cost");
    doc["previous_disclosure"] = json!("disclosure-1");
    doc["editor"] = json!("cost-annotator");
    doc["comment"]["cost"]["scope"] = json!("review-exchange");
    let line = stdout(&mut declare(&repo, &id, &doc));
    assert!(
        line.contains("Review cost: list-estimate as of cost-observation-1."),
        "{line}"
    );
    assert!(!line.contains("unmeasured"), "{line}");
    assert_eq!(
        state(&repo, &id)["round_trip"]["disclosures"][1]["comment"]["cost"]["reference"],
        "opaque-cost"
    );
    let shown = state(&repo, &id)["round_trip"]["disclosure_availability"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["disclosure_id"] == "scoped-cost")
        .unwrap()["compact_provenance"]
        .to_string();
    assert!(
        shown.contains("list-estimate as of cost-observation-1"),
        "{shown}"
    );
    assert!(!shown.contains("unmeasured"), "{shown}");
}
fn admitted_round_trip(repo: &Repo, path: &Path) -> AssertCommand {
    let bytes = fs::read(path).unwrap();
    let bundle: Value = serde_json::from_slice(&bytes).unwrap();
    let source = bundle["source_replica_id"].as_str().unwrap();
    repo.declare_gates_locally(&format!("[[replica.publishers]]\nprincipal='fixture-round-trip'\nsource_replica_id='{source}'\n[[replica.publishers.origins]]\nreplica_id='{source}'\nroles=['facts','rewrite','integration']\nactors=['tester']\n"));
    let mut command = repo.arc(&repo.root);
    command.args([
        "import",
        path.to_str().unwrap(),
        "--publisher",
        "fixture-round-trip",
        "--received-sha256",
        &format!("{:x}", Sha256::digest(&bytes)),
    ]);
    command
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
    let worktree = repo.home.join(".worktrees/repo-prepared");
    repo.commit(
        &worktree,
        "prepared.txt",
        "prepared content",
        "feat: prepare received content",
    );
    let integrated = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", &destination]));
    git(&repo.root, &["merge", "--ff-only", &integrated]);
    stdout(
        repo.arc(&repo.root)
            .args(["close", &destination, "--assert-integrated", &integrated]),
    );
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
    admitted_round_trip(&replica, &bad).assert().failure();
    assert!(!replica.root.join(".git/arc/config.json").exists());
    stdout(
        admitted_round_trip(&replica, &exported).args(["--custody-dir", sidecar.to_str().unwrap()]),
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
    let projected: Value = serde_json::from_str(&stdout(
        replica
            .arc(&replica.root)
            .args(["forge", "show", &origin, "--json"]),
    ))
    .unwrap();
    assert!(projected["related_changes"]
        .as_array()
        .unwrap()
        .contains(&json!(destination)));
    let accepted = projected["related_workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["change_id"] == destination)
        .unwrap();
    assert_eq!(
        accepted["round_trip"]["local_integration_revision"],
        integrated
    );
    let query = stdout(replica.arc(&replica.root).args([
        "query",
        "--forge",
        "example:forge.example.invalid/sample/project#1",
    ]));
    assert!(query.contains(&origin) && query.contains(&destination));
    let shown = state(&replica, &destination);
    assert_eq!(
        shown["forge_view"]["round_trip"]["local_integration_revision"],
        integrated
    );
    let status: Value = serde_json::from_str(&stdout(
        replica.arc(&replica.root).args(["status", &destination]),
    ))
    .unwrap();
    assert_eq!(
        status["forge_view"]["disclosures"][0]["schema"],
        "arc-disclosure/1"
    );
    for command in ["show", "log", "explain"] {
        let text = stdout(replica.arc(&replica.root).args([command, &destination]));
        assert!(
            text.contains("arc-disclosure/1") && text.contains("local_integration_revision"),
            "{command}: {text}"
        );
    }
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

fn git_support(repo: &Repo, id: &str, observation: &str, commit: &str) -> AssertCommand {
    let mut doc = disclosure(id, observation, "g1", "public bytes", None);
    doc["selected"] = json!([
        {"source":{"change_id":id,"commit":commit},"fields":["tree"],"access_basis":"accessible-git"}
    ]);
    declare(repo, id, &doc)
}

fn retained_refs(repo: &Repo) -> String {
    git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(objectname) %(objecttype)",
            "refs/arc/retained/",
        ],
    )
}

#[test]
fn disclosure_git_support_refuses_a_held_tag_blob_and_uppercase_commit_id() {
    let (repo, id) = setup();
    let head = repo.head(&repo.root);
    let observation = observe(&repo, &id, "read-1", "g1", "public bytes", None);
    git(
        &repo.root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "tag",
            "-a",
            "-m",
            "annotated",
            "v1",
        ],
    );
    let tag = git_out(&repo.root, &["rev-parse", "v1"]);
    assert_ne!(tag, head);
    let blob = git_out(&repo.root, &["rev-parse", "HEAD:README.md"]);
    let before = event_count(&repo, &id);
    for (label, oid) in [
        ("tag", tag),
        ("blob", blob),
        ("uppercase", head.to_uppercase()),
    ] {
        let failure = git_support(&repo, &id, &observation, &oid)
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&failure.get_output().stderr).to_string();
        assert!(!stderr.is_empty(), "{label}");
        assert_eq!(event_count(&repo, &id), before, "{label}");
        assert_eq!(retained_refs(&repo), "", "{label}");
    }
    stdout(&mut git_support(&repo, &id, &observation, &head));
    assert_eq!(retained_refs(&repo), format!("{head} commit"));
}

#[test]
fn recorded_git_support_that_is_a_held_tag_is_unavailable_unexported_and_unpinned() {
    let (repo, id) = setup();
    let head = repo.head(&repo.root);
    let observation = observe(&repo, &id, "read-1", "g1", "public bytes", None);
    stdout(&mut git_support(&repo, &id, &observation, &head));
    git(
        &repo.root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "tag",
            "-a",
            "-m",
            "annotated",
            "v1",
        ],
    );
    let tag = git_out(&repo.root, &["rev-parse", "v1"]);
    let control = repo.home.join("control-bundle.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", control.to_str().unwrap()]),
    );
    let control: Value = serde_json::from_slice(&fs::read(&control).unwrap()).unwrap();
    assert!(control["objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|object| { object["oid"] == json!(head) && object["role"] == "disclosure-commit" }));
    rewrite_event(&repo, &id, "context-declared", |event| {
        event["disclosure"]["selected"][0]["source"]["commit"] = json!(tag);
    });
    let shown: Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["forge", "show", &id, "--json"]),
    ))
    .unwrap();
    let availability: Vec<_> = shown["source_availability"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["reference"]["commit"] == json!(tag))
        .collect();
    assert_eq!(availability.len(), 1, "{shown}");
    assert_eq!(availability[0]["availability"], "unavailable");
    let bundle = repo.home.join("tag-bundle.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    let exported: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let held_as_support = |oid: &str| {
        exported["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|object| object["oid"] == json!(oid) && object["role"] == "disclosure-commit")
    };
    assert!(!held_as_support(&tag), "{exported}");
    assert!(!held_as_support(&head), "{exported}");
    let replica = Repo::new();
    deliver_git_objects(&repo, &replica);
    assert_eq!(git_out(&replica.root, &["cat-file", "-t", &tag]), "tag");
    stdout(&mut admitted_round_trip(&replica, &bundle));
    assert_eq!(retained_refs(&replica), "");
}

#[test]
fn disclosure_repository_ledger_source_exports_without_a_fake_related_change() {
    let (repo, id) = setup();
    let observation = observe(&repo, &id, "read-1", "g1", "public bytes", None);
    repo.arc(&repo.root)
        .args(["brief", &id, "--body-file", "-"])
        .write_stdin("Prepare a candidate.")
        .assert()
        .success();
    stdout(repo.arc(&repo.root).args([
        "candidate",
        "register",
        "--tree",
        "HEAD",
        "--brief",
        &id,
        "--producer",
        "fixture-producer",
    ]));
    let event = fs::read_dir(repo.root.join(".git/arc/repository/events"))
        .unwrap()
        .map(|entry| {
            serde_json::from_slice::<Value>(&fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .find(|event| event["event_type"] == "candidate-registered")
        .unwrap();
    let mut doc = disclosure(&id, &observation, "g1", "public bytes", None);
    doc["selected"] = json!([{"source":{"change_id":"repository","event":event["event_id"]},"fields":["producer assertion"],"access_basis":"withheld-private-assertion"}]);
    stdout(&mut declare(&repo, &id, &doc));
    let mut candidate_doc = doc.clone();
    let head = repo.head(&repo.root);
    candidate_doc["disclosure_id"] = json!("candidate-disclosure");
    candidate_doc["selected"] = json!([{"source":{"change_id":id,"commit":head},"fields":["tree"],"access_basis":"accessible-git"}]);
    stdout(&mut declare(
        &repo,
        event["candidate_id"].as_str().unwrap(),
        &candidate_doc,
    ));
    let bundle = repo.home.join("repository-source.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    let exported: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert!(exported.get("related_changes").is_none());
    assert!(exported["repository_events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|held| held["event_id"] == event["event_id"]));
    let replica = Repo::new();
    deliver_git_objects(&repo, &replica);
    stdout(&mut admitted_round_trip(&replica, &bundle));
    assert!(git_out(
        &replica.root,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/arc/retained/repository/"
        ]
    )
    .contains(&head));
    assert_eq!(
        state(&replica, &id)["round_trip"]["disclosure_availability"][0]["outgoing_bytes"],
        "unavailable"
    );
}

#[test]
fn compact_provenance_unestablished_model_and_human_authorship_are_distinct() {
    let (repo, id) = setup();
    let body = "Review\nAuthor: model not recorded · Review cost: unmeasured.";
    let observation = observe(&repo, &id, "read-1", "g1", body, None);
    let mut unestablished = author(Some("review-model"), None, "act-1");
    unestablished["model_observation"] = Value::Null;
    let mut doc = disclosure(
        &id,
        &observation,
        "g1",
        body,
        Some(comment(vec![unestablished])),
    );
    let output = stdout(&mut declare(&repo, &id, &doc));
    assert!(output.contains("Author: model not recorded · Review cost: unmeasured."));
    assert!(!output.contains("review-model"));
    let (human_repo, human_id) = setup();
    let human_body = "Review\nAuthor: Review Writer · Review cost: unmeasured.";
    let human_observation = observe(&human_repo, &human_id, "read-1", "g1", human_body, None);
    let mut human = author(None, None, "human-act");
    human["name"] = json!("Review Writer");
    doc = disclosure(
        &human_id,
        &human_observation,
        "g1",
        human_body,
        Some(comment(vec![human])),
    );
    assert!(stdout(&mut declare(&human_repo, &human_id, &doc))
        .contains("Author: Review Writer · Review cost: unmeasured."));
}

#[test]
fn compact_provenance_rejects_placeholder_and_malformed_coordinates_before_publication() {
    let (repo, id) = setup();
    let observation = observe(&repo, &id, "read-1", "g1", "public bytes", None);
    let mut doc = disclosure(
        &id,
        &observation,
        "g1",
        "public bytes",
        Some(comment(vec![author(
            Some("review-model"),
            Some("high"),
            "act-1",
        )])),
    );
    let before = event_count(&repo, &id);
    for (field, value) in [
        ("harness", "unknown"),
        ("harness", "none"),
        ("harness", "codex:other"),
        ("model", "unknown"),
        ("model", "model:other"),
        ("model", "model#high"),
        ("effort", "none"),
        ("effort", "high#other"),
    ] {
        let mut bad = doc.clone();
        bad["comment"]["authors"][0][field] = json!(value);
        declare(&repo, &id, &bad).assert().failure();
        assert_eq!(event_count(&repo, &id), before);
    }
    doc["comment"]["authors"][0]["harness"] = Value::Null;
    assert!(stdout(&mut declare(&repo, &id, &doc)).contains("Author: model not recorded"));
    let bundle = repo.home.join("bad-author.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", bundle.to_str().unwrap()]),
    );
    let mut foreign: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    for event in foreign["events"].as_array_mut().unwrap() {
        if event["event_type"] == "context-declared" {
            event["disclosure"]["comment"]["authors"][0]["harness"] = json!("unknown");
        }
    }
    let mut hash = Sha256::new();
    for event in foreign["events"].as_array().unwrap() {
        hash.update(serde_json::to_vec(event).unwrap());
        hash.update(b"\n");
    }
    foreign["events_sha256"] = json!(format!("{:x}", hash.finalize()));
    fs::write(&bundle, foreign.to_string()).unwrap();
    let receiving = Repo::new();
    deliver_git_objects(&repo, &receiving);
    admitted_round_trip(&receiving, &bundle).assert().failure();
    assert!(!receiving.root.join(".git/arc/config.json").exists());
}

#[test]
fn disclosure_attribution_refuses_placeholders_and_session_ids_at_admission() {
    let (repo, id) = setup();
    let observation = observe(&repo, &id, "read-1", "g1", "public bytes", None);
    let session = "019f0000-0000-7000-8000-00000000000a";
    let mut doc = disclosure(
        &id,
        &observation,
        "g1",
        "public bytes",
        Some(comment(vec![author(
            Some("review-model"),
            Some("high"),
            "act-1",
        )])),
    );
    doc["editor"] = json!("local-editor");
    let before = event_count(&repo, &id);
    for pointer in [
        "/author",
        "/reviewer",
        "/editor",
        "/discloser",
        "/comment/roles/0/participant",
        "/comment/authors/0/name",
    ] {
        for value in ["unknown", "N/A", " none ", "codex:unknown", session] {
            let mut bad = doc.clone();
            bad["comment"]["authors"][0] = json!({"name": "Writer", "harness": null,
                "model": null, "effort": null, "act": "act-1", "model_observation": null});
            *bad.pointer_mut(pointer).unwrap() = json!(value);
            if pointer == "/comment/authors/0/name" && value == "codex:unknown" {
                continue;
            }
            declare(&repo, &id, &bad).assert().failure();
            assert_eq!(event_count(&repo, &id), before, "{pointer} {value:?}");
        }
    }
    let mut absent = doc.clone();
    absent["reviewer"] = Value::Null;
    absent["editor"] = Value::Null;
    stdout(&mut declare(&repo, &id, &absent));
}

#[test]
fn forge_delivery_predecessor_change_closure_survives_offline_import() {
    let (repo, origin) = setup();
    let observation = observe(&repo, &origin, "read-1", "g1", "first", None);
    let review = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "review-record",
        "--target",
        "master",
    ])));
    let recipient = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "response-record",
        "--target",
        "master",
    ])));
    let mut first = delivery();
    first["sources"] = json!([source(&origin, &observation, "g1")]);
    stdout(&mut send(&repo, &review, &first));
    let mut later = first.clone();
    later["evidence_id"] = json!("outcome-2");
    later["previous_evidence"] = json!("outcome-1");
    stdout(&mut send(&repo, &recipient, &later));
    let bundle = repo.home.join("outcome-closure.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &recipient, "--output", bundle.to_str().unwrap()]),
    );
    let exported: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let related: Vec<_> = exported["related_changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["change_id"].clone())
        .collect();
    assert!(related.contains(&json!(origin)) && related.contains(&json!(review)));
    let receiving = Repo::new();
    deliver_git_objects(&repo, &receiving);
    stdout(&mut admitted_round_trip(&receiving, &bundle));
    let view: Value = serde_json::from_str(&stdout(
        receiving
            .arc(&receiving.root)
            .args(["forge", "show", &recipient, "--json"]),
    ))
    .unwrap();
    assert_eq!(
        view["deliveries"][0]["delivery"]["previous_evidence"],
        "outcome-1"
    );
    assert_eq!(
        view["deliveries"][0]["delivery"]["acknowledgement"]["standing"],
        "uncertain"
    );
    assert_eq!(
        view["source_availability"][0]["availability"],
        "unavailable"
    );
}

#[test]
fn forge_delivery_disclosure_aliases_and_conflicts_use_the_shared_source_selector() {
    let (repo, id) = setup();
    let seed = repo.home.join("seed.json");
    stdout(
        repo.arc(&repo.root)
            .args(["export", &id, "--output", seed.to_str().unwrap()]),
    );
    let replica = Repo::new();
    deliver_git_objects(&repo, &replica);
    stdout(
        replica
            .arc(&replica.root)
            .args(["import", seed.to_str().unwrap()]),
    );
    let a = observe(&repo, &id, "shared-read", "g1", "first", None);
    let b = observe(&replica, &id, "shared-read", "g1", "first", None);
    let exchanged = replica.home.join("exchange.json");
    stdout(replica.arc(&replica.root).args([
        "export",
        &id,
        "--output",
        exchanged.to_str().unwrap(),
    ]));
    stdout(
        repo.arc(&repo.root)
            .args(["import", exchanged.to_str().unwrap()]),
    );
    let mut sent = delivery();
    sent["sources"] = json!([source(&id, &b, "g1")]);
    stdout(&mut send(&repo, &id, &sent));
    let doc = disclosure(&id, &b, "g1", "first", None);
    stdout(&mut declare(&repo, &id, &doc));
    let view: Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "forge",
        "show",
        &id,
        "--observation",
        &b,
        "--body",
        "g1",
        "--json",
    ])))
    .unwrap();
    assert_eq!(view["body"]["availability"], "available");
    assert_eq!(
        view["source_availability"][0]["canonical_observation_id"],
        a
    );
    assert!(view["source_availability"][0]["aliases"]
        .as_array()
        .unwrap()
        .contains(&json!(b)));
    let conflicting = Repo::new();
    deliver_git_objects(&repo, &conflicting);
    stdout(
        conflicting
            .arc(&conflicting.root)
            .args(["import", seed.to_str().unwrap()]),
    );
    observe(&conflicting, &id, "shared-read", "g1", "other", None);
    let conflict = conflicting.home.join("conflict.json");
    stdout(conflicting.arc(&conflicting.root).args([
        "export",
        &id,
        "--output",
        conflict.to_str().unwrap(),
    ]));
    stdout(
        repo.arc(&repo.root)
            .args(["import", conflict.to_str().unwrap()]),
    );
    let before = event_count(&repo, &id);
    sent["attempt_id"] = json!("attempt-2");
    sent["transport_request"] = json!("request-2");
    send(&repo, &id, &sent).assert().failure();
    let mut bad = doc.clone();
    bad["disclosure_id"] = json!("bad-source");
    declare(&repo, &id, &bad).assert().failure();
    assert_eq!(event_count(&repo, &id), before);
    let current: Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["forge", "show", &id, "--json"]),
    ))
    .unwrap();
    assert!(current["source_availability"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["source_status"] == "conflict"));
    assert_eq!(
        current["deliveries"][0]["delivery"]["acknowledgement"]["standing"],
        "uncertain"
    );
}
