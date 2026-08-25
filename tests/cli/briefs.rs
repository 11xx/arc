use super::common::*;

fn begin(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        slug,
        "--no-worktree",
    ])))
}

fn record(repo: &Repo, slug: &str, body: &str, title: Option<&str>) {
    record_versioned(repo, slug, body, title, false)
}

/// Versions after the first require a recorded cause. Fixtures that only need
/// a second version supply an external one, so the requirement stays exercised
/// rather than bypassed.
fn record_versioned(repo: &Repo, slug: &str, body: &str, title: Option<&str>, revision: bool) {
    let mut command = repo.arc(&repo.root);
    command.args(["brief", slug, "--body-file", "-"]);
    if revision {
        command.args(["--cause-note", "fixture revision"]);
    }
    if let Some(title) = title {
        command.args(["--title", title]);
    }
    command.write_stdin(body).assert().success();
}

fn plan(repo: &Repo, topic: &str) -> String {
    let path = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                topic,
                "--kind",
                "plan",
                "--body-file",
                "-",
            ])
            .write_stdin("# Plan\n"),
    );
    Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn brief_record_and_read_round_trip() {
    let repo = Repo::new();
    let base_revision = repo.head(&repo.root);
    begin(&repo, "brief-roundtrip");
    let first_plan = plan(&repo, "first-plan");
    let second_plan = plan(&repo, "second-plan");
    let v1 = "# First\n\nKeep this exact.\n";
    let v2 = "# Second\n\nReplace the latest.\n";

    let output = repo
        .arc(&repo.root)
        .args([
            "brief",
            "brief-roundtrip",
            "--body-file",
            "-",
            "--title",
            "Contract",
            "--plan-ref",
            &first_plan,
            "--plan-slice",
            "first-slice",
        ])
        .write_stdin(v1)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("brief: v1\n"));
    assert!(output.contains("event: "));
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip"])
        .assert()
        .success()
        .stdout(format!(
            "base-revision: {base_revision}\nplan-ref: {first_plan}\nplan-slice: first-slice\n\n{v1}"
        ));

    repo.arc(&repo.root)
        .args([
            "brief",
            "brief-roundtrip",
            "--body-file",
            "-",
            "--cause-note",
            "fixture revision",
            "--plan-ref",
            &second_plan,
            "--plan-slice",
            "second-slice",
        ])
        .write_stdin(v2)
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip"])
        .assert()
        .success()
        .stdout(format!(
            "base-revision: {base_revision}\nplan-ref: {second_plan}\nplan-slice: second-slice\n\n{v2}"
        ));
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip", "--version", "1"])
        .assert()
        .success()
        .stdout(format!(
            "base-revision: {base_revision}\nplan-ref: {first_plan}\nplan-slice: first-slice\n\n{v1}"
        ));
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip", "--version", "3"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("brief version 3 not found"));

    begin(&repo, "brief-unlinked");
    record(&repo, "brief-unlinked", "ordinary brief\n", None);
    repo.arc(&repo.root)
        .args(["brief", "brief-unlinked"])
        .assert()
        .success()
        .stdout(format!("base-revision: {base_revision}\nordinary brief\n"));
}

#[test]
fn brief_requires_body_flag_semantics() {
    let repo = Repo::new();
    let change_id = begin(&repo, "brief-flags");
    let plan_ref = plan(&repo, "flag-plan");
    for args in [
        vec!["--plan-ref", plan_ref.as_str()],
        vec!["--plan-slice", "only-slice"],
    ] {
        repo.arc(&repo.root)
            .args(["brief", "brief-flags", "--body-file", "-"])
            .args(args)
            .write_stdin("invalid\n")
            .assert()
            .failure()
            .stderr(predicates::str::contains("must be provided together"));
    }
    repo.arc(&repo.root)
        .args([
            "brief",
            "brief-flags",
            "--body-file",
            "-",
            "--plan-ref",
            "20990101T000000Z-missing-plan.md",
            "--plan-slice",
            "missing",
        ])
        .write_stdin("invalid\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no such artifact"));
    let note_path = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "not-plan",
                "--kind",
                "note",
                "--body-file",
                "-",
            ])
            .write_stdin("note\n"),
    );
    let note_ref = Path::new(note_path.trim())
        .file_name()
        .unwrap()
        .to_string_lossy();
    repo.arc(&repo.root)
        .args([
            "brief",
            "brief-flags",
            "--body-file",
            "-",
            "--plan-ref",
            &note_ref,
            "--plan-slice",
            "wrong-kind",
        ])
        .write_stdin("invalid\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a plan"));
    repo.arc(&repo.root)
        .args([
            "brief",
            "brief-flags",
            "--body-file",
            "-",
            "--plan-ref",
            "../plan.md",
            "--plan-slice",
            "path-shaped",
        ])
        .write_stdin("invalid\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a path"));
    repo.arc(&repo.root)
        .args(["brief", "brief-flags", "--title", "No body"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--title requires --body-file"));
    repo.arc(&repo.root)
        .args(["brief", "brief-flags"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("no brief recorded"));
    let before = event_count(&repo, &change_id);
    repo.arc(&repo.root)
        .args(["brief", "brief-flags", "--body-file", "missing.md"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot read body file"));
    assert_eq!(event_count(&repo, &change_id), before);
}

#[test]
fn brief_write_is_lead_only() {
    let repo = Repo::new();
    let base_revision = repo.head(&repo.root);
    begin(&repo, "brief-roles");
    record(&repo, "brief-roles", "lead contract\n", None);
    for role in ["implementer", "reviewer"] {
        repo.arc(&repo.root)
            .env("ARC_ROLE", role)
            .args(["brief", "brief-roles", "--body-file", "-"])
            .write_stdin("forbidden\n")
            .assert()
            .code(9)
            .stderr(predicates::str::contains("role refusal"));
        repo.arc(&repo.root)
            .env("ARC_ROLE", role)
            .args(["brief", "brief-roles"])
            .assert()
            .success()
            .stdout(format!("base-revision: {base_revision}\nlead contract\n"));
    }
    repo.arc(&repo.root)
        .env("ARC_ROLE", "lead")
        .args([
            "brief",
            "brief-roles",
            "--body-file",
            "-",
            "--cause-note",
            "fixture revision",
        ])
        .write_stdin("lead update\n")
        .assert()
        .success();
}

#[test]
fn brief_cause_is_canonical_validated_and_required_after_v1() {
    let source = Repo::new();
    let change_id = begin(&source, "brief-causes");
    record(&source, "brief-causes", "initial contract\n", None);
    let finding = stdout(source.arc(&source.root).args([
        "finding",
        "brief-causes",
        "--summary",
        "contract premise is false",
    ]));
    let finding_id = finding
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap();
    let finding_event = finding
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();
    let before = event_count(&source, &change_id);

    source
        .arc(&source.root)
        .args(["brief", "brief-causes", "--body-file", "-"])
        .write_stdin("uncausally revised\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "brief v2 requires at least one cause",
        ));
    source
        .arc(&source.root)
        .args([
            "brief",
            "brief-causes",
            "--body-file",
            "-",
            "--caused-by",
            &format!("verdict:{}", &finding_event[..12]),
        ])
        .write_stdin("wrong event type\n")
        .assert()
        .failure()
        // A finding event is not a verdict at all, and saying so beats claiming
        // it is a verdict of the wrong kind.
        .stderr(predicates::str::contains("no verdict matches"));
    assert_eq!(event_count(&source, &change_id), before);

    source
        .arc(&source.root)
        .args([
            "brief",
            "brief-causes",
            "--body-file",
            "-",
            "--caused-by",
            &format!("finding:{}", &finding_id[..12]),
        ])
        .write_stdin("corrected contract\n")
        .assert()
        .success();

    let state = json_stdout(
        source
            .arc(&source.root)
            .args(["show", "brief-causes", "--json"]),
    );
    assert_eq!(
        state["briefs"][1]["caused_by"],
        serde_json::json!([{"kind": "finding", "finding_id": finding_id}])
    );

    let bundle = source.home.join("brief-causes.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "brief-causes",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let imported = json_stdout(
        destination
            .arc(&destination.root)
            .args(["show", &change_id, "--json"]),
    );
    assert_eq!(
        imported["briefs"][1]["caused_by"],
        state["briefs"][1]["caused_by"]
    );
}

/// A cause is resolved once and stored canonically in an append-only event, so
/// an ambiguous prefix has to refuse. Picking the first candidate would record
/// the wrong relationship permanently, and nothing downstream could tell.
#[test]
fn an_ambiguous_cause_prefix_refuses_rather_than_picking_one() {
    let repo = Repo::new();
    let change_id = begin(&repo, "ambiguous-cause");
    record(&repo, "ambiguous-cause", "initial contract\n", None);
    let mut finding_ids = Vec::new();
    for summary in ["first premise is false", "second premise is false"] {
        let out =
            stdout(
                repo.arc(&repo.root)
                    .args(["finding", "ambiguous-cause", "--summary", summary]),
            );
        finding_ids.push(
            out.lines()
                .find_map(|line| line.strip_prefix("finding: "))
                .unwrap()
                .to_string(),
        );
    }
    let shared = finding_ids[0]
        .chars()
        .zip(finding_ids[1].chars())
        .take_while(|(a, b)| a == b)
        .count();
    assert!(
        shared > 0,
        "identifiers share no prefix, so the case is untested: {finding_ids:?}"
    );
    let before = event_count(&repo, &change_id);
    repo.arc(&repo.root)
        .args([
            "brief",
            "ambiguous-cause",
            "--body-file",
            "-",
            "--caused-by",
            &format!("finding:{}", &finding_ids[0][..shared]),
        ])
        .write_stdin("revised\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains("ambiguous finding"));
    assert_eq!(event_count(&repo, &change_id), before);

    // The full identifier still resolves, so the refusal is about ambiguity
    // rather than a resolver that stopped working.
    repo.arc(&repo.root)
        .args([
            "brief",
            "ambiguous-cause",
            "--body-file",
            "-",
            "--caused-by",
            &format!("finding:{}", finding_ids[1]),
        ])
        .write_stdin("revised\n")
        .assert()
        .success();
}

/// Every other write-only brief option refuses on the read path. Accepting a
/// cause there would let an author believe a revision was justified on the
/// record when the command only printed the existing brief.
#[test]
fn causes_require_a_write_like_every_other_write_only_brief_option() {
    let repo = Repo::new();
    begin(&repo, "read-path-causes");
    record(&repo, "read-path-causes", "initial contract\n", None);
    for args in [
        vec!["--caused-by", "external:whatever"],
        vec!["--cause-note", "a reason"],
    ] {
        repo.arc(&repo.root)
            .args(["brief", "read-path-causes"])
            .args(&args)
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "--caused-by and --cause-note require --body-file or --scaffold",
            ));
    }
}

#[test]
fn brief_shows_in_show_and_status() {
    let repo = Repo::new();
    begin(&repo, "brief-show");
    let plan_ref = plan(&repo, "show-plan");
    record(&repo, "brief-show", "old body\n", None);
    repo.arc(&repo.root)
        .args([
            "brief",
            "brief-show",
            "--body-file",
            "-",
            "--cause-note",
            "fixture revision",
            "--title",
            "Current",
            "--plan-ref",
            &plan_ref,
            "--plan-slice",
            "status-link",
        ])
        .write_stdin("current body\n")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["show", "brief-show"])
        .assert()
        .success()
        .stdout(predicates::str::contains("## Brief (v2)"))
        .stdout(predicates::str::contains(format!("- Plan: `{plan_ref}`")))
        .stdout(predicates::str::contains("- Slice: `status-link`"))
        .stdout(predicates::str::contains("current body"));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "brief-show"]))).unwrap();
    assert_eq!(status["brief"]["version"], 2);
    assert_eq!(status["brief"]["title"], "Current");
    assert_eq!(status["brief"]["plan_ref"], plan_ref);
    assert_eq!(status["brief"]["plan_slice"], "status-link");
    assert!(status["brief"]["recorded_at"].is_string());

    begin(&repo, "brief-none");
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "brief-none"]))).unwrap();
    assert!(status["brief"].is_null());
    repo.arc(&repo.root)
        .args(["show", "brief-none"])
        .assert()
        .success()
        .stdout(predicates::str::contains("## Brief").not());
}

#[test]
fn brief_base_is_resolved_at_write_time_and_does_not_follow_head() {
    let repo = Repo::new();
    let revision_a = repo.head(&repo.root);
    begin(&repo, "anchored-brief");
    repo.arc(&repo.root)
        .args(["brief", "anchored-brief", "--body-file", "-"])
        .write_stdin("contract at A\n")
        .assert()
        .success();

    repo.commit(
        &repo.root,
        "revision-b.txt",
        "revision B\n",
        "test: advance to B",
    );
    let revision_b = repo.head(&repo.root);
    repo.arc(&repo.root)
        .args([
            "brief",
            "anchored-brief",
            "--body-file",
            "-",
            "--cause-note",
            "fixture revision",
        ])
        .write_stdin("contract at B\n")
        .assert()
        .success();

    repo.commit(
        &repo.root,
        "revision-c.txt",
        "revision C\n",
        "test: advance to C",
    );
    repo.arc(&repo.root)
        .args(["brief", "anchored-brief", "--version", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "base-revision: {revision_a}"
        )))
        .stdout(predicates::str::contains("contract at A"));
    repo.arc(&repo.root)
        .args(["brief", "anchored-brief", "--version", "2"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "base-revision: {revision_b}"
        )))
        .stdout(predicates::str::contains("contract at B"));

    repo.arc(&repo.root)
        .args([
            "brief",
            "anchored-brief",
            "--body-file",
            "-",
            "--cause-note",
            "fixture revision",
            "--base",
            "HEAD~2",
        ])
        .write_stdin("explicitly anchored\n")
        .assert()
        .success();
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "anchored-brief", "--json"]),
    );
    assert_eq!(status["brief"]["base_revision"], revision_a);

    let artifact = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "seed-anchor",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("seeded contract\n"),
    );
    let artifact = Path::new(artifact.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    repo.arc(&repo.root)
        .args([
            "begin",
            "seed-anchor",
            "--no-worktree",
            "--base",
            "HEAD~2",
            "--from-journal",
            &artifact,
        ])
        .assert()
        .success();
    let seeded = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "seed-anchor", "--json"]),
    );
    assert_eq!(seeded["base"], revision_a);
    assert_eq!(seeded["brief"]["base_revision"], revision_a);
}

#[test]
fn brief_base_drift() {
    let repo = Repo::new();
    let base_revision = repo.head(&repo.root);
    let change_id = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "brief-base-drift"]),
    ));
    let worktree = repo.home.join(".worktrees").join("repo-brief-base-drift");
    repo.arc(&repo.root)
        .args([
            "brief",
            &change_id,
            "--body-file",
            "-",
            "--base",
            &base_revision,
        ])
        .write_stdin("brief citations\n")
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["brief", &change_id])
        .assert()
        .success()
        .stdout(predicates::str::contains("line citations").not());

    repo.commit(
        &worktree,
        "brief-base-drift-one.txt",
        "one\n",
        "test: advance brief base drift once",
    );
    repo.commit(
        &worktree,
        "brief-base-drift-two.txt",
        "two\n",
        "test: advance brief base drift twice",
    );

    let annotation = "**2 commits behind the change head**; line citations may have decayed";
    repo.arc(&repo.root)
        .args(["brief", &change_id])
        .assert()
        .success()
        .stdout(predicates::str::contains(annotation));
    repo.arc(&repo.root)
        .args(["resume", &change_id])
        .assert()
        .success()
        .stdout(predicates::str::contains(annotation));
}

#[test]
fn brief_closed_change_refuses_new_versions() {
    let repo = Repo::new();
    let base_revision = repo.head(&repo.root);
    begin(&repo, "brief-closed");
    record(&repo, "brief-closed", "still readable\n", None);
    repo.arc(&repo.root)
        .args(["close", "brief-closed", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["brief", "brief-closed", "--body-file", "-"])
        .write_stdin("too late\n")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "is abandoned; event is open-only",
        ));
    repo.arc(&repo.root)
        .args(["brief", "brief-closed"])
        .assert()
        .success()
        .stdout(format!("base-revision: {base_revision}\nstill readable\n"));
}
