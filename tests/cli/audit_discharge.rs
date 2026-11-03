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

/// Claim: only an approving audit discharges review debt, and `arc audit`
/// says so. Rejects a `comment-only` or `changes-requested` audit by an
/// independent reader printing `debt discharged` and settling the debt.
#[test]
fn a_read_that_does_not_approve_leaves_the_debt_owed_and_says_why() {
    for verdict in ["comment-only", "changes-requested"] {
        let repo = repo_forbidding_self_approval();
        integrated_owing(&repo, "unread", &[]);
        let output = audit(&repo, "unread", "Auditor", verdict, &[]);
        assert!(!output.contains("debt discharged"), "{verdict}: {output}");
        assert!(
            output.contains("the debt still owes"),
            "{verdict}: {output}"
        );
        assert!(
            output.contains(&format!("a {verdict} read discharges no debt")),
            "{verdict}: {output}"
        );
        assert_owed(&repo, "unread", &event_of(&output));

        let output = audit(&repo, "unread", "Auditor", "approved", &[]);
        assert!(output.contains("debt discharged"), "{output}");
        let debt = debt_of(&repo, "unread");
        assert_eq!(debt["standing"], "discharged", "{debt}");
        assert_eq!(debt["discharged_by_event_id"], event_of(&output), "{debt}");
    }
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
