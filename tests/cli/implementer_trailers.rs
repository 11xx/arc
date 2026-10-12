use super::common::*;

const SOL: &str = "codex:gpt-6.1-sol#medium";
const SONNET: &str = "claude:claude-sonnet-5-5#high";

fn repo_with_policy(policy: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(repo.root.join(".arc/policy.toml"), policy).unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

fn repo_forbidding_self_approval() -> Repo {
    repo_with_policy("[policy]\nforbid_self_approval = true\n")
}

/// A change whose commits carry the given `Implemented-by` values, one list
/// per commit, snapshotted by the invoking lead.
fn trailed_change(repo: &Repo, slug: &str, commits: &[&[&str]]) -> String {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    for (index, trailers) in commits.iter().enumerate() {
        let mut message = format!("feat: {slug} step {index}");
        if !trailers.is_empty() {
            message.push('\n');
            for value in *trailers {
                message.push_str(&format!("\nImplemented-by: {value}"));
            }
        }
        repo.commit(
            &worktree,
            &format!("{slug}-{index}.txt"),
            "work\n",
            &message,
        );
    }
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    change_id
}

fn integrate_with_debt(repo: &Repo, slug: &str) {
    repo.arc(&repo.root)
        .args(["integrate", slug, "--debt", "nobody looked"])
        .assert()
        .success();
}

fn debt_of(repo: &Repo, slug: &str) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]))["debt"].clone()
}

#[test]
fn production_names_the_trailer_implementers_beside_the_recorder() {
    let repo = repo_forbidding_self_approval();
    let change = trailed_change(&repo, "lead", &[&[SOL], &[SOL, SONNET]]);
    integrate_with_debt(&repo, "lead");

    let debt = debt_of(&repo, "lead");
    let production = &debt["production"];
    assert_eq!(production["implementer"]["actor"], "tester", "{debt}");
    assert_eq!(
        production["trailer_implementers"],
        serde_json::json!([SOL, SONNET]),
        "{debt}"
    );
    let note = format!("Implemented-by trailers: {SOL}, {SONNET}");
    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains(&note), "{catchup}");
    let catchup_json = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(
        catchup_json["ledger"]["debt-owed"][0]["debt_production"]["trailer_implementers"],
        serde_json::json!([SOL, SONNET]),
        "{catchup_json}"
    );
    let explain = stdout(repo.arc(&repo.root).args(["explain", &change]));
    assert!(explain.contains(&note), "{explain}");
    let show = stdout(repo.arc(&repo.root).args(["show", "lead"]));
    assert!(show.contains(&note), "{show}");
    let backlog = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(backlog.contains(&note), "{backlog}");
}

#[test]
fn a_range_without_implemented_by_lines_records_an_empty_list() {
    let repo = repo_forbidding_self_approval();
    let change = trailed_change(&repo, "bare", &[&[]]);
    integrate_with_debt(&repo, "bare");

    let debt = debt_of(&repo, "bare");
    assert_eq!(
        debt["production"]["trailer_implementers"],
        serde_json::json!([]),
        "{debt}"
    );
    for text in [
        stdout(repo.arc(&repo.root).args(["show", "bare"])),
        stdout(repo.arc(&repo.root).args(["catchup"])),
        stdout(repo.arc(&repo.root).args(["explain", &change])),
    ] {
        assert!(text.contains("no Implemented-by trailers"), "{text}");
    }
}

#[test]
fn trailers_naming_only_the_recorder_are_not_repeated() {
    let repo = repo_forbidding_self_approval();
    stdout(repo.arc(&repo.root).args(["begin", "own"]));
    let worktree = repo.home.join(".worktrees").join("repo-own");
    repo.commit(
        &worktree,
        "own.txt",
        "work\n",
        &format!("feat: own\n\nImplemented-by: {SOL}"),
    );
    stdout(
        repo.arc(&worktree)
            .args(["--harness", "codex", "--model", "gpt-6.1-sol#medium"])
            .args(["snapshot", "own"]),
    );
    integrate_with_debt(&repo, "own");

    let debt = debt_of(&repo, "own");
    assert_eq!(
        debt["production"]["trailer_implementers"],
        serde_json::json!([SOL]),
        "{debt}"
    );
    let show = stdout(repo.arc(&repo.root).args(["show", "own"]));
    assert!(!show.contains("Implemented-by trailers"), "{show}");
}

#[test]
fn a_production_record_without_the_list_reads_as_before() {
    let repo = repo_forbidding_self_approval();
    let change = trailed_change(&repo, "older", &[&[SOL]]);
    integrate_with_debt(&repo, "older");
    rewrite_event(&repo, &change, "debt-declared", |event| {
        event["production"]
            .as_object_mut()
            .unwrap()
            .remove("trailer_implementers");
    });

    let debt = debt_of(&repo, "older");
    assert!(
        debt["production"].get("trailer_implementers").is_none(),
        "{debt}"
    );
    for text in [
        stdout(repo.arc(&repo.root).args(["show", "older"])),
        stdout(repo.arc(&repo.root).args(["catchup"])),
        stdout(repo.arc(&repo.root).args(["explain", &change])),
    ] {
        assert!(text.contains("implemented by"), "{text}");
        assert!(!text.contains("Implemented-by"), "{text}");
    }
}

fn audit_as(repo: &Repo, slug: &str, model: &str, verdict: &str) -> assert_cmd::assert::Assert {
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args(["--model", model, "audit", slug, "--verdict", verdict])
        .assert()
}

#[test]
fn an_audit_by_the_implementers_model_at_another_effort_warns() {
    let repo = repo_with_policy("[policy]\nforbid_self_approval = false\n");
    trailed_change(&repo, "warned", &[&[SOL]]);
    integrate_with_debt(&repo, "warned");

    audit_as(&repo, "warned", "gpt-6.1-sol#xhigh", "approved")
        .success()
        .stderr(predicates::str::contains("Implemented-by"))
        .stderr(predicates::str::contains("gpt-6.1-sol"));
}

#[test]
fn an_audit_by_another_model_does_not_warn() {
    let repo = repo_with_policy("[policy]\nforbid_self_approval = false\n");
    trailed_change(&repo, "other", &[&[SOL]]);
    integrate_with_debt(&repo, "other");

    audit_as(&repo, "other", "fixture-opus#medium", "approved")
        .success()
        .stderr(predicates::str::contains("Implemented-by").not());
}

#[test]
fn an_audit_by_the_implementers_model_is_refused_under_the_refusing_policy() {
    let repo = repo_forbidding_self_approval();
    trailed_change(&repo, "refused", &[&[SOL]]);
    integrate_with_debt(&repo, "refused");

    audit_as(&repo, "refused", "gpt-6.1-sol#xhigh", "approved")
        .failure()
        .stderr(predicates::str::contains("Implemented-by"))
        .stderr(predicates::str::contains("--model '<model>'"));
    assert_eq!(
        debt_of(&repo, "refused")["discharged_by"],
        serde_json::Value::Null
    );

    // Raising problems needs no independence.
    audit_as(&repo, "refused", "gpt-6.1-sol#xhigh", "changes-requested").success();
    audit_as(&repo, "refused", "fixture-opus#medium", "approved").success();
}

#[test]
fn a_review_by_the_implementers_model_warns_and_cannot_gate_under_the_policy() {
    let repo = repo_forbidding_self_approval();
    trailed_change(&repo, "reviewed", &[&[SOL]]);
    let output = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["--model", "gpt-6.1-sol#xhigh"])
        .args(["review", "reviewed", "--verdict", "approved"])
        .assert()
        .success()
        .stderr(predicates::str::contains("Implemented-by"))
        .get_output()
        .clone();
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(out.contains("does not gate"), "{out}");
    repo.arc(&repo.root)
        .args(["integrate", "reviewed"])
        .assert()
        .failure();

    // Another model gates.
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Other")
        .args(["--model", "fixture-opus#medium"])
        .args(["review", "reviewed", "--verdict", "approved"])
        .assert()
        .success()
        .stderr(predicates::str::contains("Implemented-by").not());
    repo.arc(&repo.root)
        .args(["integrate", "reviewed"])
        .assert()
        .success();
}

#[test]
fn a_review_by_the_implementers_model_warns_where_policy_permits_it() {
    let repo = repo_with_policy("[policy]\nforbid_self_approval = false\n");
    trailed_change(&repo, "lenient", &[&[SOL]]);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reviewer")
        .args(["--model", "gpt-6.1-sol#xhigh"])
        .args(["review", "lenient", "--verdict", "approved"])
        .assert()
        .success()
        .stderr(predicates::str::contains("Implemented-by"));
    repo.arc(&repo.root)
        .args(["integrate", "lenient"])
        .assert()
        .success();
}

const FIXTURE_READER: &str = "fixture-opus#medium";

/// A change read once by `FIXTURE_READER` and integrated owing a read
/// routing would count.
fn owed_change(repo: &Repo, slug: &str, commits: &[&[&str]]) {
    trailed_change(repo, slug, commits);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Reader")
        .args(["--model", FIXTURE_READER])
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "integrate",
            slug,
            "--debt",
            "same-family read only",
            "--kind",
            "ineligible-review",
            "--qualifier",
            "same-family",
        ])
        .assert()
        .success();
}

/// An eligible assessment of `fixture-other` whose authors answer to
/// `author_identities`.
fn eligible_assessment(repo: &Repo, name: &str, author_identities: &[&[&str]]) -> PathBuf {
    let path = repo.home.join(name);
    let authors: Vec<_> = author_identities
        .iter()
        .map(|identities| serde_json::json!({"identities": identities, "author_floor": 1}))
        .collect();
    fs::write(
        &path,
        serde_json::json!({
            "schema": "agent-routing-assessment/1",
            "independence": "eligible",
            "qualifiers": [],
            "route_version": "2026.10.12",
            "reviewer": {"identities": ["fixture-other"], "effort": "high", "review_tier": 3},
            "authors": authors,
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn audit_assessed(repo: &Repo, slug: &str, assessment: &Path) -> String {
    stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "Auditor")
            .args(["--model", "fixture-other#high"])
            .args(["audit", slug, "--verdict", "approved", "--assessment"])
            .arg(assessment),
    )
}

#[test]
fn an_assessment_covering_one_of_two_implementers_leaves_the_debt_open() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "pair", &[&[SOL], &[SONNET]]);

    let partial = eligible_assessment(&repo, "partial.json", &[&["gpt-6.1-sol"]]);
    let output = audit_assessed(&repo, "pair", &partial);
    assert!(output.contains("still owes"), "{output}");
    assert!(output.contains("claude-sonnet-5-5"), "{output}");
    let debt = debt_of(&repo, "pair");
    assert!(debt["discharged_by"].is_null(), "{debt}");
    let later = &debt["ineligible"]["later_reads"][0];
    assert_eq!(later["binding"], "authors-uncovered", "{debt}");
    assert_eq!(
        later["uncovered_authors"],
        serde_json::json!(["claude-sonnet-5-5"]),
        "{debt}"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "pair", "--json"]));
    assert_eq!(status["debt_outstanding"], true, "{status}");
    let show = stdout(repo.arc(&repo.root).args(["show", "pair"]));
    assert!(show.contains("claude-sonnet-5-5"), "{show}");
    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains("claude-sonnet-5-5"), "{catchup}");

    let full = eligible_assessment(
        &repo,
        "full.json",
        &[&["gpt-6.1-sol"], &["claude-sonnet-5-5", "sonnet-5.5"]],
    );
    let output = audit_assessed(&repo, "pair", &full);
    assert!(output.contains("debt discharged"), "{output}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "pair", "--json"]));
    assert_eq!(status["debt_outstanding"], false, "{status}");
}

#[test]
fn a_range_without_implementers_asks_nothing_of_the_assessment() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "plain", &[&[]]);
    let none = eligible_assessment(&repo, "none.json", &[]);
    let output = audit_assessed(&repo, "plain", &none);
    assert!(output.contains("debt discharged"), "{output}");
}

#[test]
fn other_debt_kinds_clear_without_the_coverage_rule() {
    let repo = repo_forbidding_self_approval();
    trailed_change(&repo, "kinds", &[&[SOL], &[SONNET]]);
    integrate_with_debt(&repo, "kinds");
    audit_as(&repo, "kinds", "fixture-opus#medium", "approved").success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "kinds", "--json"]));
    assert_eq!(status["debt_outstanding"], false, "{status}");
}

#[test]
fn an_audit_whose_assessment_covers_its_range_keeps_its_older_shape() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "shape", &[&[SOL]]);
    let covering = eligible_assessment(&repo, "covering.json", &[&["gpt-6.1-sol"]]);
    let output = audit_assessed(&repo, "shape", &covering);
    assert!(output.contains("debt discharged"), "{output}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "shape", "--json"]));
    let assessment = &status["audit_verdicts"][0]["assessment"];
    assert_eq!(assessment["binding"], "bound", "{status}");
    assert!(assessment.get("uncovered_authors").is_none(), "{status}");
}

/// An `ineligible-review` debt whose production kept no list and whose range
/// no longer resolves in the repository.
fn owed_change_with_unreadable_range(repo: &Repo, slug: &str) {
    owed_change(repo, slug, &[&[SOL]]);
    let change = opened_change_id_of(repo, slug);
    rewrite_event(repo, &change, "debt-declared", |event| {
        event["production"]
            .as_object_mut()
            .unwrap()
            .remove("trailer_implementers");
    });
    rewrite_event(repo, &change, "patchset-added", |event| {
        event["base"] = serde_json::json!("0".repeat(40));
    });
}

fn opened_change_id_of(repo: &Repo, slug: &str) -> String {
    json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]))["change_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn an_unreadable_range_never_binds_an_assessment_to_its_authors() {
    let repo = repo_forbidding_self_approval();
    owed_change_with_unreadable_range(&repo, "unread");

    let covering = eligible_assessment(&repo, "unread.json", &[&["gpt-6.1-sol"]]);
    let output = audit_assessed(&repo, "unread", &covering);
    assert!(output.contains("still owes"), "{output}");
    assert!(output.contains("could not be read"), "{output}");
    let debt = debt_of(&repo, "unread");
    assert!(debt["discharged_by"].is_null(), "{debt}");
    assert_eq!(
        debt["ineligible"]["later_reads"][0]["binding"], "authors-unread",
        "{debt}"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "unread", "--json"]));
    assert_eq!(status["debt_outstanding"], true, "{status}");
    let show = stdout(repo.arc(&repo.root).args(["show", "unread"]));
    assert!(show.contains("could not be read"), "{show}");
}
