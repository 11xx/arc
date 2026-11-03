use super::common::*;

const WORK: &str = "shipped";

struct Fixture {
    repo: Repo,
    change_id: String,
    integrated: String,
}

/// A change gated by `unit`, self-approved, and integrated under a debt
/// waiver, so it ships owing an independent review.
fn integrated_owing_review() -> Fixture {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = \"test -f marker\"\ntimeout = \"1m\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "policy and gates"]);
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", WORK])));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{WORK}"));
    repo.commit(&worktree, "marker", "", "feat: work");
    stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Solo")
            .args(["snapshot", WORK]),
    );
    repo.arc(&worktree)
        .args(["verify", WORK, "--gate", "unit"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", WORK, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", WORK, "--debt", "no second reviewer reachable"])
        .assert()
        .success();
    let integrated = git_out(&repo.root, &["rev-parse", "master"]);
    Fixture {
        repo,
        change_id,
        integrated,
    }
}

fn receipt(repo: &Repo, gate: &str, result: &str, tested: &str) -> AssertCommand {
    let mut command = repo.arc(&repo.root);
    command.args([
        "verify",
        WORK,
        "--gate",
        gate,
        "--attest",
        "--after-integration",
        "--result",
        result,
        "--tested-revision",
        tested,
        "--execution-host",
        "ci-host-1",
        "--runner",
        "watch-job-85",
    ]);
    command
}

fn status(repo: &Repo) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", WORK, "--json"]))
}

fn show(repo: &Repo) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["show", WORK, "--json"]))
}

/// Claim: a pass or a fail observed after integration is retained and read
/// back with the exact revision and tree tested, the runner, host, command,
/// timeout, producer and observation time, labelled as observed after
/// integration in `show`, `show --json`, and `status --json`.
/// Rejects: the observation being refused (`event is open-only`) and lost, or
/// read back without the coordinates it was tested at.
#[test]
fn a_check_observed_after_integration_is_retained_with_its_coordinates() {
    let fixture = integrated_owing_review();
    let repo = &fixture.repo;
    let tree = git_out(
        &repo.root,
        &["rev-parse", &format!("{}^{{tree}}", fixture.integrated)],
    );

    let output = stdout(
        receipt(repo, "unit", "pass", &fixture.integrated)
            .env("ARC_ACTOR", "Watcher")
            .args(["--note", "85 offline cases"]),
    );
    assert!(
        output.contains("verification after integration: Pass (attested)"),
        "{output}"
    );
    // A failure observed afterwards is retained just as faithfully, and the
    // exit code says what was observed.
    receipt(repo, "unit", "fail", &fixture.integrated)
        .env("ARC_ACTOR", "Watcher")
        .assert()
        .code(1);

    for (view, receipts) in [
        (
            "status",
            status(repo)["post_integration_verifications"].clone(),
        ),
        ("show", show(repo)["post_integration_verifications"].clone()),
    ] {
        let receipts = receipts
            .as_array()
            .unwrap_or_else(|| panic!("{view}: {receipts}"));
        assert_eq!(receipts.len(), 2, "{view}: {receipts:?}");
        let pass = &receipts[0];
        assert_eq!(pass["after_integration"], true, "{view}");
        assert_eq!(pass["result"], "pass", "{view}");
        assert_eq!(receipts[1]["result"], "fail", "{view}");
        assert_eq!(pass["gate"], "unit", "{view}");
        assert_eq!(pass["command"], "test -f marker", "{view}");
        assert_eq!(pass["revision"], fixture.integrated.as_str(), "{view}");
        assert_eq!(pass["tree"], tree.as_str(), "{view}");
        assert_eq!(
            pass["integrated_commit"],
            fixture.integrated.as_str(),
            "{view}"
        );
        assert_eq!(pass["hostname"], "ci-host-1", "{view}");
        assert_eq!(pass["runner"], "watch-job-85", "{view}");
        assert_eq!(pass["timeout_seconds"], 60, "{view}");
        assert_eq!(pass["note"], "85 offline cases", "{view}");
        assert_eq!(pass["actor"], "Watcher", "{view}");
        assert!(pass["created_at"].is_string(), "{view}");
        assert!(pass["event_id"].is_string(), "{view}");
    }

    let text = stdout(repo.arc(&repo.root).args(["show", WORK]));
    let section = text
        .split("## Verification after integration")
        .nth(1)
        .unwrap_or_else(|| panic!("no receipt section: {text}"));
    assert!(
        section.contains(&format!(
            "unit `test -f marker` at `{}` (tree `{tree}`) → Pass (attested, observed after \
             integration)",
            fixture.integrated
        )),
        "{section}"
    );
    assert!(
        section.contains("ran on ci-host-1 by watch-job-85"),
        "{section}"
    );
    assert!(section.contains("by Watcher"), "{section}");
}

/// Claim: a receipt changes nothing about what authorized the integration:
/// `arc explain --at <integration>` reads the same before and after, and the
/// gate evidence the change shipped with stays the evidence it shipped with.
/// Rejects: a receipt counted as gate evidence, which would make the
/// integrated change's gate read the receipt's failure.
#[test]
fn a_receipt_leaves_the_authorization_basis_as_it_was() {
    let fixture = integrated_owing_review();
    let repo = &fixture.repo;
    let integration = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        WORK,
        "--type",
        "change-integrated",
    ]));
    let integration: serde_json::Value = serde_json::from_str(integration.trim()).unwrap();
    let integration = integration["event_id"].as_str().unwrap().to_string();
    let explain_at = |repo: &Repo| {
        json_stdout(
            repo.arc(&repo.root)
                .args(["explain", WORK, "--json", "--at", &integration]),
        )
    };
    let before = explain_at(repo);
    let unbounded_before = json_stdout(repo.arc(&repo.root).args(["explain", WORK, "--json"]));
    let gates_before = status(repo)["gates"].clone();
    let shown_before = show(repo);

    receipt(repo, "unit", "fail", &fixture.integrated)
        .assert()
        .code(1);

    assert_eq!(explain_at(repo), before);
    assert_eq!(status(repo)["gates"], gates_before);
    let shown = show(repo);
    assert_eq!(shown["verifications"], shown_before["verifications"]);
    assert_eq!(shown["closure"], shown_before["closure"]);

    // The unbounded view places it in later knowledge, never in evaluation.
    let now = json_stdout(repo.arc(&repo.root).args(["explain", WORK, "--json"]));
    for slot in ["contract", "evaluation", "coverage_at_acceptance"] {
        assert_eq!(now[slot], unbounded_before[slot], "{slot}");
    }
    let later = now["later_knowledge"]["rows"].as_array().unwrap();
    assert!(
        later
            .iter()
            .any(|row| row["item"] == "post-integration-verification"
                && row["result"] == "fail"
                && row["standing"] == "declared"),
        "{later:?}"
    );
}

/// Claim: a receipt discharges no review debt.
/// Rejects: a passing check after integration read as the independent review
/// the change still owes.
#[test]
fn a_receipt_discharges_no_review_debt() {
    let fixture = integrated_owing_review();
    let repo = &fixture.repo;
    assert_eq!(status(repo)["debt_outstanding"], true);

    receipt(repo, "unit", "pass", &fixture.integrated)
        .env("ARC_ACTOR", "Auditor")
        .assert()
        .success();

    let after = status(repo);
    assert_eq!(after["debt_outstanding"], true);
    assert_eq!(after["debt_standing"], "owed");
    let owed = stdout(repo.arc(&repo.root).args(["query", "--debt"]));
    assert!(owed.contains(&fixture.change_id), "{owed}");
}

/// Claim: a receipt binds only the content that shipped. A tested revision
/// whose tree differs from the integrated commit's is refused and nothing is
/// recorded; a different commit carrying the integrated tree is accepted and
/// recorded under its own revision.
/// Rejects: a receipt for other content reading as verification of the
/// integrated tree.
#[test]
fn a_receipt_for_other_content_is_refused() {
    let fixture = integrated_owing_review();
    let repo = &fixture.repo;
    let before = event_count(repo, &fixture.change_id);
    let earlier = git_out(
        &repo.root,
        &["rev-parse", &format!("{}^1", fixture.integrated)],
    );

    receipt(repo, "unit", "pass", &earlier)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "verifies only the content that shipped",
        ));
    assert_eq!(event_count(repo, &fixture.change_id), before);

    let tree = git_out(
        &repo.root,
        &["rev-parse", &format!("{}^{{tree}}", fixture.integrated)],
    );
    let same_content = git_out(
        &repo.root,
        &[
            "commit-tree",
            &tree,
            "-p",
            &earlier,
            "-m",
            "same tree elsewhere",
        ],
    );
    receipt(repo, "unit", "pass", &same_content)
        .assert()
        .success();
    let receipts = status(repo)["post_integration_verifications"].clone();
    assert_eq!(receipts[0]["revision"], same_content.as_str());
    assert_eq!(receipts[0]["tree"], tree.as_str());
    assert_eq!(
        receipts[0]["integrated_commit"],
        fixture.integrated.as_str()
    );
}

/// Claim: an open change keeps the ordinary attestation exactly, and the
/// receipt form is refused there with a remedy naming that attestation.
/// Rejects: a receipt recorded on a change that has not shipped, or a refusal
/// that leaves the caller without the command that does fit.
#[test]
fn an_open_change_refuses_the_receipt_and_names_the_ordinary_attestation() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand = \"true\"\n");
    let (change_id, worktree, head) = change_with_patchset(&repo, "open");
    let before = event_count(&repo, &change_id);

    repo.arc(&worktree)
        .args([
            "verify",
            "open",
            "--gate",
            "unit",
            "--attest",
            "--after-integration",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "ci-host-1",
            "--runner",
            "job",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("change ").and(predicates::str::contains("is open")))
        .stderr(predicates::str::contains(
            "--attest --result <pass|fail> --tested-revision <rev>",
        ));
    assert_eq!(event_count(&repo, &change_id), before);

    repo.arc(&worktree)
        .args([
            "verify",
            "open",
            "--gate",
            "unit",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "ci-host-1",
            "--runner",
            "job",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "open", "--json"]));
    let unit = status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|gate| gate["name"] == "unit")
        .unwrap()
        .clone();
    assert_eq!(unit["green_at_head"], true, "{unit}");
    assert!(
        status.get("post_integration_verifications").is_none(),
        "{status}"
    );
}

/// Claim: the receipt form records only what a caller observed elsewhere, so
/// it requires --attest.
#[test]
fn the_receipt_form_requires_attestation() {
    let fixture = integrated_owing_review();
    fixture
        .repo
        .arc(&fixture.repo.root)
        .args(["verify", WORK, "--gate", "unit", "--after-integration"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--attest"));
}
