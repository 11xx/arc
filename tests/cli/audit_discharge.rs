use super::common::*;
use super::ineligible_review::{assessment, debt_of, repo_forbidding_self_approval, snapshotted};

const AUDITOR_MODEL: &str = "fixture-other#high";

/// A snapshotted change, integrated with nothing read and `extra` debt flags.
fn integrated_owing(repo: &Repo, slug: &str, extra: &[&str]) {
    snapshotted(repo, slug);
    repo.arc(&repo.root)
        .args(["integrate", slug, "--debt", "no reviewer reachable"])
        .args(extra)
        .assert()
        .success();
}

/// Audit `slug` as `actor` at the independent model, returning stdout.
fn audit(repo: &Repo, slug: &str, actor: &str, verdict: &str, extra: &[&str]) -> String {
    stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", actor)
            .args([
                "--model",
                AUDITOR_MODEL,
                "audit",
                slug,
                "--verdict",
                verdict,
            ])
            .args(extra),
    )
}

fn event_of(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap_or_else(|| panic!("no event line in {output}"))
        .trim()
        .to_string()
}

/// The debt is owed in every view that carries it, beside the read.
fn assert_owed(repo: &Repo, slug: &str, read: &str) {
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt"]["standing"], "owed", "{status}");
    assert_eq!(status["debt_outstanding"], true, "{status}");
    assert!(status["debt"]["discharged_by"].is_null(), "{status}");
    assert!(
        status["audit_verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|audit| audit["event_id"] == read),
        "the read stays recorded: {status}"
    );
    assert!(
        stdout(repo.arc(&repo.root).args(["query", "--debt"])).contains(slug),
        "still queued"
    );
}

/// Claim: a `comment-only` audit discharges no review debt and `arc audit`
/// says why, while a `changes-requested` audit is the review the debt owed.
/// Rejects a read that judged nothing settling the debt, and a review that
/// found defects leaving it owed for good.
#[test]
fn a_comment_only_read_leaves_the_debt_owed_and_a_refusing_read_discharges_it() {
    let repo = repo_forbidding_self_approval();
    integrated_owing(&repo, "unread", &[]);
    let output = audit(&repo, "unread", "Auditor", "comment-only", &[]);
    assert!(!output.contains("debt discharged"), "{output}");
    assert!(output.contains("the debt still owes"), "{output}");
    assert!(
        output.contains("neither approves nor requests changes"),
        "{output}"
    );
    assert_owed(&repo, "unread", &event_of(&output));

    let output = audit(&repo, "unread", "Auditor", "changes-requested", &[]);
    assert!(output.contains("debt discharged"), "{output}");
    let debt = debt_of(&repo, "unread");
    assert_eq!(debt["standing"], "discharged", "{debt}");
    assert_eq!(debt["discharged_by_event_id"], event_of(&output), "{debt}");
}

/// Claim: an audit routing assessed `ineligible` discharges no kind of debt,
/// and `arc audit` names the assessment as the reason. Rejects an approving
/// audit under a distinct actor clearing the debt despite a bound
/// `same-model`/`same-family` assessment saying it does not count.
#[test]
fn an_ineligible_assessment_leaves_every_kind_owed() {
    for kind in [None, Some("independent-review"), Some("contributor-only")] {
        let repo = repo_forbidding_self_approval();
        let extra: Vec<&str> = kind.map(|kind| vec!["--kind", kind]).unwrap_or_default();
        integrated_owing(&repo, "assessed", &extra);
        let file = assessment(
            &repo,
            "assessed.json",
            "ineligible",
            &["same-model", "same-family"],
        );
        let output = audit(
            &repo,
            "assessed",
            "Auditor",
            "approved",
            &["--assessment", file.to_str().unwrap()],
        );
        assert!(!output.contains("debt discharged"), "{kind:?}: {output}");
        assert!(output.contains("the debt still owes"), "{kind:?}: {output}");
        assert!(
            output.contains("routing assessed this auditor ineligible (same-model, same-family)"),
            "{kind:?}: {output}"
        );
        assert_owed(&repo, "assessed", &event_of(&output));

        // Without an assessment the actor and model rule decides, as before.
        let output = audit(&repo, "assessed", "Second", "approved", &[]);
        assert!(output.contains("debt discharged"), "{kind:?}: {output}");
        assert_eq!(debt_of(&repo, "assessed")["standing"], "discharged");
    }
}

/// Claim: a debt already discharged is not re-announced by every later audit.
/// Rejects a non-approving audit printing `debt discharged` when an earlier
/// read settled the debt.
#[test]
fn a_later_audit_names_the_read_that_discharged_the_debt() {
    let repo = repo_forbidding_self_approval();
    integrated_owing(&repo, "settled", &[]);
    let first = event_of(&audit(&repo, "settled", "Auditor", "approved", &[]));
    let output = audit(&repo, "settled", "Second", "comment-only", &[]);
    assert!(!output.contains("debt discharged"), "{output}");
    assert!(
        output.contains(&format!("debt already discharged by {first}")),
        "{output}"
    );
}

/// Claim: an ineligible post-integration audit can be the read an
/// `ineligible-review` debt names, by `--read` or implicitly when the shipped
/// patchset has no verdict. Rejects `arc debt` refusing an audit as the read,
/// which left a lane only the broad debt kind or a misattributed verdict.
#[test]
fn an_ineligible_review_debt_names_a_post_integration_audit() {
    let repo = repo_forbidding_self_approval();
    integrated_owing(&repo, "late-read", &[]);
    let file = assessment(&repo, "late.json", "ineligible", &["same-model"]);
    let read = event_of(&audit(
        &repo,
        "late-read",
        "Auditor",
        "approved",
        &["--assessment", file.to_str().unwrap()],
    ));
    repo.arc(&repo.root)
        .args(["debt", "late-read", "--reason", "same-model read only"])
        .args(["--kind", "ineligible-review", "--read", &read])
        .args(["--assessment", file.to_str().unwrap()])
        .assert()
        .success();
    let debt = debt_of(&repo, "late-read");
    assert_eq!(debt["missing"], "ineligible-review", "{debt}");
    assert_eq!(debt["standing"], "owed", "{debt}");
    assert_eq!(debt["ineligible"]["read"], read.as_str(), "{debt}");
    assert_eq!(debt["ineligible"]["reviewer"], "Auditor", "{debt}");
    assert_eq!(
        debt["ineligible"]["reviewer_model"], AUDITOR_MODEL,
        "{debt}"
    );
    assert_eq!(
        debt["ineligible"]["qualifiers"],
        serde_json::json!(["same-model"]),
        "{debt}"
    );

    // An unknown read is still refused, and nothing is written.
    repo.arc(&repo.root)
        .args(["debt", "late-read", "--reason", "named elsewhere"])
        .args(["--kind", "ineligible-review", "--read", "01NOTAREAD"])
        .args(["--qualifier", "same-family"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is not a verdict or audit"));
    assert_eq!(
        debt_of(&repo, "late-read")["ineligible"]["read"],
        read.as_str()
    );

    // Omitted, the read is the latest audit when no verdict exists.
    let second = event_of(&audit(
        &repo,
        "late-read",
        "Second",
        "changes-requested",
        &["--assessment", file.to_str().unwrap()],
    ));
    repo.arc(&repo.root)
        .args(["debt", "late-read", "--reason", "same-model reads only"])
        .args(["--kind", "ineligible-review", "--qualifier", "same-model"])
        .assert()
        .success();
    assert_eq!(
        debt_of(&repo, "late-read")["ineligible"]["read"],
        second.as_str()
    );
}

/// Claim: an independent refusing audit with an eligible, bound assessment
/// discharges ineligible-review debt, and the remedy offers that read.
/// Rejects requiring approval after a review found defects, or accepting
/// a comment-only, unassessed or ineligible read as its substitute.
#[test]
fn eligible_refusing_audit_discharges_ineligible_review() {
    let repo = repo_forbidding_self_approval();
    super::ineligible_review::read_change(&repo, "ineligible");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "ineligible",
            "--debt",
            "same-model read only",
            "--kind",
            "ineligible-review",
            "--qualifier",
            "same-model",
        ])
        .assert()
        .success();
    let file =
        super::ineligible_review::assessment_v2(&repo, "eligible.json", "eligible", true, &[]);
    let output = audit(
        &repo,
        "ineligible",
        "Auditor",
        "comment-only",
        &["--assessment", file.to_str().unwrap()],
    );
    assert_owed(&repo, "ineligible", &event_of(&output));
    assert!(
        output.contains("an audit that approves or requests changes"),
        "{output}"
    );
    for (topic, remedy) in [
        (
            "debt-obligations",
            "a read recorded since the declaration that approves or requests changes",
        ),
        (
            "debt-recovery",
            "that approves it or requests changes discharges it",
        ),
    ] {
        let guide = stdout(repo.arc(&repo.root).args(["guide", topic]));
        let prose = guide.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(prose.contains(remedy), "{topic}: {guide}");
    }

    let output = audit(&repo, "ineligible", "Auditor", "changes-requested", &[]);
    assert_owed(&repo, "ineligible", &event_of(&output));
    let ineligible = assessment(&repo, "ineligible.json", "ineligible", &["same-model"]);
    let output = audit(
        &repo,
        "ineligible",
        "Auditor",
        "changes-requested",
        &["--assessment", ineligible.to_str().unwrap()],
    );
    assert_owed(&repo, "ineligible", &event_of(&output));
    let output = audit(
        &repo,
        "ineligible",
        "tester",
        "changes-requested",
        &["--assessment", file.to_str().unwrap()],
    );
    assert_owed(&repo, "ineligible", &event_of(&output));
    let output = audit(
        &repo,
        "ineligible",
        "Auditor",
        "changes-requested",
        &["--assessment", file.to_str().unwrap()],
    );
    assert!(output.contains("debt discharged"), "{output}");
    let debt = debt_of(&repo, "ineligible");
    assert_eq!(debt["standing"], "discharged", "{debt}");
    assert_eq!(debt["discharged_by_event_id"], event_of(&output), "{debt}");
    assert!(!stdout(repo.arc(&repo.root).args(["query", "--debt"])).contains("ineligible"));
}
