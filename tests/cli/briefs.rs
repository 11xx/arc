use super::common::*;

fn begin(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        slug,
        "--no-worktree",
    ])))
}

fn record(repo: &Repo, slug: &str, body: &str, title: Option<&str>) {
    let mut command = repo.arc(&repo.root);
    command.args(["brief", slug, "--body-file", "-"]);
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
        .args(["brief", "brief-roles", "--body-file", "-"])
        .write_stdin("lead update\n")
        .assert()
        .success();
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
        .args(["brief", "anchored-brief", "--body-file", "-"])
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
        .stderr(predicates::str::contains("is closed"));
    repo.arc(&repo.root)
        .args(["brief", "brief-closed"])
        .assert()
        .success()
        .stdout(format!("base-revision: {base_revision}\nstill readable\n"));
}
