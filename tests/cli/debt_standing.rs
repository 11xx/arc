//! A debt's displayed standing: declared before integration, owed after it,
//! discharged once an eligible review answers it. Every view reads the same
//! answer, and readiness (`debt_outstanding`) keeps its own meaning.

use super::common::*;

const SAME_FAMILY_REVIEWER: &str = "fixture-opus#medium";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Standing {
    Declared,
    Owed,
    Discharged,
}

impl Standing {
    fn name(self) -> &'static str {
        match self {
            Standing::Declared => "declared",
            Standing::Owed => "owed",
            Standing::Discharged => "discharged",
        }
    }
}

fn repo_forbidding_self_approval() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

/// An open change with a patchset and a `nothing-read` debt.
fn declared_nothing_read(repo: &Repo, slug: &str) -> String {
    let (change_id, _, _) = change_with_patchset(repo, slug);
    repo.arc(&repo.root)
        .args([
            "debt",
            slug,
            "--reason",
            "no reviewer reachable",
            "--kind",
            "nothing-read",
        ])
        .assert()
        .success();
    change_id
}

/// An open change read once by a same-family reviewer, with an
/// `ineligible-review` debt naming that read.
fn declared_ineligible(repo: &Repo, slug: &str) -> String {
    let (change_id, _, _) = change_with_patchset(repo, slug);
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Reviewer").args([
        "--model",
        SAME_FAMILY_REVIEWER,
        "review",
        slug,
        "--verdict",
        "approved",
    ]));
    repo.arc(&repo.root)
        .args([
            "debt",
            slug,
            "--reason",
            "same-family read only",
            "--kind",
            "ineligible-review",
            "--qualifier",
            "same-family",
            "--route-version",
            "2026.10.12",
        ])
        .assert()
        .success();
    change_id
}

fn integrate(repo: &Repo, slug: &str) {
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

fn eligible_assessment(repo: &Repo, name: &str) -> PathBuf {
    let path = repo.home.join(name);
    fs::write(
        &path,
        serde_json::json!({
            "schema": "agent-routing-assessment/1",
            "independence": "eligible",
            "qualifiers": [],
            "route_version": "2026.10.12",
            "reviewer": {
                "model": SAME_FAMILY_REVIEWER,
                "review_tier": 2,
                "identities": ["fixture-other"],
                "effort": "high",
            },
            "authors": [{"model": "fixture-opus#high", "author_floor": 3}],
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn audit_with_eligible_assessment(repo: &Repo, slug: &str) {
    let file = eligible_assessment(repo, &format!("{slug}-assessment.json"));
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args([
            "--model",
            "fixture-other#high",
            "audit",
            slug,
            "--verdict",
            "approved",
            "--assessment",
            file.to_str().unwrap(),
        ])
        .assert()
        .success();
}

fn audit_independently(repo: &Repo, slug: &str) {
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args(["audit", slug, "--verdict", "approved"])
        .assert()
        .success();
}

/// The backlog project row for the fixture repository, as JSON and text. A
/// repository with nothing in its backlog has no row.
fn backlog(repo: &Repo) -> (serde_json::Value, String) {
    let value = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--json"]),
    );
    let project = value["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"debt_owed": []}));
    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    (project, text)
}

/// Every presentation of the debt on `slug` says `standing`, and only that.
fn assert_standing(repo: &Repo, slug: &str, change_id: &str, standing: Standing) {
    let name = standing.name();

    let show = stdout(repo.arc(&repo.root).args(["show", slug]));
    match standing {
        Standing::Declared => {
            assert!(
                show.contains("Declared, not yet integrated"),
                "show must name the declared standing:\n{show}"
            );
            assert!(!show.contains("Owed"), "{show}");
            assert!(!show.to_lowercase().contains("discharged"), "{show}");
            assert!(!show.contains("Post-integration audit"), "{show}");
        }
        Standing::Owed => {
            assert!(show.contains("- Owed: "), "{show}");
            assert!(!show.contains("(discharged)"), "{show}");
            assert!(!show.contains("Discharged by"), "{show}");
            assert!(!show.contains("Declared, not yet integrated"), "{show}");
        }
        Standing::Discharged => {
            assert!(show.contains("- Owed (discharged): "), "{show}");
            assert!(show.contains("Discharged by: Auditor"), "{show}");
        }
    }

    let state = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    assert_eq!(state["debt"]["standing"], name, "{state}");

    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt_standing"], name, "{status}");
    assert_eq!(status["debt"]["standing"], name, "{status}");
    // Readiness keeps its own meaning: outstanding only once integrated and
    // not discharged.
    assert_eq!(
        status["debt_outstanding"],
        standing == Standing::Owed,
        "{status}"
    );
    assert_eq!(
        status["debt"]["discharged_by"].is_object(),
        standing == Standing::Discharged,
        "{status}"
    );

    let explained = json_stdout(repo.arc(&repo.root).args(["explain", slug, "--json"]));
    let rows: Vec<&serde_json::Value> = explained["coverage_at_acceptance"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["item"] == "debt")
        .collect();
    assert_eq!(rows.len(), 1, "{explained}");
    assert_eq!(rows[0]["debt_standing"], name, "{explained}");
    let explained_text = stdout(repo.arc(&repo.root).args(["explain", slug]));
    let phrase = match standing {
        Standing::Declared => "declared, not yet integrated",
        Standing::Owed => "standing: owed",
        Standing::Discharged => "standing: discharged",
    };
    assert!(explained_text.contains(phrase), "{explained_text}");
    if standing == Standing::Declared {
        assert!(
            !explained_text.to_lowercase().contains("discharged"),
            "{explained_text}"
        );
    }

    let (project, backlog_text) = backlog(repo);
    let listed = project["debt_owed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["change_id"] == change_id);
    assert_eq!(listed, standing == Standing::Owed, "{project}");
    assert_eq!(
        backlog_text.contains(&format!("debt-owed     {change_id}")),
        standing == Standing::Owed,
        "{backlog_text}"
    );
}

#[test]
fn an_open_nothing_read_debt_is_declared_and_never_discharged() {
    let repo = repo_forbidding_self_approval();
    let id = declared_nothing_read(&repo, "open-nothing");
    assert_standing(&repo, "open-nothing", &id, Standing::Declared);
}

#[test]
fn an_open_ineligible_review_debt_is_declared_and_never_discharged() {
    let repo = repo_forbidding_self_approval();
    let id = declared_ineligible(&repo, "open-ineligible");
    assert_standing(&repo, "open-ineligible", &id, Standing::Declared);
}

#[test]
fn an_integrated_unaudited_debt_is_owed() {
    let repo = repo_forbidding_self_approval();
    let id = declared_nothing_read(&repo, "owed-nothing");
    integrate(&repo, "owed-nothing");
    assert_standing(&repo, "owed-nothing", &id, Standing::Owed);
}

#[test]
fn an_integrated_ineligible_review_debt_is_owed_until_an_eligible_audit() {
    let repo = repo_forbidding_self_approval();
    let id = declared_ineligible(&repo, "owed-ineligible");
    integrate(&repo, "owed-ineligible");
    assert_standing(&repo, "owed-ineligible", &id, Standing::Owed);

    // An audit with no assessment is recorded beside the debt and does not
    // clear this kind.
    audit_independently(&repo, "owed-ineligible");
    assert_standing(&repo, "owed-ineligible", &id, Standing::Owed);
}

#[test]
fn an_eligible_audit_discharges_an_integrated_debt_with_its_discharger() {
    let repo = repo_forbidding_self_approval();
    let id = declared_ineligible(&repo, "cleared-ineligible");
    integrate(&repo, "cleared-ineligible");
    audit_with_eligible_assessment(&repo, "cleared-ineligible");
    assert_standing(&repo, "cleared-ineligible", &id, Standing::Discharged);
}

#[test]
fn an_independent_audit_discharges_an_integrated_nothing_read_debt() {
    let repo = repo_forbidding_self_approval();
    let id = declared_nothing_read(&repo, "cleared-nothing");
    integrate(&repo, "cleared-nothing");
    audit_independently(&repo, "cleared-nothing");
    assert_standing(&repo, "cleared-nothing", &id, Standing::Discharged);
}

#[test]
fn chain_reports_the_standing_beside_the_outstanding_flag() {
    let repo = repo_forbidding_self_approval();
    for slug in ["chain-open", "chain-owed"] {
        stdout(
            repo.arc(&repo.root)
                .args(["begin", slug, "--tag", "program"]),
        );
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.commit(&worktree, &format!("{slug}.txt"), "work\n", "test: work");
        stdout(repo.arc(&worktree).args(["snapshot", slug]));
        repo.arc(&repo.root)
            .args(["debt", slug, "--reason", "no reviewer reachable"])
            .assert()
            .success();
    }
    integrate(&repo, "chain-owed");
    let chain = json_stdout(
        repo.arc(&repo.root)
            .args(["chain", "program", "--review", "--json"]),
    );
    for (slug, expected, outstanding) in [
        ("chain-open", "declared", false),
        ("chain-owed", "owed", true),
    ] {
        let member = chain["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|member| member["slug"] == slug)
            .unwrap_or_else(|| panic!("{slug} missing: {chain}"));
        assert_eq!(member["review"]["debt_standing"], expected, "{chain}");
        assert_eq!(member["review"]["debt_outstanding"], outstanding, "{chain}");
    }
    let text = stdout(repo.arc(&repo.root).args(["chain", "program", "--review"]));
    assert!(
        text.contains("audit: declared, not yet integrated"),
        "{text}"
    );
    assert!(text.contains("audit: owed, not yet discharged"), "{text}");
}
