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
    let show = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    assert_eq!(
        show["debt"]["findings_reads"][0]["kind"], "findings",
        "{show}"
    );
    let text = stdout(repo.arc(&repo.root).args(["show", slug]));
    assert!(text.contains("findings read"), "{text}");
    assert!(text.contains(reason), "{text}");
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

/// Claim: the policy only tightens: a person's audit, which clears ordinary
/// debt on authorship, still leaves an `ineligible-review` debt owed, as it
/// does without the policy. Rejects the policy discharging what the legacy
/// rule kept open.
#[test]
fn a_person_audit_leaves_an_ineligible_review_debt_owed() {
    let repo = assessed_repo();
    snapshotted(&repo, "pdebt");
    approve(&repo, "pdebt", None);
    repo.arc(&repo.root)
        .args([
            "integrate",
            "pdebt",
            "--debt",
            "a findings seat approved it",
        ])
        .assert()
        .success();
    assert_eq!(debt_of(&repo, "pdebt")["missing"], "ineligible-review");
    let output = audit(&repo, "pdebt", "Pat Reviewer <pat@example.com>", None, None);
    assert!(!output.contains("debt discharged"), "{output}");
    assert_eq!(debt_of(&repo, "pdebt")["standing"], "owed");
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

/// The head of `slug`'s change branch.
fn change_head(repo: &Repo, slug: &str) -> String {
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.head(&worktree)
}

fn event_line(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap_or_else(|| panic!("no event line in {output}"))
        .trim()
        .to_string()
}

/// An approving verdict on `slug` by Reviewer at `fixture-other#high`, under
/// the assessment at `file` when one is given.
fn approve(repo: &Repo, slug: &str, file: Option<&Path>) -> String {
    let mut command = repo.arc(&repo.root);
    command
        .env("ARC_ACTOR", "Reviewer")
        .args(["--model", "fixture-other#high"])
        .args(["review", slug, "--verdict", "approved"]);
    if let Some(file) = file {
        command.arg("--assessment").arg(file);
    }
    event_line(&stdout(&mut command))
}

fn status_of(repo: &Repo, slug: &str) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]))
}

/// Claim: under the policy an approval that does not clear satisfies no
/// integration gate, and `--debt` lands it owing an `ineligible-review` debt
/// that names the read. Rejects a findings seat approving a landing that owes
/// nothing.
#[test]
fn a_findings_only_approval_cannot_integrate_without_debt() {
    let repo = assessed_repo();
    snapshotted(&repo, "findings");
    let read = approve(&repo, "findings", None);
    let status = status_of(&repo, "findings");
    let reason = status["approval_rejection_reason"]
        .as_str()
        .unwrap_or_default();
    assert!(reason.contains("findings read"), "{status}");
    let check = json_stdout_any_status(repo.arc(&repo.root).args(["check", "findings", "--json"]));
    let assessed = &check["verdict_assessments"][0];
    assert_eq!(assessed["kind"], "findings", "{check}");
    assert_eq!(assessed["findings_reason"], "no-assessment", "{check}");
    repo.arc(&repo.root)
        .args(["integrate", "findings"])
        .assert()
        .failure();

    repo.arc(&repo.root)
        .args([
            "integrate",
            "findings",
            "--debt",
            "a findings seat approved it",
        ])
        .assert()
        .success();
    let debt = debt_of(&repo, "findings");
    assert_eq!(debt["missing"], "ineligible-review", "{debt}");
    assert_eq!(debt["ineligible"]["read"], read.as_str(), "{debt}");
    assert_eq!(debt["ineligible"]["findings"], "no-assessment", "{debt}");
    assert_eq!(debt["standing"], "owed", "{debt}");
}

/// Claim: a findings-only approval waived by a landing keeps the same read,
/// reason, qualifiers and assessment basis as `integrate --debt`. Rejects a
/// completion that keeps the debt kind but drops the read that caused it.
#[test]
fn a_landing_keeps_the_same_ineligible_read_as_integrate_debt() {
    for assessed in [false, true] {
        let mut debts = Vec::new();
        for command in ["integrate", "land"] {
            let repo = assessed_repo();
            repo.redeclare_gates("[gates.smoke]\ncommand = \"true\"\n");
            snapshotted(&repo, "findings");
            let file = assessed.then(|| {
                assessment_v2(&repo, "read.json", "eligible", false, &["below-clearance"])
            });
            let read = approve(&repo, "findings", file.as_deref());
            repo.arc(&repo.root)
                .args(["verify", "findings", "--all"])
                .assert()
                .success();
            repo.arc(&repo.root)
                .args([
                    command,
                    "findings",
                    "--debt",
                    "findings read needs clearance",
                ])
                .assert()
                .success();
            let debt = debt_of(&repo, "findings");
            assert_eq!(debt["missing"], "ineligible-review", "{command}: {debt}");
            assert_eq!(debt["ineligible"]["read"], read, "{command}: {debt}");
            let mut ineligible = debt["ineligible"].clone();
            ineligible.as_object_mut().unwrap().remove("read");
            debts.push(ineligible);
        }
        assert_eq!(debts[0], debts[1]);
        assert_eq!(
            debts[1]["findings"],
            if assessed {
                "not-clearing"
            } else {
                "no-assessment"
            }
        );
        assert_eq!(
            debts[1]["assessment"],
            if assessed { "assessed" } else { "unassessed" }
        );
        assert_eq!(
            debts[1]["qualifiers"],
            if assessed {
                serde_json::json!(["below-clearance"])
            } else {
                serde_json::json!([])
            }
        );
        if assessed {
            assert_eq!(debts[1]["route_version"], "2026.11.3");
        }
    }
}

/// Claim: an external verdict carries no assessment, so under the policy it
/// is a findings read: it lands only with `--debt`, which names it. Rejects
/// an external approval supplying a clearance-free, debt-free landing.
#[test]
fn an_external_approval_cannot_integrate_without_debt() {
    // Without `forbid_self_approval`, an external approval alone gates this
    // surface; only the clearance policy can refuse it.
    let repo = repo_with_policy("[policy]\nreview_clearance = \"assessed\"\n");
    snapshotted(&repo, "external");
    let head = change_head(&repo, "external");
    let read = event_line(&stdout(repo.arc(&repo.root).args([
        "external",
        "verdict",
        "external",
        "--verdict",
        "approved",
        "--decided-by",
        "Upstream Maintainer",
        "--reference",
        "https://example.invalid/pr/1",
        "--revision",
        &head,
    ])));
    let status = status_of(&repo, "external");
    assert_eq!(status["has_valid_approval"], false, "{status}");
    repo.arc(&repo.root)
        .args(["integrate", "external"])
        .assert()
        .failure();

    repo.arc(&repo.root)
        .args(["integrate", "external", "--debt", "decided upstream"])
        .assert()
        .success();
    let debt = debt_of(&repo, "external");
    assert_eq!(debt["missing"], "ineligible-review", "{debt}");
    assert_eq!(debt["ineligible"]["read"], read.as_str(), "{debt}");
}

/// Claim: a waiver never lifts a refusal under the policy. Rejects `--debt`
/// landing a change whose current verdict requests changes.
#[test]
fn a_debt_never_lands_over_a_refusal() {
    let repo = assessed_repo();
    snapshotted(&repo, "refused");
    approve(&repo, "refused", None);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Pat Reviewer <pat@example.com>")
        .args(["review", "refused", "--verdict", "changes-requested"])
        .args(["--cause", "executor"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "refused", "--debt", "owed anyway"])
        .assert()
        .failure();
    assert_eq!(status_of(&repo, "refused")["state"], "open");
}

/// Claim: a bound clearing verdict recorded with `arc review --assessment`
/// gates integration under the policy and leaves nothing owed; so does a
/// person's approval, judged by authorship alone. Rejects a policy that
/// leaves no way to land without debt.
#[test]
fn a_clearing_approval_integrates_without_debt() {
    let repo = assessed_repo();
    snapshotted(&repo, "cleared");
    let file = assessment_v2(&repo, "cleared.json", "eligible", true, &[]);
    approve(&repo, "cleared", Some(&file));
    let status = status_of(&repo, "cleared");
    assert_eq!(status["has_valid_approval"], true, "{status}");
    assert_eq!(
        status["verdict_assessments"][0]["kind"], "clearing",
        "{status}"
    );
    repo.arc(&repo.root)
        .args(["integrate", "cleared"])
        .assert()
        .success();
    assert!(debt_of(&repo, "cleared").is_null());

    snapshotted(&repo, "person");
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Pat Reviewer <pat@example.com>")
        .args(["review", "person", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "person"])
        .assert()
        .success();
}

/// Claim: `arc review --assessment` validates the file before writing and
/// keeps the clearance answer on the verdict. Rejects a malformed assessment
/// recorded as a verdict nobody can bind.
#[test]
fn review_keeps_and_validates_its_assessment() {
    let repo = assessed_repo();
    snapshotted(&repo, "kept");
    let bad = repo.home.join("bad.json");
    fs::write(&bad, "{}").unwrap();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["review", "kept", "--verdict", "approved", "--assessment"])
        .arg(&bad)
        .assert()
        .failure();
    assert_eq!(status_of(&repo, "kept")["verdict"], serde_json::Value::Null);
    let file = assessment_v2(&repo, "kept.json", "eligible", false, &["below-clearance"]);
    approve(&repo, "kept", Some(&file));
    let status = status_of(&repo, "kept");
    let kept = &status["verdict_assessments"][0];
    assert_eq!(kept["kind"], "findings", "{status}");
    assert_eq!(kept["findings_reason"], "not-clearing", "{status}");
}

/// Claim: `arc doctor` lists, as advice, each debt the policy leaves owed
/// that would be discharged without it, naming the read. Rejects a policy
/// switch that silently reopens debt nobody is told about.
#[test]
fn doctor_lists_the_debts_the_policy_leaves_owed() {
    let repo = assessed_repo();
    owed(&repo, "reopened");
    let output = audit(
        &repo,
        "reopened",
        "Auditor",
        Some("fixture-other#high"),
        None,
    );
    let read = event_line(&output);
    let doctor = json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]));
    let advice = doctor["advice"].as_array().unwrap();
    let entry = advice
        .iter()
        .find(|finding| finding["code"] == "clearance-keeps-debt-owed")
        .unwrap_or_else(|| panic!("no clearance advice in {doctor}"));
    let detail = entry["detail"].as_str().unwrap();
    assert!(detail.contains("reopened"), "{detail}");
    assert!(detail.contains(&read), "{detail}");
    assert!(
        !doctor["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "clearance-keeps-debt-owed"),
        "advice, never a problem: {doctor}"
    );

    owed(&repo, "cleared");
    let file = assessment_v2(&repo, "doctor.json", "eligible", true, &[]);
    audit_v2(&repo, "cleared", &file);
    let doctor = json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]));
    let named: Vec<_> = doctor["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "clearance-keeps-debt-owed")
        .filter(|finding| finding["detail"].as_str().unwrap().contains("cleared-"))
        .collect();
    assert!(named.is_empty(), "a cleared debt is not reopened: {doctor}");
}
