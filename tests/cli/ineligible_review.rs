use super::common::*;

const SAME_FAMILY_REVIEWER: &str = "fixture-opus#medium";

pub(super) fn repo_forbidding_self_approval() -> Repo {
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

/// A snapshotted change at its own file, so two changes never collide.
pub(super) fn snapshotted(repo: &Repo, slug: &str) {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "work\n",
        &format!("feat: {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
}

fn event_of(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap_or_else(|| panic!("no event line in {output}"))
        .trim()
        .to_string()
}

/// One approving read of the change by a named reviewer at a model string.
pub(super) fn read_by(repo: &Repo, slug: &str, reviewer: &str, model: &str) -> String {
    event_of(&stdout(
        repo.arc(&repo.root).env("ARC_ACTOR", reviewer).args([
            "--model",
            model,
            "review",
            slug,
            "--verdict",
            "approved",
        ]),
    ))
}

/// A change read once, by a reviewer routing would not count independent.
pub(super) fn read_change(repo: &Repo, slug: &str) -> String {
    snapshotted(repo, slug);
    read_by(repo, slug, "Reviewer", SAME_FAMILY_REVIEWER)
}

pub(super) fn assessment(
    repo: &Repo,
    name: &str,
    independence: &str,
    qualifiers: &[&str],
) -> PathBuf {
    assessment_for(repo, name, independence, qualifiers, None, None)
}

/// An assessment whose reviewer lists the model strings routing accepts for
/// it and, optionally, its effort, as `agent-routing assess --json` does.
fn assessment_for(
    repo: &Repo,
    name: &str,
    independence: &str,
    qualifiers: &[&str],
    identities: Option<&[&str]>,
    effort: Option<&str>,
) -> PathBuf {
    let path = repo.home.join(name);
    let mut reviewer = serde_json::json!({"model": SAME_FAMILY_REVIEWER, "review_tier": 2});
    if let Some(identities) = identities {
        reviewer["identities"] = serde_json::json!(identities);
    }
    if let Some(effort) = effort {
        reviewer["effort"] = serde_json::json!(effort);
    }
    fs::write(
        &path,
        serde_json::json!({
            "schema": "agent-routing-assessment/1",
            "independence": independence,
            "qualifiers": qualifiers,
            "route_version": "2026.10.12",
            "reviewer": reviewer,
            "authors": [{"model": "fixture-opus#high", "author_floor": 3}],
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn files_mentioning(dir: &Path, needle: &str) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| {
            if path.is_dir() {
                files_mentioning(&path, needle)
            } else {
                usize::from(
                    fs::read(&path)
                        .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                        .unwrap_or(false),
                )
            }
        })
        .sum()
}

fn integrate_ineligible(repo: &Repo, slug: &str, extra: &[&str]) -> assert_cmd::assert::Assert {
    repo.arc(&repo.root)
        .args([
            "integrate",
            slug,
            "--debt",
            "same-family read only",
            "--kind",
            "ineligible-review",
        ])
        .args(extra)
        .assert()
}

pub(super) fn debt_of(repo: &Repo, slug: &str) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]))["debt"].clone()
}

#[test]
fn a_declared_integration_records_the_read_and_its_qualifiers() {
    let repo = repo_forbidding_self_approval();
    let read = read_change(&repo, "declared");
    integrate_ineligible(
        &repo,
        "declared",
        &[
            "--qualifier",
            "same-family",
            "--qualifier",
            "below-floor",
            "--qualifier",
            "same-family",
            "--route-version",
            "2026.10.12",
        ],
    )
    .success();

    let debt = debt_of(&repo, "declared");
    assert_eq!(debt["missing"], "ineligible-review", "{debt}");
    let record = &debt["ineligible"];
    assert_eq!(record["read"], read.as_str(), "{debt}");
    // Closed-set order, deduplicated, whatever order the flags came in.
    assert_eq!(
        record["qualifiers"],
        serde_json::json!(["same-family", "below-floor"]),
        "{debt}"
    );
    assert_eq!(record["assessment"], "declared", "{debt}");
    assert_eq!(record["route_version"], "2026.10.12", "{debt}");
    assert_eq!(record["reviewer"], "Reviewer", "{debt}");
    assert_eq!(record["reviewer_model"], SAME_FAMILY_REVIEWER, "{debt}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "declared", "--json"]));
    assert_eq!(status["debt_outstanding"], true, "{status}");
}

#[test]
fn an_assessed_integration_keeps_qualifiers_and_never_tiers() {
    let repo = repo_forbidding_self_approval();
    let read = read_change(&repo, "assessed");
    let file = assessment(
        &repo,
        "assess.json",
        "ineligible",
        &["same-model", "same-family"],
    );
    integrate_ineligible(&repo, "assessed", &["--assessment", file.to_str().unwrap()]).success();

    let debt = debt_of(&repo, "assessed");
    let record = &debt["ineligible"];
    assert_eq!(record["read"], read.as_str(), "{debt}");
    assert_eq!(
        record["qualifiers"],
        serde_json::json!(["same-model", "same-family"]),
        "{debt}"
    );
    assert_eq!(record["assessment"], "assessed", "{debt}");
    assert_eq!(record["route_version"], "2026.10.12", "{debt}");
    let text = debt.to_string();
    assert!(
        !text.contains("tier") && !text.contains("floor\":"),
        "{text}"
    );
    // Neither does any file under the ledger: a tier is routing's reading at
    // read time.
    assert_eq!(files_mentioning(&repo.root.join(".git"), "review_tier"), 0);
}

#[test]
fn read_names_another_verdict_of_the_same_patchset() {
    let repo = repo_forbidding_self_approval();
    let first = read_change(&repo, "named");
    let second = read_by(&repo, "named", "Second", "fixture-haiku#low");
    assert_ne!(first, second);
    integrate_ineligible(
        &repo,
        "named",
        &["--qualifier", "below-floor", "--read", &first],
    )
    .success();
    assert_eq!(
        debt_of(&repo, "named")["ineligible"]["read"],
        first.as_str()
    );

    let repo = repo_forbidding_self_approval();
    read_change(&repo, "stranger");
    integrate_ineligible(
        &repo,
        "stranger",
        &["--qualifier", "below-floor", "--read", "01NOSUCHEVENT"],
    )
    .failure()
    .stderr(predicates::str::contains("01NOSUCHEVENT"));
}

#[test]
fn it_refuses_without_a_verdict_to_name() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "unread");
    integrate_ineligible(&repo, "unread", &["--qualifier", "same-model"])
        .failure()
        .stderr(predicates::str::contains("no verdict"));
    assert!(debt_of(&repo, "unread").is_null());
}

#[test]
fn it_refuses_an_eligible_assessment() {
    let repo = repo_forbidding_self_approval();
    read_change(&repo, "eligible");
    let file = assessment(&repo, "eligible.json", "eligible", &[]);
    integrate_ineligible(&repo, "eligible", &["--assessment", file.to_str().unwrap()])
        .failure()
        .stderr(predicates::str::contains("eligible"));
    assert!(debt_of(&repo, "eligible").is_null());
}

#[test]
fn it_refuses_an_unknown_qualifier_in_a_flag_or_a_file() {
    let repo = repo_forbidding_self_approval();
    read_change(&repo, "unknown");
    integrate_ineligible(&repo, "unknown", &["--qualifier", "smaller-brain"]).failure();
    let file = assessment(&repo, "unknown.json", "ineligible", &["smaller-brain"]);
    integrate_ineligible(&repo, "unknown", &["--assessment", file.to_str().unwrap()])
        .failure()
        .stderr(predicates::str::contains("smaller-brain"));
    assert!(debt_of(&repo, "unknown").is_null());
}

#[test]
fn it_refuses_a_malformed_assessment_and_a_missing_one() {
    let repo = repo_forbidding_self_approval();
    read_change(&repo, "shape");
    integrate_ineligible(&repo, "shape", &[])
        .failure()
        .stderr(predicates::str::contains("--qualifier"));
    let wrong = repo.home.join("wrong.json");
    fs::write(
        &wrong,
        r#"{"schema":"agent-routing-assessment/9","independence":"ineligible","qualifiers":["same-model"]}"#,
    )
    .unwrap();
    integrate_ineligible(&repo, "shape", &["--assessment", wrong.to_str().unwrap()])
        .failure()
        .stderr(predicates::str::contains("agent-routing-assessment/1"));
    integrate_ineligible(
        &repo,
        "shape",
        &["--qualifier", "same-model", "--route-version", "has space"],
    )
    .failure()
    .stderr(predicates::str::contains("route version"));
    let empty = assessment(&repo, "empty.json", "ineligible", &[]);
    integrate_ineligible(&repo, "shape", &["--assessment", empty.to_str().unwrap()])
        .failure()
        .stderr(predicates::str::contains("qualifier"));
    assert!(debt_of(&repo, "shape").is_null());
}

#[test]
fn read_flags_are_refused_on_any_other_kind() {
    let repo = repo_forbidding_self_approval();
    read_change(&repo, "other");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "other",
            "--debt",
            "x",
            "--kind",
            "independent-review",
            "--qualifier",
            "same-model",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("ineligible-review"));
}

fn owed_change(repo: &Repo, slug: &str) {
    read_change(repo, slug);
    integrate_ineligible(
        repo,
        slug,
        &[
            "--qualifier",
            "same-family",
            "--route-version",
            "2026.10.12",
        ],
    )
    .success();
}

#[test]
fn an_audit_without_an_assessment_leaves_the_debt_open_and_listed() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "audited");
    let output = stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Auditor").args([
        "--model",
        "fixture-opus#low",
        "audit",
        "audited",
        "--verdict",
        "approved",
    ]));
    assert!(output.contains("still owes"), "{output}");
    let debt = debt_of(&repo, "audited");
    assert!(debt["discharged_by"].is_null(), "{debt}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "audited", "--json"]));
    assert_eq!(status["debt_outstanding"], true, "{status}");
    let later = &debt["ineligible"]["later_reads"][0];
    assert_eq!(later["reviewer"], "Auditor", "{debt}");
    assert_eq!(later["model"], "fixture-opus#low", "{debt}");
    assert!(later["independence"].is_null(), "{debt}");
    assert!(
        stdout(repo.arc(&repo.root).args(["query", "--debt"])).contains("audited"),
        "still queued"
    );
}

#[test]
fn an_ineligible_assessment_on_an_audit_never_discharges() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "again");
    let file = assessment(&repo, "again.json", "ineligible", &["same-family"]);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args([
            "audit",
            "again",
            "--verdict",
            "approved",
            "--assessment",
            file.to_str().unwrap(),
        ])
        .assert()
        .success();
    let debt = debt_of(&repo, "again");
    assert!(debt["discharged_by"].is_null(), "{debt}");
    let later = &debt["ineligible"]["later_reads"][0];
    assert_eq!(later["independence"], "ineligible", "{debt}");
    assert_eq!(
        later["qualifiers"],
        serde_json::json!(["same-family"]),
        "{debt}"
    );
}

/// Audit `slug` as Auditor, at `model` when given, under an eligible
/// assessment whose reviewer is `identities` at `effort`.
fn audit_under(
    repo: &Repo,
    slug: &str,
    model: Option<&str>,
    identities: Option<&[&str]>,
    effort: Option<&str>,
) -> String {
    let file = assessment_for(
        repo,
        &format!("{slug}.json"),
        "eligible",
        &[],
        identities,
        effort,
    );
    let mut command = repo.arc(&repo.root);
    command.env("ARC_ACTOR", "Auditor");
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    stdout(command.args([
        "audit",
        slug,
        "--verdict",
        "approved",
        "--assessment",
        file.to_str().unwrap(),
    ]))
}

/// The audit stayed open: what it said, and what the ledger lists beside the
/// obligation.
fn assert_unbound(repo: &Repo, slug: &str, output: &str, binding: &str, reason: &str) {
    assert!(output.contains(reason), "{output}");
    assert!(output.contains("still owes"), "{output}");
    let debt = debt_of(repo, slug);
    assert!(debt["discharged_by"].is_null(), "{debt}");
    let later = &debt["ineligible"]["later_reads"][0];
    assert_eq!(later["independence"], "eligible", "{debt}");
    assert_eq!(later["binding"], binding, "{debt}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    assert_eq!(status["debt_outstanding"], true, "{status}");
    assert_eq!(
        status["audit_verdicts"][0]["assessment"]["binding"], binding,
        "{status}"
    );
    let text = stdout(repo.arc(&repo.root).args(["show", slug]));
    assert!(text.contains(reason), "{text}");
}

#[test]
fn an_audit_with_an_eligible_assessment_discharges_the_debt() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "cleared");
    let output = audit_under(
        &repo,
        "cleared",
        Some("fixture-other#high"),
        Some(&["fixture-other", "other-alias"]),
        Some("high"),
    );
    assert!(output.contains("debt discharged"), "{output}");
    let status = json_stdout(repo.arc(&repo.root).args(["status", "cleared", "--json"]));
    assert_eq!(status["debt_outstanding"], false, "{status}");
    assert_eq!(status["debt"]["discharged_by"]["reviewer"], "Auditor");
    assert_eq!(
        status["audit_verdicts"][0]["assessment"]["independence"], "eligible",
        "{status}"
    );
    assert_eq!(
        status["audit_verdicts"][0]["assessment"]["binding"], "bound",
        "{status}"
    );
    // Only the outcome is kept, never the identities routing listed.
    assert!(!status.to_string().contains("other-alias"), "{status}");
    assert!(!stdout(repo.arc(&repo.root).args(["query", "--debt"])).contains("cleared"));
}

#[test]
fn an_effort_only_one_side_records_does_not_unbind_the_assessment() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "effortless");
    let output = audit_under(
        &repo,
        "effortless",
        Some("fixture-other"),
        Some(&["fixture-other"]),
        Some("high"),
    );
    assert!(output.contains("debt discharged"), "{output}");
}

#[test]
fn an_assessment_for_another_model_leaves_the_debt_open() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "wrong-model");
    let output = audit_under(
        &repo,
        "wrong-model",
        Some("fixture-other#high"),
        Some(&["fixture-elsewhere"]),
        Some("high"),
    );
    assert_unbound(
        &repo,
        "wrong-model",
        &output,
        "model-mismatch",
        "the auditor's recorded model is not one the assessed reviewer answers to",
    );
}

#[test]
fn an_assessment_for_another_effort_leaves_the_debt_open() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "wrong-effort");
    let output = audit_under(
        &repo,
        "wrong-effort",
        Some("fixture-other#low"),
        Some(&["fixture-other"]),
        Some("high"),
    );
    assert_unbound(
        &repo,
        "wrong-effort",
        &output,
        "effort-mismatch",
        "the auditor's recorded effort differs from the assessed reviewer's",
    );
}

#[test]
fn an_audit_recording_no_model_leaves_the_debt_open() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "no-model");
    let output = audit_under(&repo, "no-model", None, Some(&["fixture-other"]), None);
    assert_unbound(
        &repo,
        "no-model",
        &output,
        "model-unrecorded",
        "the audit records no model to bind the assessment to",
    );
}

#[test]
fn an_assessment_without_identities_leaves_the_debt_open() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "no-identities");
    let output = audit_under(
        &repo,
        "no-identities",
        Some("fixture-other#high"),
        None,
        None,
    );
    assert_unbound(
        &repo,
        "no-identities",
        &output,
        "identities-unstated",
        "the assessment names no model identities for its reviewer",
    );
}

#[test]
fn an_eligible_assessment_does_not_excuse_the_author_auditing() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "self");
    let file = assessment(&repo, "self.json", "eligible", &[]);
    // `tester` snapshotted the work: the existing independence rule still binds.
    repo.arc(&repo.root)
        .args([
            "audit",
            "self",
            "--verdict",
            "approved",
            "--assessment",
            file.to_str().unwrap(),
        ])
        .assert()
        .failure();
}

#[test]
fn other_kinds_discharge_exactly_as_before() {
    let repo = repo_forbidding_self_approval();
    read_change(&repo, "plain");
    repo.arc(&repo.root)
        .args(["integrate", "plain", "--debt", "owed"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args(["audit", "plain", "--verdict", "approved"])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&repo.root).args(["status", "plain", "--json"]));
    assert_eq!(status["debt_outstanding"], false, "{status}");
    assert!(status["debt"].get("ineligible").is_none(), "{status}");
}

#[test]
fn the_reads_show_the_qualifiers_reviewer_basis_and_route_version() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "shown");

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    let row = &inbox["debt-owed"][0];
    assert_eq!(row["debt_missing"], "ineligible-review", "{inbox}");
    assert_eq!(
        row["debt_ineligible"]["qualifiers"],
        serde_json::json!(["same-family"]),
        "{row}"
    );
    assert_eq!(inbox["debt-owed-by-kind"][0]["kind"], "ineligible-review");

    let line = "ineligible read: same-family; reviewer Reviewer (fixture-opus#medium); declared; route 2026.10.12";
    for command in [["inbox"].as_slice(), ["catchup"].as_slice()] {
        let text = stdout(repo.arc(&repo.root).args(command));
        assert!(text.contains(line), "{command:?}: {text}");
    }

    let rows = json_stdout(repo.arc(&repo.root).args(["query", "--debt", "--json"]));
    assert_eq!(
        rows[0]["debt_ineligible"]["reviewer_model"], SAME_FAMILY_REVIEWER,
        "{rows}"
    );

    let backlog = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--json"]),
    );
    let project = backlog["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["anchor"].as_str().unwrap().ends_with("repo"))
        .unwrap_or_else(|| panic!("project missing: {backlog}"));
    let debt = &project["debt_owed"][0];
    assert_eq!(debt["missing"], "ineligible-review", "{debt}");
    assert_eq!(debt["ineligible"]["assessment"], "declared", "{debt}");
    let text = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(text.contains(line), "{text}");

    let explained = json_stdout(repo.arc(&repo.root).args(["explain", "shown", "--json"]));
    let rows = explained["coverage_at_acceptance"]["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("{explained}"));
    let debt = rows.iter().find(|row| row["item"] == "debt").unwrap();
    assert_eq!(debt["ineligible"]["route_version"], "2026.10.12", "{debt}");
    let text = stdout(repo.arc(&repo.root).args(["explain", "shown"]));
    assert!(text.contains(line), "{text}");

    let touching = repo.home.join(".worktrees/repo-touches");
    stdout(repo.arc(&repo.root).args(["begin", "touches"]));
    repo.commit(&touching, "shown.txt", "work\nmore\n", "feat: touches");
    stdout(repo.arc(&touching).args(["snapshot", "touches"]));
    let advisory = stdout_any_status(repo.arc(&touching).args(["check", "touches"]));
    assert!(advisory.contains(line), "{advisory}");
    let show = stdout(repo.arc(&repo.root).args(["show", "shown"]));
    assert!(show.contains("same-family"), "{show}");
}

#[test]
fn within_the_kind_a_below_floor_read_is_worked_first() {
    let repo = repo_forbidding_self_approval();
    // The older debt has the milder qualifier, so age alone would put it first.
    read_change(&repo, "mild");
    integrate_ineligible(&repo, "mild", &["--qualifier", "same-family"]).success();
    read_change(&repo, "weak");
    integrate_ineligible(
        &repo,
        "weak",
        &["--qualifier", "same-family", "--qualifier", "below-floor"],
    )
    .success();

    let order = stdout(repo.arc(&repo.root).args(["query", "--debt"]));
    let mut lines = order.lines();
    let first = json_stdout(repo.arc(&repo.root).args(["status", "weak", "--json"]));
    assert_eq!(lines.next().unwrap(), first["change_id"].as_str().unwrap());
}

#[test]
fn a_kind_weaker_than_independent_review_and_stronger_than_contributor_only_sorts_between() {
    let repo = repo_forbidding_self_approval();
    owed_change(&repo, "named-read");
    read_change(&repo, "independent");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "independent",
            "--debt",
            "x",
            "--kind",
            "independent-review",
        ])
        .assert()
        .success();
    read_change(&repo, "contrib");
    repo.arc(&repo.root)
        .args([
            "integrate",
            "contrib",
            "--debt",
            "x",
            "--kind",
            "contributor-only",
        ])
        .assert()
        .success();
    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    let kinds: Vec<&str> = inbox["debt-owed-by-kind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "contributor-only",
            "ineligible-review",
            "independent-review"
        ],
        "{inbox}"
    );
}

#[test]
fn arc_debt_after_integration_takes_the_same_inputs() {
    let repo = repo_forbidding_self_approval();
    read_change(&repo, "late");
    repo.arc(&repo.root)
        .args(["integrate", "late", "--debt", "first"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "debt",
            "late",
            "--reason",
            "named",
            "--kind",
            "ineligible-review",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--qualifier"));
    repo.arc(&repo.root)
        .args([
            "debt",
            "late",
            "--reason",
            "named",
            "--kind",
            "ineligible-review",
            "--qualifier",
            "floor-unknown",
        ])
        .assert()
        .success();
    assert_eq!(
        debt_of(&repo, "late")["ineligible"]["qualifiers"],
        serde_json::json!(["floor-unknown"])
    );
}
