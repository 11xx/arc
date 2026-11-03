//! `[policy] review_clearance = "assessed"`: a read clears only when its
//! agent subject carries a bound assessment that says it clears; every other
//! agent read records findings and leaves the review owed.

use super::common::*;
use super::ineligible_review::{assessment, assessment_v2, audit_v2, debt_of, snapshotted};

const ASSESSED: &str = "[policy]\nforbid_self_approval = true\nreview_clearance = \"assessed\"\n";

pub(super) fn repo_with_policy(policy: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

pub(super) fn assessed_repo() -> Repo {
    repo_with_policy(ASSESSED)
}

/// A change that integrated without a review, owing one.
fn owed(repo: &Repo, slug: &str) {
    snapshotted(repo, slug);
    repo.arc(&repo.root)
        .args(["integrate", slug, "--debt", "nobody read it"])
        .assert()
        .success();
}

fn audit(repo: &Repo, slug: &str, actor: &str, model: Option<&str>, file: Option<&Path>) -> String {
    let mut command = repo.arc(&repo.root);
    command.env("ARC_ACTOR", actor);
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    command.args(["audit", slug, "--verdict", "approved"]);
    if let Some(file) = file {
        command.arg("--assessment").arg(file);
    }
    stdout(&mut command)
}

fn assert_findings_read(repo: &Repo, slug: &str, output: &str, reason: &str) {
    assert!(output.contains("still owes"), "{output}");
    assert!(output.contains("findings read"), "{output}");
    assert!(
        output.contains("agent-routing assess --purpose clearance --json"),
        "{output}"
    );
    let debt = debt_of(repo, slug);
    assert!(debt["discharged_by"].is_null(), "{debt}");
    let read = &debt["findings_reads"][0];
    assert_eq!(read["kind"], "findings", "{debt}");
    assert_eq!(read["reason"], reason, "{debt}");
    assert_eq!(
        read["remedy"], "agent-routing assess --purpose clearance --json",
        "{debt}"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt_outstanding"], true, "{status}");
}

/// Claim: under the policy a `/2` read that routing says does not clear
/// leaves the debt open as a findings read naming the remedy. Rejects
/// discharge by `independence: eligible` alone, which is what the same audit
/// does without the policy.
#[test]
fn a_non_clearing_second_schema_audit_is_a_findings_read() {
    let repo = assessed_repo();
    owed(&repo, "finds");
    let file = assessment_v2(&repo, "finds.json", "eligible", false, &["below-clearance"]);
    let output = audit_v2(&repo, "finds", &file);
    assert_findings_read(&repo, "finds", &output, "not-clearing");
    let read = &debt_of(&repo, "finds")["findings_reads"][0];
    assert_eq!(read["qualifiers"], serde_json::json!(["below-clearance"]));
}

/// Claim: a bound `/2` assessment that clears discharges under the policy.
/// Rejects a policy that refuses every agent read.
#[test]
fn a_bound_clearing_audit_discharges() {
    let repo = assessed_repo();
    owed(&repo, "clears");
    let file = assessment_v2(&repo, "clears.json", "eligible", true, &[]);
    let output = audit_v2(&repo, "clears", &file);
    assert!(output.contains("debt discharged"), "{output}");
    let debt = debt_of(&repo, "clears");
    assert!(debt.get("findings_reads").is_none(), "{debt}");
}

/// Claim: a `/1` assessment answers no clearance, so under the policy it
/// never clears. Rejects reading `independence: eligible` as clearance.
#[test]
fn a_first_schema_assessment_never_clears() {
    let repo = assessed_repo();
    owed(&repo, "first");
    let file = assessment(&repo, "first.json", "eligible", &[]);
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    value["reviewer"] = serde_json::json!({"identities": ["fixture-other"], "effort": "high"});
    fs::write(&file, value.to_string()).unwrap();
    let output = audit(
        &repo,
        "first",
        "Auditor",
        Some("fixture-other#high"),
        Some(&file),
    );
    assert_findings_read(&repo, "first", &output, "assessment-without-clearance");
}

/// Claim: under the policy an agent's audit without an assessment is a
/// findings read. Rejects discharge by authorship alone, which the same
/// audit gets without the policy.
#[test]
fn an_audit_without_an_assessment_is_a_findings_read() {
    let repo = assessed_repo();
    owed(&repo, "bare");
    let output = audit(&repo, "bare", "Auditor", Some("fixture-other#high"), None);
    assert_findings_read(&repo, "bare", &output, "no-assessment");

    let legacy = repo_with_policy("[policy]\nforbid_self_approval = true\n");
    owed(&legacy, "bare");
    let output = audit(&legacy, "bare", "Auditor", Some("fixture-other#high"), None);
    assert!(output.contains("debt discharged"), "{output}");
}

/// Claim: an agent subject without a recorded model cannot clear, and an
/// auditor whose model names no effort does not bind a clearing assessment.
/// Rejects clearance on an identity routing never assessed.
#[test]
fn an_agent_without_a_model_or_effort_is_a_findings_read() {
    let repo = assessed_repo();
    owed(&repo, "nomodel");
    let output = audit(&repo, "nomodel", "Auditor", None, None);
    assert_findings_read(&repo, "nomodel", &output, "model-unrecorded");

    owed(&repo, "noeffort");
    let file = assessment_v2(&repo, "noeffort.json", "eligible", true, &[]);
    let output = audit(
        &repo,
        "noeffort",
        "Auditor",
        Some("fixture-other"),
        Some(&file),
    );
    assert_findings_read(&repo, "noeffort", &output, "effort-unrecorded");
}

/// Claim: a person subject is judged by authorship alone under the policy,
/// because routing seats no people. Rejects a person's independent audit
/// being held as a findings read for want of an assessment.
#[test]
fn a_person_audit_is_judged_by_authorship_alone() {
    let repo = assessed_repo();
    owed(&repo, "person");
    let output = audit(
        &repo,
        "person",
        "Pat Reviewer <pat@example.com>",
        None,
        None,
    );
    assert!(output.contains("debt discharged"), "{output}");
}

/// Claim: a comment-only audit by a clearing seat still discharges nothing,
/// and a changes-requested one by the same seat fulfils the review without
/// approving it. Rejects clearance turning a comment into a discharge.
#[test]
fn a_comment_only_audit_by_a_clearing_seat_still_does_not_discharge() {
    let repo = assessed_repo();
    owed(&repo, "comment");
    let file = assessment_v2(&repo, "comment.json", "eligible", true, &[]);
    let record = |verdict: &str| {
        stdout(
            repo.arc(&repo.root)
                .env("ARC_ACTOR", "Auditor")
                .args(["--model", "fixture-other#high"])
                .args(["audit", "comment", "--verdict", verdict, "--assessment"])
                .arg(&file),
        )
    };
    let output = record("comment-only");
    assert!(output.contains("comment-only"), "{output}");
    let debt = debt_of(&repo, "comment");
    assert!(debt["discharged_by"].is_null(), "{debt}");
    assert!(debt.get("findings_reads").is_none(), "{debt}");

    let output = record("changes-requested");
    assert!(output.contains("debt discharged"), "{output}");
    let explain = stdout(repo.arc(&repo.root).args(["explain", "comment"]));
    assert!(explain.contains("fulfilled, not approved"), "{explain}");
}

/// Claim: the stricter reading wins: the operator policy alone turns the
/// rule on, and a project file that does not name the key cannot turn it
/// off. Rejects a key honoured only in `.arc/policy.toml`.
#[test]
fn the_operator_policy_alone_turns_the_rule_on() {
    let repo = repo_with_policy("[policy]\nforbid_self_approval = true\n");
    let operator = repo.root.join(".git/arc/operator-policy.toml");
    owed(&repo, "operator");
    fs::write(&operator, "[policy]\nreview_clearance = \"assessed\"\n").unwrap();
    let output = audit(
        &repo,
        "operator",
        "Auditor",
        Some("fixture-other#high"),
        None,
    );
    assert_findings_read(&repo, "operator", &output, "no-assessment");
}

/// Claim: `review_clearance` accepts only `assessed`. Rejects a typo read as
/// the key's absence, which would silently keep the weaker rule.
#[test]
fn an_unknown_clearance_value_is_refused() {
    let repo = repo_with_policy("[policy]\nreview_clearance = \"assesed\"\n");
    repo.arc(&repo.root)
        .args(["begin", "typo"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("review_clearance"));
}
