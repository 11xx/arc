use super::common::*;

#[test]
fn malformed_plan_records_no_credit_and_correction_is_shared() {
    let repo = Repo::new();
    let path =
        stdout(
            repo.arc(&repo.root)
                .args(["journal", "plan", "repair-credit", "--title", "Plan"]),
        );
    let file = Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    fs::write(path.trim(), "# Plan\nplanned-by: {broken\n\nBody\n").unwrap();
    begin_change(&repo, "malformed-credit", None);
    repo.arc(&repo.root)
        .args([
            "brief",
            "malformed-credit",
            "--plan-ref",
            file,
            "--plan-slice",
            "one",
            "--body-file",
            "-",
        ])
        .write_stdin("# Brief\n")
        .assert()
        .success();
    let before = json_stdout(
        repo.arc(&repo.root)
            .args(["brief", "malformed-credit", "--json"]),
    );
    assert_eq!(
        before["brief"]["plan_source"]["planner_status"],
        "malformed"
    );
    repo.arc(&repo.root)
        .args([
            "journal",
            "correct",
            file,
            "--target",
            "artifact",
            "--field",
            "planners",
            "--value",
            r#"[{"actor":"human"}]"#,
            "--note",
            "correct author",
        ])
        .assert()
        .success();
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", file, "--json"]),
    );
    assert_eq!(view["planner_status"], "corrected");
    assert_eq!(view["planners"][0]["actor"], "human");
    assert_eq!(
        before,
        json_stdout(
            repo.arc(&repo.root)
                .args(["brief", "malformed-credit", "--json"])
        )
    );
}

#[test]
fn selected_plan_captures_author_instead_of_brief_recorder() {
    let repo = Repo::new();
    let path = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "planner-a")
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", "plan-session")
            .env("ARC_MODEL", "planner-model#high")
            .args(["journal", "plan", "authorship", "--body-file", "-"])
            .write_stdin("# Authorship\n\nImplement a slice.\n"),
    );
    let file = Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let bytes = fs::read(path.trim()).unwrap();
    begin_change(&repo, "selected", None);
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "implementer-b")
        .args([
            "brief",
            "selected",
            "--plan-ref",
            file,
            "--plan-slice",
            "one",
            "--body-file",
            "-",
        ])
        .write_stdin("# Slice one\n")
        .assert()
        .success();
    let view = json_stdout(repo.arc(&repo.root).args(["brief", "selected", "--json"]));
    assert_eq!(view["schema"], "arc-brief/1");
    let source = &view["brief"]["plan_source"];
    assert_eq!(source["sha256"], hex::encode(Sha256::digest(&bytes)));
    assert_eq!(source["planners"][0]["actor"], "planner-a");
    assert_eq!(source["planners"][0]["session"], "plan-session");
}

#[test]
fn portable_planners_survive_transitions_and_text_suggests_credit() {
    let repo = Repo::new();
    let path = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "author-a")
            .env("ARC_MODEL", "model-a#high")
            .args(["journal", "plan", "portable", "--title", "Portable"]),
    );
    assert!(fs::read_to_string(path.trim())
        .unwrap()
        .contains("planned-by:"));
    let file = Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    repo.arc(&repo.root)
        .args(["journal", "transition", file, "--to", "later"])
        .assert()
        .success();
    let later = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "latest", "portable", "--kind", "later", "--json"]),
    );
    repo.arc(&repo.root)
        .args([
            "journal",
            "transition",
            later["file"].as_str().unwrap(),
            "--to",
            "plan",
            "--planned-by",
            r#"{"actor":"coauthor"}"#,
        ])
        .assert()
        .success();
    let open = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "open", "--kind", "plan", "--json"]),
    );
    let file = open["open"][0]["file"].as_str().unwrap();
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", file, "--json"]),
    );
    assert_eq!(view["planners"].as_array().unwrap().len(), 2);
    begin_change(&repo, "credit", None);
    repo.arc(&repo.root)
        .args([
            "brief",
            "credit",
            "--plan-ref",
            file,
            "--plan-slice",
            "one",
            "--body-file",
            "-",
        ])
        .write_stdin("# Brief\n")
        .assert()
        .success();
    let text = stdout(repo.arc(&repo.root).args(["brief", "credit"]));
    assert!(text.contains("plan-sha256:"));
    assert!(text.contains("Assisted-by: test:model-a#high (planner)"));
}

#[test]
fn represented_human_does_not_inherit_model_and_fenced_examples_are_not_metadata() {
    let repo = Repo::new();
    let path = stdout(repo.arc(&repo.root).env("ARC_MODEL", "model#high").args([
        "journal",
        "plan",
        "human",
        "--on-behalf-of",
        "human",
        "--title",
        "Plan",
    ]));
    let file = Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", file, "--json"]),
    );
    assert_eq!(view["planners"][0], serde_json::json!({"actor":"human"}));
    fs::write(
        path.trim(),
        "# Plan\n\n```md\n# Example\nplanned-by: {broken\n```\n",
    )
    .unwrap();
    let view = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "show", file, "--json"]),
    );
    assert_ne!(view["planner_status"], "malformed");
}
