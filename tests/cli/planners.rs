use super::common::*;

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
