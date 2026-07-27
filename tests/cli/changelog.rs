use crate::common::*;
use predicates::prelude::*;

fn begin(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])))
}

fn record(repo: &Repo, cwd: &Path, slug: &str, section: &str, body: &str) {
    repo.arc(cwd)
        .args(["changelog", slug, "--section", section, "--body-file", "-"])
        .write_stdin(body)
        .assert()
        .success();
}

fn integrate(repo: &Repo, slug: &str) {
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: {slug}"),
    );
    repo.arc(&worktree)
        .args(["snapshot", slug])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

#[test]
fn recording_then_reading_round_trips_section_and_body() {
    let repo = Repo::new();
    begin(&repo, "roundtrip");
    let worktree = repo.home.join(".worktrees/repo-roundtrip");
    record(&repo, &worktree, "roundtrip", "fixed", "- fixed it\n");
    let value: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&worktree).args([
        "changelog",
        "roundtrip",
        "--json",
    ])))
    .unwrap();
    assert_eq!(value["section"], "fixed");
    assert_eq!(value["body"], "- fixed it\n");
}

#[test]
fn rerecording_replaces_the_derived_entry_and_keeps_both_events() {
    let repo = Repo::new();
    let change_id = begin(&repo, "replace");
    let worktree = repo.home.join(".worktrees/repo-replace");
    record(&repo, &worktree, "replace", "added", "- first\n");
    record(&repo, &worktree, "replace", "changed", "- second\n");
    let value: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&worktree).args([
        "changelog",
        "replace",
        "--json",
    ])))
    .unwrap();
    assert_eq!(value["section"], "changed");
    assert_eq!(value["body"], "- second\n");
    let count = fs::read_dir(event_dir(&repo, &change_id))
        .unwrap()
        .filter(|entry| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(entry.as_ref().unwrap().path()).unwrap()).unwrap();
            value["event_type"] == "changelog-recorded"
        })
        .count();
    assert_eq!(count, 2);
}

#[test]
fn projection_includes_integrated_and_excludes_open_changes() {
    let repo = Repo::new();
    begin(&repo, "integrated");
    let integrated = repo.home.join(".worktrees/repo-integrated");
    record(&repo, &integrated, "integrated", "added", "- shipped\n");
    integrate(&repo, "integrated");
    begin(&repo, "open");
    let open = repo.home.join(".worktrees/repo-open");
    record(&repo, &open, "open", "added", "- not yet\n");
    repo.arc(&repo.root)
        .args(["changelog"])
        .assert()
        .stdout(predicate::str::contains("- shipped"))
        .stdout(predicate::str::contains("- not yet").not());
}

#[test]
fn recording_after_integration_updates_the_projection() {
    let repo = Repo::new();
    let change_id = begin(&repo, "late-entry");
    integrate(&repo, "late-entry");
    let before = event_count(&repo, &change_id);
    record(
        &repo,
        &repo.root,
        "late-entry",
        "fixed",
        "- documented after integration\n",
    );
    assert_eq!(event_count(&repo, &change_id), before + 1);
    let projection = json_stdout(repo.arc(&repo.root).args(["changelog", "--json"]));
    let entry = projection
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["change"] == "late-entry")
        .unwrap();
    assert_eq!(entry["section"], "fixed");
    assert_eq!(entry["body"], "- documented after integration\n");

    begin(&repo, "abandoned-entry");
    repo.arc(&repo.root)
        .args(["close", "abandoned-entry", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "changelog",
            "abandoned-entry",
            "--section",
            "fixed",
            "--body-file",
            "-",
        ])
        .write_stdin("- not shipped\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("is abandoned"))
        .stderr(predicate::str::contains(
            "changelog entries require an open or integrated change",
        ));

    begin(&repo, "replacement-entry");
    begin(&repo, "superseded-entry");
    repo.arc(&repo.root)
        .args([
            "close",
            "superseded-entry",
            "--superseded",
            "replacement-entry",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "changelog",
            "superseded-entry",
            "--section",
            "fixed",
            "--body-file",
            "-",
        ])
        .write_stdin("- superseded\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("is superseded"))
        .stderr(predicate::str::contains(
            "changelog entries require an open or integrated change",
        ));
}

#[test]
fn projection_honors_the_release_boundary() {
    let repo = Repo::new();
    begin(&repo, "released");
    let released = repo.home.join(".worktrees/repo-released");
    record(&repo, &released, "released", "fixed", "- old\n");
    integrate(&repo, "released");
    git(&repo.root, &["tag", "v1"]);
    begin(&repo, "new");
    let new = repo.home.join(".worktrees/repo-new");
    record(&repo, &new, "new", "fixed", "- new\n");
    integrate(&repo, "new");
    repo.arc(&repo.root)
        .args(["changelog"])
        .assert()
        .stdout(predicate::str::contains("- new"))
        .stdout(predicate::str::contains("- old").not());
}

#[test]
fn projection_groups_sections_in_keep_a_changelog_order() {
    let repo = Repo::new();
    for (slug, section, body) in [
        ("security", "security", "- secure\n"),
        ("added", "added", "- add\n"),
        ("removed", "removed", "- remove\n"),
    ] {
        begin(&repo, slug);
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        record(&repo, &worktree, slug, section, body);
        integrate(&repo, slug);
    }
    let output = stdout(repo.arc(&repo.root).args(["changelog"]));
    assert!(output.find("### Added").unwrap() < output.find("### Removed").unwrap());
    assert!(output.find("### Removed").unwrap() < output.find("### Security").unwrap());
    assert!(!output.contains("### Changed"));
}

#[test]
fn write_splices_only_unreleased_and_is_idempotent() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\nold\n\n## [1.0.0]\n\nreleased bytes\n",
        "docs: add changelog",
    );
    begin(&repo, "write");
    let worktree = repo.home.join(".worktrees/repo-write");
    record(&repo, &worktree, "write", "added", "- projected\n");
    integrate(&repo, "write");
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .success();
    let once = fs::read(repo.root.join("CHANGELOG.md")).unwrap();
    assert!(String::from_utf8_lossy(&once).contains("- projected"));
    assert!(once.ends_with(b"## [1.0.0]\n\nreleased bytes\n"));
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .success();
    assert_eq!(fs::read(repo.root.join("CHANGELOG.md")).unwrap(), once);
}

#[test]
fn reviewer_role_is_refused_when_recording() {
    let repo = Repo::new();
    begin(&repo, "roles");
    let worktree = repo.home.join(".worktrees/repo-roles");
    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args([
            "changelog",
            "roles",
            "--section",
            "added",
            "--body-file",
            "-",
        ])
        .write_stdin("- nope\n")
        .assert()
        .code(9)
        .stderr(predicate::str::contains(
            "role refusal: reviewer may not changelog",
        ));
}
