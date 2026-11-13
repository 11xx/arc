use super::common::*;

fn begin(repo: &Repo, slug: &str, file: &str) -> PathBuf {
    repo.arc(&repo.root)
        .args(["begin", slug])
        .assert()
        .success();
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    fs::create_dir_all(wt.join(file).parent().unwrap()).unwrap();
    repo.commit(&wt, file, slug, "test: covered content");
    repo.arc(&wt).args(["snapshot"]).assert().success();
    wt
}

fn counts(repo: &Repo, wt: &Path) -> (usize, usize) {
    let state = json_stdout(repo.arc(wt).args(["show", "--json"]));
    let rows = state["verifications"].as_array().unwrap();
    let reused = rows.iter().filter(|r| r["base_pass"].is_object()).count();
    (rows.len() - reused, reused)
}

/// Claim: the project's declarations skip only warmed people-only changes and
/// execute every required gate for source, embedded text and protected inputs.
/// Rejects inert source, unnamed source coverage and cold documentation skips.
/// Commands are markers: this checks dispatch, not compilation or gate budgets.
#[test]
fn project_paths_cover_source_and_warmed_documentation_skips_execution() {
    let repo = Repo::new();
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".arc/gates.toml")).unwrap();
    let mut declarations: toml::Value = toml::from_str(&text).unwrap();
    for (_, gate) in declarations["gates"].as_table_mut().unwrap().iter_mut() {
        gate["command"] = toml::Value::String("true".into());
        gate.as_table_mut().unwrap().remove("timings");
    }
    repo.redeclare_gates(&toml::to_string(&declarations).unwrap());
    let warm = begin(&repo, "warm-project-gates", "README.md");
    repo.arc(&warm).args(["verify", "--all"]).assert().success();
    assert_eq!(counts(&repo, &warm), (3, 0), "cold documentation executes");
    repo.arc(&warm)
        .args(["review", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "warm-project-gates"])
        .assert()
        .success();
    let docs = begin(&repo, "warm-docs", "notes.md");
    repo.arc(&docs).args(["verify", "--all"]).assert().success();
    assert_eq!(
        counts(&repo, &docs),
        (0, 3),
        "warm documentation executes nothing"
    );
    for (slug, file) in [
        ("source", "src/commands/timeline.rs"),
        ("embedded-text", "src/commands/scaffolds/discussion.md"),
        ("gate-input", ".arc/notes.md"),
    ] {
        let wt = begin(&repo, slug, file);
        let paths = json_stdout(repo.arc(&wt).args(["gates", "paths", "--json"]));
        let row = paths["paths"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == file)
            .unwrap();
        assert_eq!(
            row["unclaimed"], false,
            "source coverage must be explicit: {row}"
        );
        assert_eq!(row["inert"], false, "source must not be inert: {row}");
        assert_eq!(
            row["gates"].as_array().unwrap().len(),
            4,
            "publication coverage: {row}"
        );
        repo.arc(&wt).args(["verify", "--all"]).assert().success();
        assert_eq!(
            counts(&repo, &wt),
            (3, 0),
            "covered changes execute every integration gate"
        );
    }
}
