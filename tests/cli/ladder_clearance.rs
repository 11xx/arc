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

fn ladder_candidate() -> (Repo, PathBuf, String) {
    ladder_candidate_with_policy(true)
}

fn ladder_candidate_with_policy(assessed: bool) -> (Repo, PathBuf, String) {
    let repo = repo(assessed, true, None);
    let mut lower = table();
    lower["version"][0]["model"][1]["efforts"][1]["rank"] = json!(1);
    fs::write(
        repo.root.join(".arc/review-ladder.toml"),
        toml::to_string(&lower).unwrap(),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/review-ladder.toml"]);
    git(&repo.root, &["commit", "-m", "test: prior ladder"]);
    let before = repo.head(&repo.root);
    let wt = subject(&repo, "ladder-debt", Some(SOL), "");
    fs::write(
        wt.join(".arc/review-ladder.toml"),
        toml::to_string(&table()).unwrap(),
    )
    .unwrap();
    git(&wt, &["add", ".arc/review-ladder.toml"]);
    git(&wt, &["commit", "-m", "test: proposed reader promotion"]);
    repo.arc(&wt)
        .env("ARC_ACTOR", "Writer")
        .args(["--model", SOL, "snapshot", "ladder-debt"])
        .assert()
        .success();
    (repo, wt, before)
}

fn integrated_ladder_change() -> (Repo, PathBuf, String) {
    let (repo, wt, before) = ladder_candidate();
    repo.arc(&wt)
        .args([
            "integrate",
            "ladder-debt",
            "--debt",
            "fixture ladder unread",
        ])
        .assert()
        .success();
    (repo, wt, before)
}

/// A ladder edit cannot promote the reader whose audit discharges that edit.
/// Rejects the integrated table becoming its own clearance authority.
#[test]
fn reviewed_ladder_change_audit_uses_the_prechange_registry() {
    let (repo, _, before) = integrated_ladder_change();
    audit(&repo, "ladder-debt", Some(FABLE), None);
    let value = show(&repo, "ladder-debt");
    assert_eq!(value["debt"]["standing"], "owed");
    assert!(value["debt"]["discharged_by"].is_null());
    assert_eq!(value["review_context"]["authorizing"]["revision"], before);
    assert_eq!(judgment(&value)["clears"], false);
    assert_eq!(judgment(&value)["reason"], "below-floor");
}

fn assert_checkout_clearance_is_not_loosened(commit_policy: bool) {
    for ladder in [false, true] {
        let repo = repo(false, ladder, None);
        let wt = owed(&repo, "checkout-clearance", Some(SOL), "");
        fs::write(
            wt.join(".arc/policy.toml"),
            "[policy]\nforbid_self_approval = true\nreview_clearance = \"assessed\"\n",
        )
        .unwrap();
        if commit_policy {
            git(&wt, &["add", ".arc/policy.toml"]);
            git(&wt, &["commit", "-m", "test: awaiting clearance policy"]);
        }
        repo.arc(&wt)
            .env("ARC_ACTOR", "Reader")
            .args([
                "--model",
                FABLE,
                "audit",
                "checkout-clearance",
                "--verdict",
                "approved",
            ])
            .assert()
            .success();
        let value = json_stdout(repo.arc(&wt).args(["show", "checkout-clearance", "--json"]));
        assert_eq!(
            value["debt"]["standing"], "owed",
            "ladder={ladder}, committed={commit_policy}"
        );
        assert!(value["debt"]["discharged_by"].is_null());
        assert!(value["debt"]["findings_reads"]
            .to_string()
            .contains("no-assessment"));
        let status = json_stdout(
            repo.arc(&wt)
                .args(["status", "checkout-clearance", "--json"]),
        );
        assert_eq!(status["debt_outstanding"], true);
        let query = stdout(repo.arc(&wt).args(["query", "--debt"]));
        assert!(query.contains("checkout-clearance"), "{query}");
        let inbox = json_stdout(repo.arc(&wt).args(["inbox", "--json"]));
        assert!(
            inbox["debt-owed"]
                .to_string()
                .contains("checkout-clearance"),
            "{inbox}"
        );
        let catchup = json_stdout(repo.arc(&wt).args(["catchup", "--json"]));
        assert!(
            catchup["ledger"]["debt-owed"]
                .to_string()
                .contains("checkout-clearance"),
            "{catchup}"
        );
    }
}

/// A target table with legacy policy cannot lower the checkout recording rule.
#[test]
fn reviewed_checkout_clearance_stays_assessed_with_an_uncommitted_policy() {
    assert_checkout_clearance_is_not_loosened(false);
}

#[test]
fn reviewed_checkout_clearance_stays_assessed_with_an_awaiting_policy() {
    assert_checkout_clearance_is_not_loosened(true);
}

/// Missing target refs do not create a ladder context or author scans when
/// the repository has no ladder file. Every no-ladder JSON view omits the key.
#[test]
fn reviewed_missing_target_without_a_ladder_omits_context_in_every_view() {
    let repo = repo(false, false, None);
    git(&repo.root, &["branch", "release"]);
    repo.arc(&repo.root)
        .args(["begin", "missing-target", "--target", "release"])
        .assert()
        .success();
    let wt = repo.home.join(".worktrees/repo-missing-target");
    repo.commit(&wt, "work.txt", "work", "test: subject");
    repo.arc(&wt)
        .args(["--model", SOL, "snapshot", "missing-target"])
        .assert()
        .success();
    git(&repo.root, &["branch", "-d", "release"]);
    let mut unexpected = Vec::new();
    for command in ["show", "status", "review", "check"] {
        let value =
            json_stdout_any_status(repo.arc(&wt).args([command, "missing-target", "--json"]));
        if value.get("review_context").is_some() {
            unexpected.push(command);
        }
    }
    assert!(
        unexpected.is_empty(),
        "no-ladder views gained context: {unexpected:?}"
    );
}

/// A historical closure without its pre-change revision cannot authorize
/// itself through today's table. Rejects silently substituting the target head.
#[test]
fn reviewed_ladder_change_without_a_closure_basis_stays_unestablished() {
    let (repo, _, _) = integrated_ladder_change();
    let id = show(&repo, "ladder-debt")["change_id"]
        .as_str()
        .unwrap()
        .to_string();
    rewrite_event(&repo, &id, "change-integrated", |event| {
        event["event_type"] = json!("change-closed");
        event["outcome"] = json!("integrated");
        event.as_object_mut().unwrap().remove("target_before");
    });
    audit(&repo, "ladder-debt", Some(FABLE), None);
    let value = show(&repo, "ladder-debt");
    assert_eq!(value["debt"]["standing"], "owed");
    assert_eq!(judgment(&value)["kind"], "not-established");
    assert!(judgment(&value)["details"]
        .to_string()
        .contains("target_before"));
}

/// A subdirectory invocation must select the same frozen registry as the root.
/// Rejects cwd-relative path detection that misses the ladder edit.
#[test]
fn reviewed_ladder_change_keeps_its_registry_from_a_subdirectory() {
    let (repo, _, _) = integrated_ladder_change();
    audit(&repo, "ladder-debt", Some(FABLE), None);
    let shown = show(&repo, "ladder-debt");
    let event = shown["audit_verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap();
    let nested = repo.root.join("nested");
    fs::create_dir(&nested).unwrap();
    let status = json_stdout(repo.arc(&nested).args(["status", "ladder-debt", "--json"]));
    assert_eq!(
        status["review_context"]["judgments"][event]["clears"],
        false
    );
    assert_eq!(status["debt_outstanding"], true);
}

/// A replayed ladder edit cannot rejudge its earlier verdict or debt audit
/// through the table it landed. Rejects accepted-head authority after replay.
#[test]
fn reviewed_replayed_ladder_change_keeps_verdict_and_audit_under_prior_authority() {
    let (repo, wt, _) = ladder_candidate();
    repo.arc(&wt)
        .env("ARC_ACTOR", "Reader")
        .args([
            "--model",
            FABLE,
            "review",
            "ladder-debt",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.redeclare_gates("[gates.smoke]\ncommand = \"true\"\n");
    let before = repo.head(&repo.root);
    repo.arc(&wt)
        .args(["verify", "ladder-debt", "--all"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["land", "ladder-debt", "--debt", "fixture ladder unread"])
        .assert()
        .success();
    audit(&repo, "ladder-debt", Some(FABLE), None);
    let value = show(&repo, "ladder-debt");
    assert_eq!(value["debt"]["standing"], "owed");
    assert_eq!(value["closure"]["target_before"], before);
    assert_ne!(
        value["closure"]["source_head"],
        value["closure"]["integrated_commit"]
    );
    let verdict = value["verdicts"].as_array().unwrap().last().unwrap()["event_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        value["review_context"]["judgments"][verdict]["clears"],
        false
    );
    assert_eq!(judgment(&value)["clears"], false);
}

/// An unavailable historical basis cannot switch a legacy policy to assessed.
/// Rejects a ladder file adding a clearance bar when neither policy requires it.
#[test]
fn a_missing_basis_does_not_activate_clearance_for_legacy_policy() {
    let (repo, wt, _) = ladder_candidate_with_policy(false);
    repo.arc(&wt)
        .args([
            "integrate",
            "ladder-debt",
            "--debt",
            "fixture ladder unread",
        ])
        .assert()
        .success();
    let id = show(&repo, "ladder-debt")["change_id"]
        .as_str()
        .unwrap()
        .to_string();
    rewrite_event(&repo, &id, "change-integrated", |event| {
        event["event_type"] = json!("change-closed");
        event["outcome"] = json!("integrated");
        event.as_object_mut().unwrap().remove("target_before");
    });
    audit(&repo, "ladder-debt", Some(FABLE), None);
    let value = show(&repo, "ladder-debt");
    assert_eq!(value["debt"]["standing"], "discharged");
    assert!(value["review_context"]["judgments"].is_null());
}

mod basis_time {
    use super::*;
    const SESSION: &str = "01a12b77-6c25-7c01-b325-6e5e44523cc4";
    const ISSUED: &str = "2026-09-01T00:00:00Z";
    const EARLY: &str = "2026-09-02T00:00:00Z";
    const LATE: &str = "2026-10-07T00:00:00Z";

    fn dated_repo() -> Repo {
        let repo = repo(true, true, None);
        let mut dated = table();
        let mut late = dated["version"][0].clone();
        late["id"] = json!("v2");
        late["effective_from"] = json!(LATE);
        dated["version"][0]["model"][1]["efforts"][1]["rank"] = json!(1);
        dated["version"].as_array_mut().unwrap().push(late);
        fs::write(
            repo.root.join(".arc/review-ladder.toml"),
            toml::to_string(&dated).unwrap(),
        )
        .unwrap();
        git(&repo.root, &["add", ".arc/review-ladder.toml"]);
        git(
            &repo.root,
            &["commit", "-m", "test: dated capability table"],
        );
        repo
    }
    fn receipt(repo: &Repo, slug: &str, audit: bool) -> Value {
        if audit {
            owed(repo, slug, Some(SOL), "");
        } else {
            subject(repo, slug, Some(SOL), "");
        }
        let kind = if audit { "audit" } else { "verdict" };
        let mut value = json_stdout(repo.arc(&repo.root).args([
            "receipt",
            "template",
            &format!("--{kind}"),
            slug,
        ]));
        value["issued_at"] = json!(ISSUED);
        let subject = value["items"][0]["subject"].to_string();
        let digest = hex::encode(Sha256::digest(
            format!("{kind}\n{subject}\n{ISSUED}").as_bytes(),
        ));
        value["items"][0]["key"] = json!(format!("rk-{}", &digest[..32]));
        value["items"][0]["verdict"] = json!("approved");
        value["reviewer"] = json!({"harness":"codex","session":SESSION,"model":FABLE});
        value
    }
    fn reply(repo: &Repo, text: &str, at: &str, role: &str) {
        let path = codex_recording(repo, SESSION, "fable-5.1");
        use std::io::Write;
        let row = json!({"type":"response_item","timestamp":at,"payload":{"type":"message","role":role,"content":[{"type":"output_text","text":text}]}});
        writeln!(
            fs::OpenOptions::new().append(true).open(path).unwrap(),
            "{row}"
        )
        .unwrap();
    }
    fn ingest(repo: &Repo, value: &Value, audit: bool) -> Value {
        let path = repo.home.join("receipt.json");
        fs::write(&path, value.to_string()).unwrap();
        json_stdout(
            repo.arc(&repo.root)
                .env("CODEX_HOME", repo.home.join("codex-state"))
                .args([
                    if audit { "audit" } else { "review" },
                    "--receipt",
                    path.to_str().unwrap(),
                    "--json",
                ]),
        )
    }
    fn read(shown: &Value, audit: bool) -> &Value {
        shown[if audit { "audit_verdicts" } else { "verdicts" }]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
    }
    fn judged(shown: &Value, audit: bool) -> &Value {
        &shown["review_context"]["judgments"][read(shown, audit)["event_id"].as_str().unwrap()]
    }

    /// A matching existing reply pins the earlier capability table on both read paths.
    /// Rejects ingestion time granting capability the reader lacked at judgment.
    #[test]
    fn basis_reply_before_version_survives_late_ingestion() {
        for audit in [false, true] {
            let repo = dated_repo();
            let value = receipt(&repo, "reply-time", audit);
            reply(
                &repo,
                &format!("{}: approved", value["items"][0]["key"].as_str().unwrap()),
                EARLY,
                "assistant",
            );
            ingest(&repo, &value, audit);
            let shown = show(&repo, "reply-time");
            assert_eq!(judged(&shown, audit)["clears"], false, "{shown:#}");
            assert_eq!(judged(&shown, audit)["basis_version"], "v1");
            assert_eq!(judged(&shown, audit)["basis_source"], "session-reply");
            assert_eq!(read(&shown, audit)["basis_time"], EARLY);
            assert_eq!(read(&shown, audit)["basis_version"], "v1");
        }
    }

    /// Issuing a template does not date the read; only a matching judgment does.
    /// Rejects issuance time and another item's or user's reply as the basis.
    #[test]
    fn basis_issued_template_and_unmatched_reply_do_not_backdate_a_read() {
        let repo = dated_repo();
        let value = receipt(&repo, "no-reply", true);
        let key = value["items"][0]["key"].as_str().unwrap();
        reply(
            &repo,
            &format!("{key}-other: approved\n{key}: changes-requested"),
            EARLY,
            "assistant",
        );
        ingest(&repo, &value, true);
        let shown = show(&repo, "no-reply");
        assert_eq!(judged(&shown, true)["clears"], true);
        assert_eq!(judged(&shown, true)["basis_source"], "recorded");
        assert_eq!(
            read(&shown, true)["basis_time"],
            read(&shown, true)["created_at"]
        );
    }

    /// An active recording with no matching reply uses declared time, including
    /// the issued boundary and the exact version instant, rather than session age.
    #[test]
    fn basis_active_session_uses_bounded_declaration() {
        for audit in [false, true] {
            for (at, version, clears) in [(ISSUED, "v1", false), (LATE, "v2", true)] {
                let repo = dated_repo();
                let mut value = receipt(&repo, "declared", audit);
                value["items"][0]["judged_at"] = json!(at);
                // A prompt echo is not the reader's judgment.
                reply(
                    &repo,
                    &format!("{}: approved", value["items"][0]["key"].as_str().unwrap()),
                    EARLY,
                    "user",
                );
                ingest(&repo, &value, audit);
                let shown = show(&repo, "declared");
                assert_eq!(judged(&shown, audit)["clears"], clears);
                assert_eq!(judged(&shown, audit)["basis_source"], "declared");
                assert_eq!(judged(&shown, audit)["basis_version"], version);
                assert_eq!(read(&shown, audit)["basis_time"], at);
            }
        }
    }

    /// A valid reply outranks the declaration; a template issued before a
    /// version and judged at its effective instant uses that version.
    #[test]
    fn basis_reply_takes_precedence_over_declared_and_issued_times() {
        for at in [EARLY, LATE] {
            let repo = dated_repo();
            let mut value = receipt(&repo, "precedence", true);
            value["items"][0]["judged_at"] = json!(if at == EARLY { LATE } else { EARLY });
            let text = json!({"key":value["items"][0]["key"], "verdict":"approved"}).to_string();
            reply(&repo, &text, at, "assistant");
            ingest(&repo, &value, true);
            let shown = show(&repo, "precedence");
            assert_eq!(judged(&shown, true)["basis_source"], "session-reply");
            assert_eq!(
                judged(&shown, true)["basis_version"],
                if at == EARLY { "v1" } else { "v2" }
            );
            assert_eq!(judged(&shown, true)["clears"], at == LATE);
        }
    }

    /// Out-of-window declarations refuse the item without a read event,
    /// even when a matching session reply exists.
    #[test]
    fn basis_invalid_declarations_refuse_without_a_read() {
        for audit in [false, true] {
            for at in [
                "2026-08-31T23:59:59.999999999Z",
                "2999-01-01T00:00:00Z",
                "not-a-time",
            ] {
                let repo = dated_repo();
                let mut value = receipt(&repo, "invalid-time", audit);
                value["items"][0]["judged_at"] = json!(at);
                reply(
                    &repo,
                    &format!("{}: approved", value["items"][0]["key"].as_str().unwrap()),
                    EARLY,
                    "assistant",
                );
                let path = repo.home.join("invalid.json");
                fs::write(&path, value.to_string()).unwrap();
                let output = repo
                    .arc(&repo.root)
                    .env("CODEX_HOME", repo.home.join("codex-state"))
                    .args([
                        if audit { "audit" } else { "review" },
                        "--receipt",
                        path.to_str().unwrap(),
                        "--json",
                    ])
                    .output()
                    .unwrap();
                assert!(!output.status.success());
                let result: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(result["items"][0]["code"], "judged-at", "{result:#}");
                let shown = show(&repo, "invalid-time");
                assert!(
                    shown[if audit { "audit_verdicts" } else { "verdicts" }]
                        .as_array()
                        .is_none_or(Vec::is_empty),
                    "{shown:#}"
                );
            }
        }
    }

    /// A reply outside the receipt's window or a judgment of another item
    /// cannot backdate the read. An absent match never waits for session completion.
    #[test]
    fn basis_reply_requires_an_exact_item_judgment_in_the_window() {
        for (text, at) in [
            ("wrong-key", EARLY),
            ("wrong-verdict", EARLY),
            ("matching", "2999-01-01T00:00:00Z"),
            ("matching", "2026-08-31T23:59:59Z"),
        ] {
            let repo = dated_repo();
            let mut value = receipt(&repo, "matching", true);
            value["items"][0]["judged_at"] = json!(LATE);
            let key = value["items"][0]["key"].as_str().unwrap();
            let text = match text {
                "wrong-key" => format!("{key}-other: approved"),
                "wrong-verdict" => format!("{key}: changes-requested"),
                _ => format!("{key}: approved"),
            };
            reply(&repo, &text, at, "assistant");
            ingest(&repo, &value, true);
            let shown = show(&repo, "matching");
            assert_eq!(judged(&shown, true)["basis_source"], "declared");
            assert_eq!(judged(&shown, true)["basis_version"], "v2");
        }
    }

    /// Transport keeps the recorder's version as evidence; the receiver applies
    /// its own table at the persisted instant, not its current-time table.
    #[test]
    fn basis_transport_preserves_evidence_and_uses_receiver_history() {
        for audit in [false, true] {
            let source = dated_repo();
            let value = receipt(&source, "transport-time", audit);
            reply(
                &source,
                &format!("{}: approved", value["items"][0]["key"].as_str().unwrap()),
                EARLY,
                "assistant",
            );
            ingest(&source, &value, audit);
            if !audit {
                source
                    .arc(&source.root)
                    .args(["integrate", "transport-time", "--debt", "fixture unread"])
                    .assert()
                    .success();
            }
            let original = show(&source, "transport-time");
            let bundle = source.home.join("basis-bundle.json");
            source
                .arc(&source.root)
                .args(["export", "transport-time", "--output"])
                .arg(&bundle)
                .assert()
                .success();
            let value: Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
            let receiver = dated_repo();
            let path = receiver.root.join(".arc/review-ladder.toml");
            let mut own: Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            own["version"][0]["id"] = json!("receiver-v1");
            own["version"][1]["id"] = json!("receiver-v2");
            own["version"][0]["model"][1]["efforts"][1]["rank"] = json!(4);
            own["version"][1]["model"][1]["efforts"][1]["rank"] = json!(1);
            fs::write(&path, toml::to_string(&own).unwrap()).unwrap();
            git(&receiver.root, &["add", ".arc/review-ladder.toml"]);
            git(&receiver.root, &["commit", "-m", "test: receiver history"]);
            deliver_git_objects(&source, &receiver);
            let publisher = value["source_replica_id"]
                .as_str()
                .unwrap_or_else(|| value["repository_id"].as_str().unwrap());
            let origin = value["repository_id"].as_str().unwrap();
            let reader = format!("codex:{SESSION}:{FABLE}");
            receiver.declare_gates_locally(&format!(
                "[replica]\ntrusted_review_replicas=['{origin}']\n[[replica.publishers]]\nprincipal='channel'\nsource_replica_id='{publisher}'\n[[replica.publishers.origins]]\nreplica_id='{origin}'\nroles=['facts','review-authority','debt-input','integration']\nactors=['tester','Writer','{reader}']\n"));
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
            let shown = show(&receiver, "transport-time");
            for field in ["basis_time", "basis_source", "basis_version"] {
                assert_eq!(
                    read(&shown, audit)[field],
                    read(&original, audit)[field],
                    "{field}"
                );
            }
            assert_eq!(read(&shown, audit)["basis_version"], "v1");
            assert_eq!(judged(&shown, audit)["basis_version"], "receiver-v1");
            assert_eq!(judged(&shown, audit)["clears"], true);
        }
    }

    /// Direct reads persist their clock. Legacy events without basis fields
    /// use created_at with an explicit inferred label and do not reread sessions.
    #[test]
    fn basis_direct_and_legacy_reads_distinguish_recorded_from_inferred() {
        let repo = dated_repo();
        owed(&repo, "legacy-time", Some(SOL), "");
        audit(&repo, "legacy-time", Some(FABLE), None);
        let shown = show(&repo, "legacy-time");
        assert_eq!(judged(&shown, true)["basis_source"], "recorded");
        assert_eq!(
            read(&shown, true)["basis_time"],
            read(&shown, true)["created_at"]
        );
        assert_eq!(read(&shown, true)["basis_version"], "v2");
        let file = repo
            .root
            .join(".git/arc/changes")
            .join(shown["change_id"].as_str().unwrap())
            .join("events")
            .join(format!(
                "{}.json",
                read(&shown, true)["event_id"].as_str().unwrap()
            ));
        let mut event: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        for field in ["basis_time", "basis_source", "basis_version"] {
            event.as_object_mut().unwrap().remove(field).unwrap();
        }
        fs::write(&file, event.to_string()).unwrap();
        let legacy = show(&repo, "legacy-time");
        assert_eq!(judged(&legacy, true)["basis_source"], "inferred");
        assert_eq!(
            judged(&legacy, true)["basis_time"],
            read(&shown, true)["created_at"]
        );
        assert_eq!(judged(&legacy, true)["basis_version"], "v2");
        assert!(read(&legacy, true)["basis_time"].is_null());
        let text = stdout(repo.arc(&repo.root).args(["show", "legacy-time"]));
        assert!(text.contains("inferred, version v2"), "{text}");
    }

    /// A matching reply anywhere in a retained recording supplies the basis.
    /// Rejects silently falling back when the reply precedes the normal tail window.
    #[test]
    fn basis_reply_before_the_session_tail_still_dates_the_read() {
        let repo = dated_repo();
        let value = receipt(&repo, "long-session", true);
        reply(
            &repo,
            &format!("{}: approved", value["items"][0]["key"].as_str().unwrap()),
            EARLY,
            "assistant",
        );
        let day = repo.home.join("codex-state/sessions/2026/07/24");
        let path = day.join(format!("rollout-2026-07-24T00-00-00-{SESSION}.jsonl"));
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        let row = json!({"type":"response_item","timestamp":LATE,"payload":{"type":"function_call_output","call_id":"padding","output":"x".repeat(128 * 1024)}});
        for _ in 0..40 {
            writeln!(file, "{row}").unwrap();
        }
        drop(file);
        ingest(&repo, &value, true);
        let shown = show(&repo, "long-session");
        assert_eq!(judged(&shown, true)["clears"], false, "{shown:#}");
        assert_eq!(judged(&shown, true)["basis_source"], "session-reply");
        assert_eq!(read(&shown, true)["basis_time"], EARLY);
    }
}
