use crate::common::*;
use predicates::prelude::*;

fn begin(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])))
}

fn record(repo: &Repo, cwd: &Path, slug: &str, category: &str, body: &str) {
    repo.arc(cwd)
        .args([
            "changelog",
            slug,
            "--category",
            category,
            "--body-file",
            "-",
        ])
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
    assert_eq!(value["entries"][0]["category"], "fixed");
    assert_eq!(value["entries"][0]["body"], "- fixed it\n");
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
    assert_eq!(value["entries"][0]["category"], "changed");
    assert_eq!(value["entries"][0]["body"], "- second\n");
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
    let entry = projection["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["change"] == "late-entry")
        .unwrap();
    assert_eq!(entry["category"], "fixed");
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
            "--category",
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
            "--category",
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
fn json_projection_always_carries_full_event_provenance() {
    let repo = Repo::new();
    begin(&repo, "provenance-entry");
    let worktree = repo.home.join(".worktrees/repo-provenance-entry");
    let recorded = stdout(
        repo.arc(&worktree)
            .env("ARC_ACTOR", "Changelog Lead")
            .env("ARC_HARNESS", "claude")
            .env("ARC_SESSION", "session-provenance")
            .args([
                "--on-behalf-of",
                "Release Executor",
                "changelog",
                "provenance-entry",
                "--category",
                "fixed",
                "--body-file",
                "-",
            ])
            .write_stdin("- provenance survives projection\n"),
    );
    let event_id = recorded
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();
    integrate(&repo, "provenance-entry");

    let project = json_stdout(repo.arc(&repo.root).args(["changelog", "--json"]));
    assert_eq!(project["schema"], "arc-changelog/1");
    assert!(project["boundary"].is_null());
    assert_eq!(project["target"], "CHANGELOG.md");
    assert_eq!(project["renderer"], "keep-a-changelog");
    let entry = project["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["change"] == "provenance-entry")
        .unwrap();
    assert_eq!(entry["category"], "fixed");
    assert_eq!(entry["body"], "- provenance survives projection\n");
    assert!(entry["integrated_commit"].is_string());
    assert!(entry["integrated_at"].is_string());
    assert_eq!(entry["recorded"]["event_id"], event_id);
    assert_eq!(entry["recorded"]["actor"], "Changelog Lead");
    assert_eq!(entry["recorded"]["on_behalf_of"], "Release Executor");
    assert_eq!(entry["recorded"]["effective_author"], "Release Executor");
    assert_eq!(entry["recorded"]["harness"], "claude");
    assert_eq!(entry["recorded"]["session"], "session-provenance");
    assert!(entry["recorded"]["created_at"].is_string());

    let single =
        json_stdout(
            repo.arc(&repo.root)
                .args(["changelog", "provenance-entry", "--json"]),
        );
    assert_eq!(single["schema"], "arc-changelog/1");
    assert!(single["boundary"].is_null());
    assert_eq!(single["entries"].as_array().unwrap().len(), 1);
    assert_eq!(single["entries"][0]["recorded"], entry["recorded"]);

    let clean = stdout(repo.arc(&repo.root).args(["changelog", "provenance-entry"]));
    assert!(!clean.contains("arc provenance:"), "{clean}");
    let annotated =
        stdout(
            repo.arc(&repo.root)
                .args(["changelog", "provenance-entry", "--provenance"]),
        );
    for expected in [
        "arc provenance: change=provenance-entry",
        &format!("event={event_id}"),
        "actor=Changelog Lead",
        "on_behalf_of=Release Executor",
        "harness=claude",
        "session=session-provenance",
    ] {
        assert!(annotated.contains(expected), "{annotated}");
    }
    repo.arc(&repo.root)
        .args(["changelog", "provenance-entry", "--json", "--provenance"])
        .assert()
        .failure()
        .code(2);
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
fn free_form_categories_render_after_canonical_categories() {
    let repo = Repo::new();
    for (slug, category, body) in [
        ("fixed-category", "fIxEd", "- fixed\n"),
        ("highlights-category", "  Highlights  ", "- highlighted\n"),
        ("api-category", "API Notes", "- api\n"),
        ("added-category", "added", "- added\n"),
    ] {
        begin(&repo, slug);
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.arc(&worktree)
            .args([
                "changelog",
                slug,
                "--category",
                category,
                "--body-file",
                "-",
            ])
            .write_stdin(body)
            .assert()
            .success();
        integrate(&repo, slug);
    }

    let output = stdout(repo.arc(&repo.root).args(["changelog"]));
    for heading in ["### Added", "### Fixed", "### API Notes", "### Highlights"] {
        assert!(output.contains(heading), "{output}");
    }
    assert!(output.find("### Added").unwrap() < output.find("### Fixed").unwrap());
    assert!(output.find("### Fixed").unwrap() < output.find("### API Notes").unwrap());
    assert!(output.find("### API Notes").unwrap() < output.find("### Highlights").unwrap());
    assert!(!output.contains("### fIxEd"), "{output}");

    let entry =
        json_stdout(
            repo.arc(&repo.root)
                .args(["changelog", "highlights-category", "--json"]),
        );
    assert_eq!(entry["entries"][0]["category"], "Highlights");

    let change_id = opened_change_id(&stdout(
        repo.arc(&repo.root).args(["begin", "legacy-category"]),
    ));
    let worktree = repo.home.join(".worktrees/repo-legacy-category");
    repo.arc(&worktree)
        .args([
            "changelog",
            "legacy-category",
            "--category",
            "Legacy",
            "--body-file",
            "-",
        ])
        .write_stdin("- legacy\n")
        .assert()
        .success();
    let event_path = fs::read_dir(event_dir(&repo, &change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let event: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            event["event_type"] == "changelog-recorded"
        })
        .unwrap();
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(&event_path).unwrap()).unwrap();
    legacy["section"] = legacy.as_object_mut().unwrap().remove("category").unwrap();
    fs::write(&event_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
    let replayed =
        json_stdout(
            repo.arc(&worktree)
                .args(["changelog", "legacy-category", "--json"]),
        );
    assert_eq!(replayed["entries"][0]["category"], "Legacy");

    repo.arc(&repo.root)
        .args(["changelog", "legacy-category", "--section", "fixed"])
        .assert()
        .failure()
        .code(2);
    for malformed in ["   ", "Line One\nLine Two"] {
        repo.arc(&worktree)
            .args([
                "changelog",
                "legacy-category",
                "--category",
                malformed,
                "--body-file",
                "-",
            ])
            .write_stdin("- invalid\n")
            .assert()
            .failure();
    }
}

#[test]
fn configured_target_uses_keep_a_changelog_renderer() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/changelog.toml"),
        "target = \"NEWS.md\"\nrenderer = \"keep-a-changelog\"\n",
    )
    .unwrap();
    fs::write(
        repo.root.join("NEWS.md"),
        "# News\n\n## [Unreleased]\n\nold\n\n## [1.0.0]\n\nreleased\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "docs: configure news"]);

    begin(&repo, "configured-write");
    let worktree = repo.home.join(".worktrees/repo-configured-write");
    record(
        &repo,
        &worktree,
        "configured-write",
        "Highlights",
        "- configured target\n",
    );
    integrate(&repo, "configured-write");

    let projection = json_stdout(repo.arc(&repo.root).args(["changelog", "--json"]));
    assert_eq!(projection["target"], "NEWS.md");
    assert_eq!(projection["renderer"], "keep-a-changelog");
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .success()
        .stdout("");
    let written = fs::read_to_string(repo.root.join("NEWS.md")).unwrap();
    assert!(written.contains("### Highlights\n\n- configured target"));
    assert!(written.ends_with("## [1.0.0]\n\nreleased\n"));
    assert!(!repo.root.join("CHANGELOG.md").exists());

    fs::write(
        repo.root.join("NEWS.md"),
        "format arc does not understand\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .success()
        .stdout(predicate::str::contains("## [Unreleased]"))
        .stdout(predicate::str::contains("- configured target"));
    assert_eq!(
        fs::read_to_string(repo.root.join("NEWS.md")).unwrap(),
        "format arc does not understand\n"
    );

    let outside = repo.root.parent().unwrap().join("outside.md");
    fs::write(&outside, "outside\n").unwrap();
    fs::write(
        repo.root.join(".arc/changelog.toml"),
        "target = \"../outside.md\"\nrenderer = \"keep-a-changelog\"\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "changelog target must stay inside the repository",
        ));
    assert_eq!(fs::read_to_string(outside).unwrap(), "outside\n");

    fs::write(
        repo.root.join(".arc/changelog.toml"),
        "target = \"NEWS.md\"\nrenderer = \"command\"\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["changelog"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "unsupported changelog renderer `command`",
        ));
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
            "--category",
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
