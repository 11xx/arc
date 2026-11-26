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

fn declare_whole_debt(repo: &Repo, worktree: &Path, slug: &str) {
    stdout(repo.arc(worktree).args([
        "debt",
        slug,
        "--reason",
        "independent review deferred",
        "--extent",
        "whole",
    ]));
}

fn assert_newer_approval_leaves_no_debt(rebase: bool) {
    let repo = repo_forbidding_self_approval();
    let slug = "reviewed-successor";
    let (_, worktree, first_head) = change_with_patchset(&repo, slug);
    let first_base = repo.head(&repo.root);
    declare_whole_debt(&repo, &worktree, slug);
    let second_base = if rebase {
        repo.commit(&repo.root, "target.txt", "target\n", "move target");
        stdout(repo.arc(&repo.root).args(["rebase", slug]));
        repo.head(&repo.root)
    } else {
        repo.commit(&worktree, "new.txt", "second edit\n", "extend change");
        stdout(repo.arc(&worktree).args(["snapshot", slug]));
        first_base.clone()
    };
    let second_head = repo.head(&worktree);
    assert_ne!(first_head, second_head);
    assert_eq!(first_base != second_base, rebase);

    let draft = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    assert_eq!(draft["debt"]["patchset_id"], "ps-01", "{draft}");
    assert_eq!(draft["debt"]["extent"]["base"], first_base, "{draft}");
    assert_eq!(draft["debt"]["extent"]["revision"], first_head, "{draft}");
    assert_eq!(draft["patchsets"][1]["id"], "ps-02", "{draft}");
    assert_eq!(draft["patchsets"][1]["base"], second_base, "{draft}");
    assert_eq!(draft["patchsets"][1]["head"], second_head, "{draft}");

    stdout(repo.arc(&worktree).env("ARC_ACTOR", "Reviewer").args([
        "review",
        slug,
        "--verdict",
        "approved",
    ]));
    integrate(&repo, slug);
    let state = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    let closure = &state["closure"];
    assert_eq!(closure["source_patchset_id"], "ps-02", "{state}");
    assert_eq!(
        closure["authorization"]["verdict_event_id"], state["verdicts"][0]["event_id"],
        "{state}"
    );
    assert!(
        closure["authorization"]["verdict_event_id"].is_string(),
        "{state}"
    );
    assert!(
        closure["authorization"]["audit_debt_event_id"].is_null(),
        "{state}"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt_outstanding"], false, "{status}");
    assert!(status["debt_standing"].is_null(), "{status}");
    assert!(stdout(repo.arc(&repo.root).args(["query", "--debt"]))
        .trim()
        .is_empty());
    let debt_event = draft["debt"]["event_id"].as_str().unwrap();
    let history = json_stdout(
        repo.arc(&repo.root)
            .args(["show", slug, "--at", debt_event, "--json"]),
    );
    assert_eq!(history["debt"]["event_id"], debt_event, "{history}");
    assert_eq!(history["debt"]["standing"], "declared", "{history}");
}

#[test]
fn independent_approval_of_a_new_revision_leaves_no_draft_debt() {
    assert_newer_approval_leaves_no_debt(false);
}

#[test]
fn independent_approval_after_a_rebase_leaves_no_draft_debt() {
    assert_newer_approval_leaves_no_debt(true);
}

#[test]
fn debt_that_authorized_its_patchset_is_owed_until_a_whole_audit() {
    let repo = repo_forbidding_self_approval();
    let slug = "shipped-waiver";
    let (_, worktree, head) = change_with_patchset(&repo, slug);
    declare_whole_debt(&repo, &worktree, slug);
    integrate(&repo, slug);
    let state = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    assert_eq!(state["debt"]["standing"], "owed", "{state}");
    assert_eq!(state["closure"]["source_patchset_id"], "ps-01", "{state}");
    assert_eq!(state["debt"]["extent"]["revision"], head, "{state}");
    assert_eq!(
        state["closure"]["authorization"]["audit_debt_event_id"], state["debt"]["event_id"],
        "{state}"
    );
    let audit = stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Auditor").args([
        "audit",
        slug,
        "--verdict",
        "approved",
        "--extent",
        "whole",
    ]));
    assert!(audit.contains("debt discharged"), "{audit}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt_standing"], "discharged", "{status}");
    assert_eq!(status["debt_outstanding"], false, "{status}");
}

#[test]
fn a_new_patchset_requires_its_own_waiver_without_independent_approval() {
    let repo = repo_forbidding_self_approval();
    let slug = "stale-debt";
    let (_, worktree, _) = change_with_patchset(&repo, slug);
    declare_whole_debt(&repo, &worktree, slug);
    let target = repo.head(&repo.root);
    repo.commit(&worktree, "new.txt", "second edit\n", "extend change");
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .code(3);
    assert_eq!(repo.head(&repo.root), target);
    let unread = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(unread["ready_reason"], "no-valid-approval", "{unread}");

    stdout(
        repo.arc(&worktree)
            .args(["review", slug, "--verdict", "approved"]),
    );
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .code(3);
    assert_eq!(repo.head(&repo.root), target);
    let self_reviewed = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(
        self_reviewed["verdict"]["valid_for_current_head"], false,
        "{self_reviewed}"
    );

    declare_whole_debt(&repo, &worktree, slug);
    integrate(&repo, slug);
    let state = json_stdout(repo.arc(&repo.root).args(["show", slug, "--json"]));
    assert_eq!(state["debt"]["standing"], "owed", "{state}");
    assert_eq!(state["debt"]["patchset_id"], "ps-02", "{state}");
    assert_eq!(state["closure"]["source_patchset_id"], "ps-02", "{state}");
    assert_eq!(
        state["closure"]["authorization"]["audit_debt_event_id"], state["debt"]["event_id"],
        "{state}"
    );
    let audit = stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Auditor").args([
        "audit",
        slug,
        "--verdict",
        "approved",
        "--extent",
        "whole",
    ]));
    assert!(audit.contains("debt discharged"), "{audit}");
}

#[test]
fn a_contributor_approval_cannot_retire_an_earlier_waiver() {
    let repo = Repo::new();
    let slug = "contributor-successor";
    let (_, worktree, _) = change_with_patchset(&repo, slug);
    declare_whole_debt(&repo, &worktree, slug);
    repo.commit(&worktree, "new.txt", "second edit\n", "extend change");
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    stdout(
        repo.arc(&worktree)
            .args(["review", slug, "--verdict", "approved"]),
    );
    integrate(&repo, slug);
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt_standing"], "owed", "{status}");
    assert_eq!(status["debt_outstanding"], true, "{status}");
    assert_eq!(status["debt"]["patchset_id"], "ps-01", "{status}");
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

/// Approve `slug` as `reviewer` at `fixture-other#high`, under an eligible
/// assessment of that reviewer when `assessed`.
fn review_at_other(repo: &Repo, slug: &str, reviewer: &str, assessed: bool) {
    let mut command = repo.arc(&repo.root);
    command.env("ARC_ACTOR", reviewer).args([
        "--model",
        "fixture-other#high",
        "review",
        slug,
        "--verdict",
        "approved",
    ]);
    if assessed {
        let file = eligible_assessment(repo, &format!("{slug}-{reviewer}.json"));
        command.arg("--assessment").arg(file);
    }
    command.assert().success();
}

/// Claim: an `ineligible-review` debt is discharged by a verdict on the
/// shipped patchset, recorded after the declaration, that carries its own
/// eligible, bound assessment, as an audit with one would; a verdict without
/// one leaves the debt owed. Rejects a reader that only lets an audit
/// discharge this kind, and one that lets any independent verdict do it.
#[test]
fn an_assessed_verdict_since_the_declaration_discharges_ineligible_review() {
    let repo = repo_forbidding_self_approval();
    let id = declared_ineligible(&repo, "verdict-cleared");
    review_at_other(&repo, "verdict-cleared", "Plain", false);
    review_at_other(&repo, "verdict-cleared", "Auditor", true);
    integrate(&repo, "verdict-cleared");
    assert_standing(&repo, "verdict-cleared", &id, Standing::Discharged);
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "verdict-cleared", "--json"]),
    );
    assert_eq!(
        status["debt"]["discharged_by"]["reviewer"], "Auditor",
        "{status}"
    );

    let id = declared_ineligible(&repo, "verdict-unassessed");
    review_at_other(&repo, "verdict-unassessed", "Plain", false);
    integrate(&repo, "verdict-unassessed");
    assert_standing(&repo, "verdict-unassessed", &id, Standing::Owed);
}
