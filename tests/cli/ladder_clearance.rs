//! Ladder clearance is authoritative only under assessed policy; every other bar remains.
use super::common::*;
use serde_json::{json, Value};

const SOL: &str = "gpt-6.1-sol#xhigh";
const FABLE: &str = "fable-5.1#high";
const OPUS: &str = "claude-opus-5-5#high";
const GEMINI: &str = "gemini-test#high";

fn table() -> Value {
    json!({"schema":"arc-review-ladder/1","version":[{
        "id":"v1","effective_from":"2026-01-01T00:00:00Z","source":"fixture capability judgment","gap":false,
        "model":[
            {"model":"gpt-6.1-sol","identities":["gpt-6.1-sol","sol-alias"],"family":"openai","superseded":false,
             "efforts":[{"effort":"medium","rank":4,"floor":2},{"effort":"xhigh","rank":4,"floor":3}]},
            {"model":"fable-5.1","identities":["fable-5.1"],"family":"anthropic","superseded":true,
             "efforts":[{"effort":"medium","rank":1,"floor":1},{"effort":"high","rank":4,"floor":3}]},
            {"model":"claude-opus-5-5","identities":["claude-opus-5-5"],"family":"anthropic","superseded":false,
             "efforts":[{"effort":"low","rank":2,"floor":1},{"effort":"high","rank":4,"floor":3}]},
            {"model":"gemini-test","identities":["gemini-test"],"family":"google","superseded":false,
             "efforts":[{"effort":"medium","rank":2,"floor":2},{"effort":"high","rank":4,"floor":3}]}
        ]
    }]})
}
fn repo(assessed: bool, ladder: bool, since: Option<&str>) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    let mut policy = "[policy]\nforbid_self_approval = true\n".to_string();
    if assessed {
        policy.push_str("review_clearance = \"assessed\"\n");
    }
    if let Some(since) = since {
        policy.push_str(&format!("review_clearance_since = {since:?}\n"));
    }
    fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
    if ladder {
        fs::write(
            repo.root.join(".arc/review-ladder.toml"),
            toml::to_string(&table()).unwrap(),
        )
        .unwrap();
    }
    git(&repo.root, &["add", ".arc"]);
    git(
        &repo.root,
        &["commit", "-m", "test: clearance declarations"],
    );
    repo
}
fn subject(repo: &Repo, slug: &str, author: Option<&str>, trailers: &str) -> PathBuf {
    repo.arc(&repo.root)
        .args(["begin", slug])
        .assert()
        .success();
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &wt,
        "one.txt",
        "one",
        &format!("test: subject\n\n{trailers}"),
    );
    repo.commit(&wt, "two.txt", "two", "test: second file");
    let mut cmd = repo.arc(&wt);
    cmd.env("ARC_ACTOR", "Writer");
    if let Some(model) = author {
        cmd.args(["--model", model]);
    }
    cmd.args(["snapshot", slug]).assert().success();
    wt
}
fn owed(repo: &Repo, slug: &str, author: Option<&str>, trailers: &str) -> PathBuf {
    let wt = subject(repo, slug, author, trailers);
    repo.arc(&wt)
        .args(["integrate", slug, "--debt", "fixture unread"])
        .assert()
        .success();
    wt
}
fn audit(repo: &Repo, slug: &str, model: Option<&str>, file: Option<&Path>) -> String {
    let mut cmd = repo.arc(&repo.root);
    cmd.env("ARC_ACTOR", "Reader");
    if let Some(model) = model {
        cmd.args(["--model", model]);
    }
    cmd.args(["audit", slug, "--verdict", "approved"]);
    if let Some(file) = file {
        cmd.arg("--assessment").arg(file);
    }
    stdout(&mut cmd)
}
fn show(repo: &Repo, slug: &str) -> Value {
    json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]))
}
fn judgment(value: &Value) -> &Value {
    let event = value["audit_verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap();
    &value["review_context"]["judgments"][event]
}
fn assessment(repo: &Repo, name: &str, model: &str, clears: bool) -> PathBuf {
    let (model, effort) = model.rsplit_once('#').unwrap();
    let path = repo.home.join(name);
    fs::write(&path,json!({
        "schema":"agent-routing-assessment/2","independence":if clears {"eligible"}else{"ineligible"},"findings":if clears {"eligible"}else{"ineligible"},"clears":clears,
        "qualifiers":if clears {vec![]} else {vec!["below-floor"]},
        "reviewer":{"identities":[model],"effort":effort},"authors":[]
    }).to_string()).unwrap();
    path
}

/// A capable reader discharges without an assessment or any routing executable.
#[test]
fn a_clearing_read_needs_neither_routing_nor_an_assessment() {
    let repo = repo(true, true, None);
    owed(&repo, "no-routing", Some(SOL), "");
    let bin = repo.home.join("git-only");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink("/usr/bin/git", bin.join("git")).unwrap();
    repo.arc(&repo.root)
        .env("PATH", &bin)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "audit",
            "no-routing",
            "--verdict",
            "approved",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("debt discharged"));
    let value = show(&repo, "no-routing");
    assert!(!value["debt"]["discharged_by"].is_null());
    assert_eq!(judgment(&value)["clears"], true);
}
/// Unseated models are judged by rank, while a shared family still prevents clearing.
#[test]
fn an_unseated_same_family_reader_finds_without_clearing() {
    let repo = repo(true, true, None);
    owed(&repo, "family", Some(OPUS), "");
    audit(&repo, "family", Some(FABLE), None);
    let value = show(&repo, "family");
    let j = judgment(&value);
    assert_eq!(j["findings_eligible"], true);
    assert_eq!(j["clears"], false);
    assert_eq!(j["reason"], "same-family");
    assert!(value["debt"]["discharged_by"].is_null());
    audit(&repo, "family", Some(SOL), None);
    assert!(!show(&repo, "family")["debt"]["discharged_by"].is_null());
}
#[test]
fn a_true_assessment_cannot_overrule_a_nonclearing_ladder() {
    let repo = repo(true, true, None);
    owed(&repo, "true-file", Some(OPUS), "");
    let file = assessment(&repo, "true.json", FABLE, true);
    audit(&repo, "true-file", Some(FABLE), Some(&file));
    let value = show(&repo, "true-file");
    assert!(value["debt"]["discharged_by"].is_null());
    assert_eq!(
        judgment(&value)["assessment_disagreement"]["assessment_clears"],
        true
    );
    assert_eq!(
        judgment(&value)["assessment_disagreement"]["ladder_clears"],
        false
    );
    let text = stdout(repo.arc(&repo.root).args(["show", "true-file"]));
    assert!(text.contains("assessment-disagreement"), "{text}");
}
#[test]
fn a_false_bound_assessment_cannot_veto_a_clearing_ladder() {
    let repo = repo(true, true, None);
    owed(&repo, "false-file", Some(SOL), "");
    let file = assessment(&repo, "false.json", FABLE, false);
    audit(&repo, "false-file", Some(FABLE), Some(&file));
    let value = show(&repo, "false-file");
    assert!(!value["debt"]["discharged_by"].is_null());
    assert_eq!(
        judgment(&value)["assessment_disagreement"]["assessment_clears"],
        false
    );
    assert_eq!(
        judgment(&value)["assessment_disagreement"]["ladder_clears"],
        true
    );
    let text = stdout(repo.arc(&repo.root).args(["show", "false-file"]));
    assert!(text.contains("assessment-disagreement"), "{text}");
}
/// Both controls pin the exact activation boundary.
#[test]
fn assessed_without_a_ladder_keeps_assessment_only_judgment() {
    let repo = repo(true, false, None);
    owed(&repo, "no-ladder", Some(SOL), "");
    audit(&repo, "no-ladder", Some(FABLE), None);
    assert!(show(&repo, "no-ladder")["debt"]["discharged_by"].is_null());
}
#[test]
fn a_ladder_without_assessed_policy_changes_no_clearance() {
    let repo = repo(false, true, None);
    owed(&repo, "no-policy", Some(OPUS), "");
    audit(&repo, "no-policy", Some(FABLE), None);
    assert!(!show(&repo, "no-policy")["debt"]["discharged_by"].is_null());
}
#[test]
fn a_read_must_answer_every_labs_author() {
    let repo = repo(true, true, None);
    owed(
        &repo,
        "mixed-labs",
        Some(SOL),
        "Implemented-by: claude:claude-opus-5-5#high",
    );
    audit(&repo, "mixed-labs", Some(FABLE), None);
    let value = show(&repo, "mixed-labs");
    assert_eq!(judgment(&value)["findings_eligible"], true);
    assert_eq!(judgment(&value)["clears"], false);
    audit(&repo, "mixed-labs", Some(GEMINI), None);
    assert!(!show(&repo, "mixed-labs")["debt"]["discharged_by"].is_null());
}
#[test]
fn an_unknown_author_cannot_be_cleared_by_a_true_assessment() {
    let repo = repo(true, true, None);
    owed(&repo, "unknown-writer", None, "");
    let file = assessment(&repo, "unknown-true.json", FABLE, true);
    audit(&repo, "unknown-writer", Some(FABLE), Some(&file));
    let value = show(&repo, "unknown-writer");
    assert!(value["debt"]["discharged_by"].is_null());
    assert_eq!(judgment(&value)["kind"], "not-established");
}
#[test]
fn a_lower_effort_same_model_verdict_is_not_findings_eligible() {
    let repo = repo(true, true, None);
    let wt = subject(&repo, "lower", Some(SOL), "");
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            "gpt-6.1-sol#medium",
            "review",
            "lower",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    let value = show(&repo, "lower");
    let event = value["verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap();
    let j = &value["review_context"]["judgments"][event];
    assert_eq!(j["findings_eligible"], false);
    assert_eq!(j["reason"], "same-model-lower-effort");
}
#[test]
fn an_unknown_effort_is_named_not_established() {
    let repo = repo(true, true, None);
    owed(&repo, "outside-effort", Some(SOL), "");
    audit(&repo, "outside-effort", Some("fable-5.1#max"), None);
    let value = show(&repo, "outside-effort");
    let j = judgment(&value);
    assert_eq!(j["kind"], "not-established");
    assert!(j["details"].to_string().contains("fable-5.1#max"));
}
#[test]
fn an_effortless_legacy_reviewer_uses_the_complete_rows_minimum() {
    let repo = repo(true, true, Some("2099-01-01T00:00:00Z"));
    owed(&repo, "effortless-reader", Some(SOL), "");
    audit(&repo, "effortless-reader", Some("fable-5.1"), None);
    let value = show(&repo, "effortless-reader");
    let j = judgment(&value);
    assert_eq!(j["reason"], "below-floor");
    assert!(j["conservative_bounds"]
        .to_string()
        .contains("minimum rank 1"));
}
#[test]
fn an_effortless_author_uses_the_complete_rows_maximum() {
    let repo = repo(true, true, None);
    owed(&repo, "effortless-writer", Some("claude-opus-5-5"), "");
    audit(&repo, "effortless-writer", Some("gemini-test#medium"), None);
    let value = show(&repo, "effortless-writer");
    let j = judgment(&value);
    assert_eq!(j["reason"], "below-floor");
    assert!(j["conservative_bounds"]
        .to_string()
        .contains("maximum floor 3"));
}
#[test]
fn coordinate_enforcement_starts_at_the_policy_cutoff() {
    let repo = repo(true, true, Some("2026-01-01T00:00:00Z"));
    owed(&repo, "required-coordinate", Some(SOL), "");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reader")
        .args(["audit", "required-coordinate", "--verdict", "approved"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "review-clearance-coordinate-required",
        ));
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Person <person@example.org>")
        .args(["audit", "required-coordinate", "--verdict", "approved"])
        .assert()
        .success();
}
/// A capable read must still cover the entire obligation.
#[test]
fn ladder_clearance_does_not_replace_extent_coverage() {
    let repo = repo(true, true, None);
    owed(&repo, "partial", Some(SOL), "");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "audit",
            "partial",
            "--verdict",
            "approved",
            "--extent-paths",
            "one.txt",
        ])
        .assert()
        .success();
    let value = show(&repo, "partial");
    assert_eq!(judgment(&value)["clears"], true);
    assert!(value["debt"]["discharged_by"].is_null());
    assert!(!value["debt"]["partial_coverage"]
        .as_array()
        .unwrap()
        .is_empty());
}
/// Optional assessment evidence must still bind the actual coordinate.
#[test]
fn ladder_clearance_does_not_replace_assessment_binding() {
    let repo = repo(true, true, None);
    owed(&repo, "wrong-binding", Some(SOL), "");
    let file = assessment(&repo, "wrong.json", GEMINI, true);
    audit(&repo, "wrong-binding", Some(FABLE), Some(&file));
    let value = show(&repo, "wrong-binding");
    assert_eq!(judgment(&value)["clears"], true);
    assert!(value["debt"]["discharged_by"].is_null());
    assert!(value["debt"]["findings_reads"]
        .to_string()
        .contains("unbound"));
}
/// A ladder cannot confer permission on an actor who wrote the subject.
#[test]
fn ladder_clearance_does_not_replace_actor_independence() {
    let repo = repo(true, true, None);
    let wt = subject(&repo, "same-actor", Some(SOL), "");
    repo.arc(&wt)
        .env("ARC_ACTOR", "Writer")
        .args([
            "--model",
            FABLE,
            "review",
            "same-actor",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    let value = show(&repo, "same-actor");
    let event = value["verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap();
    assert_eq!(value["review_context"]["judgments"][event]["clears"], false);
    let status = json_stdout(repo.arc(&wt).args(["status", "same-actor", "--json"]));
    assert_eq!(status["has_valid_approval"], false);
}

/// A clearing approval gates actual integration without a debt or assessment.
#[test]
fn a_ladder_clearing_approval_authorizes_integration() {
    let repo = repo(true, true, None);
    let wt = subject(&repo, "approved", Some(SOL), "");
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "review",
            "approved",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "approved", "--json"]));
    assert_eq!(status["has_valid_approval"], true);
    repo.arc(&wt)
        .args(["integrate", "approved"])
        .assert()
        .success();
}
/// The candidate's stronger table cannot supply its own approving judgment.
#[test]
fn a_candidate_cannot_raise_its_approving_readers_rank() {
    let repo = repo(true, true, None);
    let mut target = table();
    target["version"][0]["model"][1]["efforts"][1]["rank"] = json!(1);
    fs::write(
        repo.root.join(".arc/review-ladder.toml"),
        toml::to_string(&target).unwrap(),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/review-ladder.toml"]);
    git(
        &repo.root,
        &["commit", "-m", "test: target floor excludes reader"],
    );
    let wt = subject(&repo, "candidate-table", Some(SOL), "");
    fs::write(
        wt.join(".arc/review-ladder.toml"),
        toml::to_string(&table()).unwrap(),
    )
    .unwrap();
    git(&wt, &["add", ".arc/review-ladder.toml"]);
    git(&wt, &["commit", "-m", "test: candidate reader capability"]);
    repo.arc(&wt)
        .env("ARC_ACTOR", "Writer")
        .args(["--model", SOL, "snapshot", "candidate-table"])
        .assert()
        .success();
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "review",
            "candidate-table",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&wt).args(["status", "candidate-table", "--json"]));
    assert_eq!(status["has_valid_approval"], false);
    repo.arc(&wt)
        .args(["integrate", "candidate-table"])
        .assert()
        .failure();
}

/// A ladder clearing read discharges ineligible-review for an agent; the
/// current person assessment requirement remains independent of capability.
#[test]
fn composed_ineligible_debt_keeps_the_person_requirement_but_allows_a_ladder_agent() {
    let repo = repo(true, true, None);
    let wt = subject(&repo, "ineligible", Some(OPUS), "");
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "review",
            "ineligible",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&wt)
        .args([
            "integrate",
            "ineligible",
            "--debt",
            "fixture same-family read",
        ])
        .assert()
        .success();
    assert_eq!(
        show(&repo, "ineligible")["debt"]["missing"],
        "ineligible-review"
    );
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Person <person@example.org>")
        .args(["audit", "ineligible", "--verdict", "approved"])
        .assert()
        .success();
    assert!(show(&repo, "ineligible")["debt"]["discharged_by"].is_null());
    audit(&repo, "ineligible", Some(SOL), None);
    assert!(!show(&repo, "ineligible")["debt"]["discharged_by"].is_null());
}
/// The assessment must bind the observed coordinate even when both the
/// declared and observed models meet the ladder capability bar.
#[test]
fn composed_observed_model_binding_cannot_be_bypassed_by_a_capable_declaration() {
    let repo = repo(true, true, None);
    owed(&repo, "observed", Some(SOL), "");
    let mut row = super::model_resolution::assistant("answer", "2026-11-01T00:00:00Z", "high");
    row["message"]["model"] = json!("gemini-test");
    super::model_resolution::recording(&repo, &[row]);
    let file = assessment(&repo, "declared.json", FABLE, true);
    super::model_resolution::acting(&repo)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "audit",
            "observed",
            "--verdict",
            "approved",
            "--assessment",
        ])
        .arg(file)
        .assert()
        .success();
    let value = show(&repo, "observed");
    assert_eq!(judgment(&value)["clears"], true);
    assert!(value["debt"]["discharged_by"].is_null());
    assert!(value["debt"]["findings_reads"]
        .to_string()
        .contains("unbound"));
}
/// An imported capable read remains a fact without receiver review authority.
#[test]
fn composed_import_authority_cannot_be_supplied_by_the_ladder() {
    let source = repo(true, true, None);
    owed(&source, "foreign", Some(SOL), "");
    audit(&source, "foreign", Some(FABLE), None);
    let bundle = source.home.join("foreign.json");
    source
        .arc(&source.root)
        .args(["export", "foreign", "--output"])
        .arg(&bundle)
        .assert()
        .success();
    let value: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let receiver = repo(true, true, None);
    deliver_git_objects(&source, &receiver);
    let publisher = value["source_replica_id"]
        .as_str()
        .unwrap_or_else(|| value["repository_id"].as_str().unwrap());
    let origin = value["repository_id"].as_str().unwrap();
    receiver.declare_gates_locally(&format!(
        "[replica]\ntrusted_review_replicas=[]\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{publisher}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['facts','review-input','debt-input','integration']\nactors=['tester','Writer','Reader']\n"));
    let digest = hex::encode(Sha256::digest(fs::read(&bundle).unwrap()));
    receiver
        .arc(&receiver.root)
        .args([
            "import",
            bundle.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &digest,
        ])
        .assert()
        .success();
    let shown = show(&receiver, "foreign");
    assert_eq!(judgment(&shown)["clears"], true);
    assert!(shown["debt"]["discharged_by"].is_null());
}

/// A refused composed read cannot publish the snapshot before discovering
/// that its mandatory reviewer coordinate is missing.
#[test]
fn missing_coordinates_refuse_before_a_composed_snapshot() {
    let repo = repo(true, true, None);
    let wt = subject(&repo, "composed-coordinate", Some(SOL), "");
    repo.commit(&wt, "three.txt", "three", "test: pending snapshot");
    let before = show(&repo, "composed-coordinate")["patchsets"]
        .as_array()
        .unwrap()
        .len();
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "review",
            "composed-coordinate",
            "--snapshot",
            "--solo",
            "--verdict",
            "approved",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "review-clearance-coordinate-required",
        ));
    assert_eq!(
        show(&repo, "composed-coordinate")["patchsets"]
            .as_array()
            .unwrap()
            .len(),
        before
    );
}

/// A trusted foreign verdict is capability evidence, not authority to use
/// the sender closure's ladder instead of the receiver registry.
#[test]
fn a_foreign_preintegration_verdict_uses_the_receivers_registry() {
    let source = repo(true, true, None);
    let wt = subject(&source, "received-verdict", Some(SOL), "");
    source
        .arc(&wt)
        .args([
            "debt",
            "received-verdict",
            "--reason",
            "fixture review owed",
        ])
        .assert()
        .success();
    source
        .arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "review",
            "received-verdict",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    source
        .arc(&wt)
        .args(["integrate", "received-verdict"])
        .assert()
        .success();
    let bundle = source.home.join("verdict.json");
    source
        .arc(&source.root)
        .args(["export", "received-verdict", "--output"])
        .arg(&bundle)
        .assert()
        .success();
    let value: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let receiver = repo(true, true, None);
    let mut lower = table();
    lower["version"][0]["model"][1]["efforts"][1]["rank"] = json!(1);
    fs::write(
        receiver.root.join(".arc/review-ladder.toml"),
        toml::to_string(&lower).unwrap(),
    )
    .unwrap();
    git(&receiver.root, &["add", ".arc/review-ladder.toml"]);
    git(
        &receiver.root,
        &["commit", "-m", "test: receiver capability basis"],
    );
    deliver_git_objects(&source, &receiver);
    let publisher = value["source_replica_id"]
        .as_str()
        .unwrap_or_else(|| value["repository_id"].as_str().unwrap());
    let origin = value["repository_id"].as_str().unwrap();
    receiver.declare_gates_locally(&format!(
        "[replica]\ntrusted_review_replicas=[\'{origin}\']\n[[replica.publishers]]\nprincipal=\'channel\'\nsource_replica_id=\'{publisher}\'\n[[replica.publishers.origins]]\nreplica_id=\'{origin}\'\nroles=[\'facts\',\'review-authority\',\'debt-input\',\'integration\']\nactors=[\'tester\',\'Writer\',\'Reader\']\n"));
    let digest = hex::encode(Sha256::digest(fs::read(&bundle).unwrap()));
    receiver
        .arc(&receiver.root)
        .args([
            "import",
            bundle.to_str().unwrap(),
            "--publisher",
            "channel",
            "--received-sha256",
            &digest,
        ])
        .assert()
        .success();
    let shown = show(&receiver, "received-verdict");
    assert!(shown["debt"]["discharged_by"].is_null());
    let event = shown["verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        shown["review_context"]["judgments"][event]["reason"],
        "below-floor"
    );
}

/// Effort-only observed disagreement still constrains capability, even
/// though the identity self-review check compares model names without effort.
#[test]
fn an_observed_lower_effort_cannot_borrow_the_declared_effort_rank() {
    let repo = repo(true, true, None);
    owed(&repo, "observed-effort", Some(SOL), "");
    let mut row = super::model_resolution::assistant("answer", "2026-11-01T00:00:00Z", "medium");
    row["message"]["model"] = json!("fable-5.1");
    super::model_resolution::recording(&repo, &[row]);
    super::model_resolution::acting(&repo)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "audit",
            "observed-effort",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    let value = show(&repo, "observed-effort");
    assert!(value["debt"]["discharged_by"].is_null());
    assert_eq!(judgment(&value)["reason"], "below-floor");
}
