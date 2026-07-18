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

#[test]
fn brief_record_and_read_round_trip() {
    let repo = Repo::new();
    begin(&repo, "brief-roundtrip");
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
        .stdout(v1);

    record(&repo, "brief-roundtrip", v2, None);
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip"])
        .assert()
        .success()
        .stdout(v2);
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip", "--version", "1"])
        .assert()
        .success()
        .stdout(v1);
    repo.arc(&repo.root)
        .args(["brief", "brief-roundtrip", "--version", "3"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("brief version 3 not found"));
}

#[test]
fn brief_requires_body_flag_semantics() {
    let repo = Repo::new();
    let change_id = begin(&repo, "brief-flags");
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
            .stdout("lead contract\n");
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
    record(&repo, "brief-show", "old body\n", None);
    record(&repo, "brief-show", "current body\n", Some("Current"));
    repo.arc(&repo.root)
        .args(["show", "brief-show"])
        .assert()
        .success()
        .stdout(predicates::str::contains("## Brief (v2)"))
        .stdout(predicates::str::contains("current body"));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "brief-show"]))).unwrap();
    assert_eq!(status["brief"]["version"], 2);
    assert_eq!(status["brief"]["title"], "Current");
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
fn brief_closed_change_refuses_new_versions() {
    let repo = Repo::new();
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
        .stdout("still readable\n");
}

#[test]
fn brief_survives_export_import() {
    let source = Repo::new();
    begin(&source, "brief-bundle");
    record(
        &source,
        "brief-bundle",
        "portable contract\n",
        Some("Portable"),
    );
    let bundle = source.home.join("brief-bundle.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "brief-bundle",
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
        .success()
        .stdout(predicates::str::contains("unknown event type").not());
    destination
        .arc(&destination.root)
        .args(["brief", "brief-bundle"])
        .assert()
        .success()
        .stdout("portable contract\n");
}
