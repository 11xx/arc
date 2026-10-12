use super::common::*;
use super::ineligible_review::{
    assessment, debt_of, read_by, read_change, repo_forbidding_self_approval, snapshotted,
};

const REASON: &str = "review owed";

/// Every event file in the repository's ledger, whichever change or scope
/// holds it.
fn ledger_events(repo: &Repo) -> usize {
    fn count(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| {
                if path.is_dir() {
                    count(&path)
                } else {
                    usize::from(
                        path.parent().and_then(|parent| parent.file_name())
                            == Some("events".as_ref()),
                    )
                }
            })
            .sum()
    }
    count(&repo.root.join(".git/arc"))
}

fn integrate(repo: &Repo, slug: &str, dry_run: bool, extra: &[&str]) -> std::process::Output {
    let mut command = repo.arc(&repo.root);
    command
        .args(["integrate", slug, "--debt", REASON])
        .args(extra);
    if dry_run {
        command.arg("--dry-run");
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_dry_run_shows_each_debt_kind_and_writes_nothing() {
    for kind in [
        "nothing-read",
        "merge-resolution-unread",
        "repair-unread",
        "contributor-only",
        "independent-review",
    ] {
        let repo = repo_forbidding_self_approval();
        snapshotted(&repo, "planned");
        let before = ledger_events(&repo);
        let plan = integrate(&repo, "planned", true, &["--kind", kind]);
        assert!(plan.status.success(), "{kind}: {}", text(&plan.stderr));
        let out = text(&plan.stdout);
        assert!(
            out.contains(&format!("would declare debt for ps-01 ({kind}): {REASON}")),
            "{kind}: {out}"
        );
        assert!(out.contains("would integrate"), "{kind}: {out}");
        assert_eq!(ledger_events(&repo), before, "{kind}");
        assert!(debt_of(&repo, "planned").is_null(), "{kind}");
    }
}

#[test]
fn a_dry_run_derives_the_kind_the_real_run_would_record() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "derived");
    let plan = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        "derived",
        "--debt",
        REASON,
        "--dry-run",
        "--json",
    ]));
    assert_eq!(plan["debt"]["kind"], "nothing-read", "{plan}");
    integrate(&repo, "derived", false, &[]);
    assert_eq!(debt_of(&repo, "derived")["missing"], "nothing-read");
}

#[test]
fn a_dry_run_shows_the_named_read_qualifiers_and_assessment_source() {
    let repo = repo_forbidding_self_approval();
    let read = read_change(&repo, "ineligible");
    let before = ledger_events(&repo);
    let args = [
        "--kind",
        "ineligible-review",
        "--qualifier",
        "same-family",
        "--qualifier",
        "below-floor",
        "--route-version",
        "2026.10.12",
    ];
    let out = text(&integrate(&repo, "ineligible", true, &args).stdout);
    assert!(
        out.contains("would declare debt for ps-01 (ineligible-review)"),
        "{out}"
    );
    assert!(out.contains(&read), "{out}");
    assert!(out.contains("same-family, below-floor"), "{out}");
    assert!(out.contains("declared"), "{out}");

    let mut json_args = vec![
        "integrate",
        "ineligible",
        "--debt",
        REASON,
        "--dry-run",
        "--json",
    ];
    json_args.extend(args);
    let plan = json_stdout(repo.arc(&repo.root).args(&json_args));
    let debt = &plan["debt"];
    assert_eq!(debt["disposition"], "would-declare", "{plan}");
    assert_eq!(debt["kind"], "ineligible-review", "{plan}");
    assert_eq!(debt["reason"], REASON, "{plan}");
    assert_eq!(debt["patchset_id"], "ps-01", "{plan}");
    assert!(debt["event_id"].is_null(), "{plan}");
    let record = &debt["ineligible"];
    assert_eq!(record["read"], read.as_str(), "{plan}");
    assert_eq!(
        record["qualifiers"],
        serde_json::json!(["same-family", "below-floor"]),
        "{plan}"
    );
    assert_eq!(record["assessment"], "declared", "{plan}");
    assert_eq!(record["route_version"], "2026.10.12", "{plan}");
    assert_eq!(ledger_events(&repo), before);

    let file = assessment(&repo, "assess.json", "ineligible", &["same-model"]);
    let assessed = json_stdout(repo.arc(&repo.root).args([
        "integrate",
        "ineligible",
        "--debt",
        REASON,
        "--kind",
        "ineligible-review",
        "--assessment",
        file.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]));
    assert_eq!(assessed["debt"]["ineligible"]["assessment"], "assessed");
    assert_eq!(ledger_events(&repo), before);
}

#[test]
fn a_plan_without_a_debt_carries_none() {
    let repo = repo_forbidding_self_approval();
    read_by_approved(&repo, "plain");
    let plan =
        json_stdout(
            repo.arc(&repo.root)
                .args(["integrate", "plain", "--dry-run", "--json"]),
        );
    assert!(plan.get("debt").is_none(), "{plan}");
}

/// A change approved by a reviewer the policy accepts.
fn read_by_approved(repo: &Repo, slug: &str) {
    snapshotted(repo, slug);
    read_by(repo, slug, "Reviewer", "fixture-opus#medium");
}

#[test]
fn a_dry_run_reports_the_refusal_the_real_run_gives_and_writes_nothing() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "unread");
    read_change(&repo, "eligible");
    read_change(&repo, "shape");
    let eligible = assessment(&repo, "eligible.json", "eligible", &[]);
    let eligible = eligible.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "unread",
            vec!["--kind", "ineligible-review", "--qualifier", "same-model"],
        ),
        (
            "eligible",
            vec!["--kind", "ineligible-review", "--assessment", eligible],
        ),
        ("shape", vec!["--kind", "ineligible-review"]),
        (
            "shape",
            vec![
                "--kind",
                "ineligible-review",
                "--qualifier",
                "same-model",
                "--read",
                "01NOSUCHEVENT",
            ],
        ),
        (
            "shape",
            vec!["--kind", "independent-review", "--qualifier", "same-model"],
        ),
        (
            "shape",
            vec![
                "--qualifier",
                "smaller-brain",
                "--kind",
                "ineligible-review",
            ],
        ),
    ];
    for (slug, extra) in cases {
        let before = ledger_events(&repo);
        for json in [false, true] {
            let mut dry_args = vec!["--dry-run"];
            if json {
                dry_args.push("--json");
            }
            dry_args.extend(&extra);
            let planned = integrate(&repo, slug, true, &dry_args[1..]);
            let real = integrate(&repo, slug, false, &extra);
            assert!(
                !planned.status.success(),
                "{slug} {extra:?}: {}",
                text(&planned.stdout)
            );
            assert_eq!(
                planned.status.code(),
                real.status.code(),
                "{slug} {extra:?}"
            );
            assert_eq!(
                text(&planned.stderr),
                text(&real.stderr),
                "{slug} {extra:?}"
            );
            if json {
                assert!(planned.stdout.is_empty(), "{}", text(&planned.stdout));
            }
        }
        assert_eq!(ledger_events(&repo), before, "{slug} {extra:?}");
    }
}

#[test]
fn a_dry_run_agrees_with_the_real_run_on_head_policy_and_basis() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "agreed");
    let plan_path = repo.home.join("plan.json");
    let plan = stdout(repo.arc(&repo.root).args([
        "integrate",
        "agreed",
        "--debt",
        REASON,
        "--dry-run",
        "--json",
    ]));
    fs::write(&plan_path, &plan).unwrap();
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert!(
        plan["authorization"].get("audit_debt_event_id").is_none(),
        "{plan}"
    );
    assert!(
        plan["authorization"].get("verdict_event_id").is_none(),
        "{plan}"
    );

    let real = integrate(
        &repo,
        "agreed",
        false,
        &["--expect-basis", plan_path.to_str().unwrap()],
    );
    assert!(real.status.success(), "{}", text(&real.stderr));
    assert!(
        !text(&real.stderr).contains("moved"),
        "{}",
        text(&real.stderr)
    );
    let state = json_stdout(repo.arc(&repo.root).args(["show", "agreed", "--json"]));
    let recorded = &state["closure"]["authorization"];
    assert!(state["closure"]["integrated_commit"].is_string(), "{state}");
    assert_eq!(
        recorded["audit_debt_event_id"], state["debt"]["event_id"],
        "{state}"
    );
    let mut expected = plan["authorization"].clone();
    expected["audit_debt_event_id"] = recorded["audit_debt_event_id"].clone();
    assert_eq!(recorded, &expected, "{state}");
    assert_eq!(
        state["closure"]["source_head"], plan["approved_head"],
        "{state}"
    );
}

#[test]
fn a_dry_run_without_a_debt_still_refuses_a_missing_review() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "unreviewed");
    let refused = repo
        .arc(&repo.root)
        .args(["integrate", "unreviewed", "--dry-run"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(text(&refused.stdout).contains("would not integrate"));
}

#[test]
fn a_dry_run_never_projects_debt_past_a_changes_requested_veto() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "vetoed");
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Reviewer").args([
        "review",
        "vetoed",
        "--verdict",
        "changes-requested",
        "--cause",
        "brief",
    ]));
    let before = ledger_events(&repo);
    let planned = integrate(&repo, "vetoed", true, &[]);
    let real = integrate(&repo, "vetoed", false, &[]);
    assert!(!planned.status.success(), "{}", text(&planned.stdout));
    assert!(!real.status.success());
    assert_eq!(planned.status.code(), real.status.code());
    assert!(
        !text(&planned.stdout).contains("would integrate"),
        "{}",
        text(&planned.stdout)
    );
    let json = repo
        .arc(&repo.root)
        .args([
            "integrate",
            "vetoed",
            "--debt",
            REASON,
            "--dry-run",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!json.status.success());
    assert!(json.stdout.is_empty(), "{}", text(&json.stdout));
    // The real run declared its debt before it refused; the dry runs did not.
    assert_eq!(ledger_events(&repo), before + 1);
}

#[test]
fn a_dry_run_reports_a_debt_an_earlier_refusal_kept() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "kept");
    repo.declare_gates_locally("[gates.required]\ncommand='false'\n");
    let refused = integrate(&repo, "kept", false, &[]);
    assert!(!refused.status.success());
    let kept = debt_of(&repo, "kept");
    let event = kept["event_id"].as_str().unwrap();
    let before = ledger_events(&repo);
    let planned = integrate(&repo, "kept", true, &[]);
    let out = text(&planned.stdout);
    assert!(
        out.contains(&format!(
            "debt already declared for ps-01 (nothing-read): {REASON}"
        )),
        "{out}"
    );
    assert!(out.contains(event), "{out}");
    assert_eq!(ledger_events(&repo), before);
}

#[test]
fn a_dry_run_of_an_iterating_change_meets_the_refusal_after_the_debt() {
    let repo = repo_forbidding_self_approval();
    snapshotted(&repo, "iterating");
    stdout(repo.arc(&repo.root).args(["iterating", "iterating"]));
    let before = ledger_events(&repo);
    let planned = integrate(&repo, "iterating", true, &[]);
    assert_eq!(ledger_events(&repo), before);
    let real = integrate(&repo, "iterating", false, &[]);
    assert!(!planned.status.success());
    assert_eq!(planned.status.code(), real.status.code());
    assert!(
        text(&planned.stdout).contains("would declare debt for ps-01"),
        "{}",
        text(&planned.stdout)
    );
    assert!(text(&planned.stdout).contains("would not integrate"));
}
