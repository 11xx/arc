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

/// Integrate the change, handing back what the integration wrote to stderr,
/// where its advisories appear.
fn integrate(repo: &Repo, slug: &str) -> String {
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
    let out = repo
        .arc(&repo.root)
        .args(["integrate", slug])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stderr).into_owned()
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
        "# News\n\n## [Unreleased]\n\n## [1.0.0]\n\nreleased\n",
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
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(
            "NEWS.md holds no `## [Unreleased]` heading",
        ));
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
        .stderr(predicate::str::contains(".arc/changelog.toml"))
        .stderr(predicate::str::contains(
            "renderer `command` requires a non-empty renderer_command",
        ));
}

#[test]
fn write_splices_only_unreleased_and_is_idempotent() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\n## [1.0.0]\n\nreleased bytes\n",
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
fn write_fills_an_unreleased_block_that_ends_the_file() {
    for (name, original) in [
        ("trailing-newline", "# Changelog\n\n## [Unreleased]\n"),
        ("no-trailing-newline", "# Changelog\n\n## [Unreleased]"),
    ] {
        let repo = Repo::new();
        repo.commit(&repo.root, "CHANGELOG.md", original, "docs: add changelog");
        begin(&repo, "first-release");
        let worktree = repo.home.join(".worktrees/repo-first-release");
        record(
            &repo,
            &worktree,
            "first-release",
            "added",
            "- first entry\n",
        );
        integrate(&repo, "first-release");

        let projected = stdout(repo.arc(&repo.root).args(["changelog"]));
        repo.arc(&repo.root)
            .args(["changelog", "--write"])
            .assert()
            .success()
            .stdout("");
        let once = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
        assert_eq!(once, format!("# Changelog\n\n{projected}"), "{name}");
        repo.arc(&repo.root)
            .args(["changelog", "--write"])
            .assert()
            .success();
        assert_eq!(
            fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap(),
            once,
            "{name}"
        );
    }
}

#[test]
fn write_refuses_a_target_the_built_in_renderer_cannot_update() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## 1.0.0\n\nreleased\n",
        "docs: add changelog",
    );
    begin(&repo, "unshaped");
    let worktree = repo.home.join(".worktrees/repo-unshaped");
    record(&repo, &worktree, "unshaped", "added", "- projected\n");
    integrate(&repo, "unshaped");

    let before = fs::read(repo.root.join("CHANGELOG.md")).unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("CHANGELOG.md"))
        .stderr(predicate::str::contains("## [Unreleased]"))
        .stderr(predicate::str::contains("nothing was written"))
        .stderr(predicate::str::contains(
            "can select renderer = \"command\" with a renderer_command in .arc/changelog.toml",
        ));
    assert_eq!(fs::read(repo.root.join("CHANGELOG.md")).unwrap(), before);

    fs::remove_file(repo.root.join("CHANGELOG.md")).unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("CHANGELOG.md does not exist"))
        .stderr(predicate::str::contains("nothing was written"));
    assert!(!repo.root.join("CHANGELOG.md").exists());
}

#[test]
fn write_accepts_a_recorded_entry_wrapped_at_another_column() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- an entry whose prose the ledger holds\n  exactly, wrapped in the file at a column the\n  projection does not choose\n\n## [1.0.0]\n\nreleased\n",
        "docs: add changelog",
    );
    begin(&repo, "rewrapped");
    let worktree = repo.home.join(".worktrees/repo-rewrapped");
    record(
        &repo,
        &worktree,
        "rewrapped",
        "added",
        "an entry whose prose the ledger holds exactly, wrapped in the file at a column \
         the projection does not choose\n",
    );
    integrate(&repo, "rewrapped");

    let projected = stdout(repo.arc(&repo.root).args(["changelog"]));
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .success()
        .stdout("");
    let written = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
    let block = projected.strip_prefix("## [Unreleased]\n").unwrap();
    assert!(written.contains(block), "{written}");
    assert!(!written.contains("ledger holds\n"), "{written}");
    assert!(!written.contains("<!-- unrecorded -->"), "{written}");
    assert!(written.ends_with("## [1.0.0]\n\nreleased\n"), "{written}");
}

#[test]
fn write_accepts_a_recorded_multi_paragraph_entry_wrapped_at_another_column() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\n### Added\n\n\
         - the opening paragraph of a body the ledger holds,\n  wrapped here at a column of its own\n\n\
         \x20 a second paragraph of the same entry, indented\n  under the bullet it belongs to\n\n\
         ## [1.0.0]\n\nreleased\n",
        "docs: add changelog",
    );
    begin(&repo, "paragraphs");
    let worktree = repo.home.join(".worktrees/repo-paragraphs");
    record(
        &repo,
        &worktree,
        "paragraphs",
        "added",
        "the opening paragraph of a body the ledger holds, wrapped here at a column of \
         its own\n\na second paragraph of the same entry, indented under the bullet it \
         belongs to\n",
    );
    integrate(&repo, "paragraphs");

    let projected = stdout(repo.arc(&repo.root).args(["changelog"]));
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .success();
    let written = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
    assert!(
        written.contains(projected.strip_prefix("## [Unreleased]\n").unwrap()),
        "{written}"
    );
    assert!(!written.contains("<!-- unrecorded -->"), "{written}");
}

#[test]
fn write_names_the_paragraph_it_cannot_account_for() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\n### Added\n\n\
         - hand-written prose the ledger never saw,\n  running past one line\n\n\
         ### Removed\n\n- an entry no ledger holds\n\n\
         ## [1.0.0]\n\nreleased\n",
        "docs: add changelog",
    );
    begin(&repo, "named");
    let worktree = repo.home.join(".worktrees/repo-named");
    record(&repo, &worktree, "named", "added", "- projected\n");
    integrate(&repo, "named");

    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "  - hand-written prose the ledger never saw, ...",
        ))
        // A heading the projection emits is accounted for; one for a category
        // holding no recorded entry is not.
        .stderr(predicate::str::contains("  ### Removed"))
        .stderr(predicate::str::contains("### Added").not());

    repo.arc(&repo.root)
        .args(["changelog", "--write", "--keep-unrecorded"])
        .assert()
        .success();
    let kept = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
    assert!(
        kept.contains(
            "<!-- unrecorded -->\n\n- hand-written prose the ledger never saw,\n  running past one line\n\n### Removed\n\n- an entry no ledger holds\n\n### Added\n\n- projected\n"
        ),
        "{kept}"
    );
}

#[test]
fn write_refuses_unrecorded_prose_and_keeps_it_on_request() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- hand-written prose\n\n## [1.0.0]\n\nreleased\n",
        "docs: add changelog",
    );
    begin(&repo, "guarded");
    let worktree = repo.home.join(".worktrees/repo-guarded");
    record(&repo, &worktree, "guarded", "added", "- projected\n");
    integrate(&repo, "guarded");

    let before = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "CHANGELOG.md holds prose no recorded changelog entry produced",
        ))
        .stderr(predicate::str::contains("  - hand-written prose"))
        // A heading the projection emits itself is accounted for.
        .stderr(predicate::str::contains("### Added").not());
    assert_eq!(
        fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap(),
        before
    );

    repo.arc(&repo.root)
        .args(["changelog", "--write", "--keep-unrecorded"])
        .assert()
        .success();
    let kept = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
    assert!(
        kept.contains(
            "## [Unreleased]\n\n<!-- unrecorded -->\n\n- hand-written prose\n\n### Added\n\n- projected\n"
        ),
        "{kept}"
    );
    assert!(kept.ends_with("## [1.0.0]\n\nreleased\n"));

    repo.arc(&repo.root)
        .args(["changelog", "--write", "--keep-unrecorded"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap(),
        kept
    );

    repo.arc(&repo.root)
        .args(["changelog", "--keep-unrecorded"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--keep-unrecorded applies only to --write",
        ));
}

#[test]
fn integration_advises_when_the_change_recorded_no_entry() {
    let repo = Repo::new();
    begin(&repo, "undocumented");
    let advice = integrate(&repo, "undocumented");
    assert!(
        advice.contains("advice: no changelog entry on")
            && advice.contains("arc changelog undocumented --category CATEGORY --body-file FILE"),
        "{advice}"
    );

    begin(&repo, "documented");
    let worktree = repo.home.join(".worktrees/repo-documented");
    record(&repo, &worktree, "documented", "added", "- documented\n");
    let quiet = integrate(&repo, "documented");
    assert!(!quiet.contains("advice: no changelog entry"), "{quiet}");
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

/// A repository whose `.arc/changelog.toml` selects a command renderer that
/// runs `render.sh`, holding one integrated entry. The script is the
/// project's program, so the fixture writes it into the repository.
fn command_renderer_repo(script: &str, extra_config: &str) -> Repo {
    let repo = Repo::new();
    fs::create_dir(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/changelog.toml"),
        format!(
            "target = \"NEWS\"\nrenderer = \"command\"\nrenderer_command = [\"sh\", \"render.sh\"]\n{extra_config}"
        ),
    )
    .unwrap();
    fs::write(repo.root.join("render.sh"), script).unwrap();
    git(&repo.root, &["add", "."]);
    git(
        &repo.root,
        &["commit", "-m", "chore: configure a news renderer"],
    );
    begin(&repo, "rendered");
    let worktree = repo.home.join(".worktrees/repo-rendered");
    record(&repo, &worktree, "rendered", "added", "- rendered entry\n");
    integrate(&repo, "rendered");
    repo
}

fn read_request(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn command_renderer_writes_a_target_in_its_own_format() {
    let repo = command_renderer_repo(
        "cat > \"$REQUEST_OUT\"\nprintf 'NEWS\\n====\\n\\n  * rendered entry\\n'\n",
        "",
    );
    let request_out = repo.home.join("request.json");
    let projection = json_stdout(repo.arc(&repo.root).args(["changelog", "--json"]));
    assert_eq!(projection["renderer"], "command");

    repo.arc(&repo.root)
        .env("REQUEST_OUT", &request_out)
        .args(["changelog", "--write"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(
        fs::read_to_string(repo.root.join("NEWS")).unwrap(),
        "NEWS\n====\n\n  * rendered entry\n"
    );
    let request = read_request(&request_out);
    assert_eq!(request["schema"], "arc-changelog-render-request/1");
    assert_eq!(request["operation"], "write");
    assert_eq!(request["target"], "NEWS");
    assert_eq!(request["target_content"], serde_json::Value::Null);
    assert_eq!(request["include_provenance"], false);
    assert_eq!(request["projection"], projection);

    fs::write(repo.root.join("NEWS"), "old news\n").unwrap();
    repo.arc(&repo.root)
        .env("REQUEST_OUT", &request_out)
        .args(["changelog", "--write"])
        .assert()
        .success();
    assert_eq!(read_request(&request_out)["target_content"], "old news\n");
    assert_eq!(
        fs::read_to_string(repo.root.join("NEWS")).unwrap(),
        "NEWS\n====\n\n  * rendered entry\n"
    );
}

#[test]
fn command_renderer_answers_a_plain_read() {
    let repo = command_renderer_repo("cat > \"$REQUEST_OUT\"\nprintf 'rendered view\\n'\n", "");
    let request_out = repo.home.join("request.json");
    repo.arc(&repo.root)
        .env("REQUEST_OUT", &request_out)
        .args(["changelog", "--provenance"])
        .assert()
        .success()
        .stdout("rendered view\n");
    let request = read_request(&request_out);
    assert_eq!(request["operation"], "render");
    assert_eq!(request["target_content"], serde_json::Value::Null);
    assert_eq!(request["include_provenance"], true);
    assert!(!repo.root.join("NEWS").exists());

    // `--json` is the projection itself and never runs the renderer.
    fs::remove_file(&request_out).unwrap();
    repo.arc(&repo.root)
        .env("REQUEST_OUT", &request_out)
        .args(["changelog", "--json"])
        .assert()
        .success();
    assert!(!request_out.exists());
}

#[test]
fn command_renderer_failures_leave_the_target_untouched() {
    for (name, script, cause) in [
        (
            "non-zero exit",
            "cat > /dev/null\necho 'renderer broke' >&2\nexit 3\n",
            "exited with status 3",
        ),
        ("empty output", "cat > /dev/null\n", "printed nothing"),
        (
            "non-UTF-8 output",
            "cat > /dev/null\nprintf '\\377\\376\\n'\n",
            "printed output that is not UTF-8",
        ),
        (
            "oversized output",
            "cat > /dev/null\nhead -c 16777217 /dev/zero | tr '\\0' 'x'\n",
            "printed more than 16 MiB",
        ),
    ] {
        let repo = command_renderer_repo(script, "");
        fs::write(repo.root.join("NEWS"), "kept\n").unwrap();
        let assert = repo
            .arc(&repo.root)
            .args(["changelog", "--write"])
            .assert()
            .code(1)
            .stdout("")
            .stderr(predicate::str::contains(cause))
            .stderr(predicate::str::contains("NEWS was not written"));
        if name == "non-zero exit" {
            assert.stderr(predicate::str::contains("renderer broke"));
        }
        assert_eq!(
            fs::read_to_string(repo.root.join("NEWS")).unwrap(),
            "kept\n",
            "{name}"
        );
    }

    let repo = command_renderer_repo("exit 0\n", "");
    fs::write(
        repo.root.join(".arc/changelog.toml"),
        "target = \"NEWS\"\nrenderer = \"command\"\nrenderer_command = [\"./no-such-renderer\"]\n",
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["changelog", "--write"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("could not start"));
    assert!(!repo.root.join("NEWS").exists());
}

#[test]
fn command_renderer_is_killed_with_its_group_at_the_deadline() {
    let repo = command_renderer_repo(
        "cat > /dev/null\nsleep 30 &\necho $! > \"$PID_OUT\"\nwait\n",
        "renderer_timeout = \"1s\"\n",
    );
    let pid_out = repo.home.join("renderer.pid");
    fs::write(repo.root.join("NEWS"), "kept\n").unwrap();
    let started = std::time::Instant::now();
    repo.arc(&repo.root)
        .env("PID_OUT", &pid_out)
        .args(["changelog", "--write"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("did not finish within 1s"))
        .stderr(predicate::str::contains("NEWS was not written"));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(
        fs::read_to_string(repo.root.join("NEWS")).unwrap(),
        "kept\n"
    );

    // The background child shared the renderer's process group, so the kill
    // reached it: it is gone, or a zombie awaiting its new parent's reap.
    let pid = fs::read_to_string(&pid_out).unwrap().trim().to_owned();
    let stat = Path::new("/proc").join(&pid).join("stat");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let alive = fs::read_to_string(&stat)
            .map(|stat| !stat.contains(") Z "))
            .unwrap_or(false);
        if !alive {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "renderer child {pid} survived"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn renderer_configuration_is_checked_at_load() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join(".arc")).unwrap();
    for (config, refusal) in [
        (
            "renderer = \"command\"\nrenderer_command = []\n",
            "renderer `command` requires a non-empty renderer_command",
        ),
        (
            "renderer = \"keep-a-changelog\"\nrenderer_command = [\"sh\", \"render.sh\"]\n",
            "renderer_command applies only to renderer `command`",
        ),
        (
            "renderer = \"keep-a-changelog\"\nrenderer_timeout = \"5s\"\n",
            "renderer_timeout applies only to renderer `command`",
        ),
        (
            "renderer = \"command\"\nrenderer_command = [\"sh\"]\nrenderer_timeout = \"soon\"\n",
            "renderer_timeout",
        ),
        (
            "renderer = \"pandoc\"\n",
            "unsupported changelog renderer `pandoc`",
        ),
    ] {
        fs::write(repo.root.join(".arc/changelog.toml"), config).unwrap();
        repo.arc(&repo.root)
            .args(["changelog"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(".arc/changelog.toml"))
            .stderr(predicate::str::contains(refusal));
    }
}

#[test]
fn keep_unrecorded_belongs_to_the_built_in_renderer() {
    let repo = command_renderer_repo("cat > /dev/null\necho news\n", "");
    repo.arc(&repo.root)
        .args(["changelog", "--write", "--keep-unrecorded"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--keep-unrecorded applies only to the keep-a-changelog renderer",
        ));
    assert!(!repo.root.join("NEWS").exists());
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn write_keeps_the_permissions_an_existing_target_had() {
    let repo = Repo::new();
    repo.commit(
        &repo.root,
        "CHANGELOG.md",
        "# Changelog\n\n## [Unreleased]\n",
        "docs: add changelog",
    );
    begin(&repo, "moded");
    let worktree = repo.home.join(".worktrees/repo-moded");
    record(&repo, &worktree, "moded", "added", "- kept mode\n");
    integrate(&repo, "moded");

    let changelog = repo.root.join("CHANGELOG.md");
    set_mode(&changelog, 0o666);
    repo.arc_under_umask(&repo.root, "022")
        .args(["changelog", "--write"])
        .assert()
        .success();
    assert!(fs::read_to_string(&changelog)
        .unwrap()
        .contains("- kept mode"));
    assert_eq!(mode_of(&changelog), 0o666);

    // Through a symlink, the file it names is replaced and keeps its mode;
    // the link stays a link to the same file.
    fs::create_dir(repo.root.join("docs")).unwrap();
    fs::rename(&changelog, repo.root.join("docs/CHANGES.md")).unwrap();
    std::os::unix::fs::symlink("docs/CHANGES.md", &changelog).unwrap();
    let named = repo.root.join("docs/CHANGES.md");
    fs::write(&named, "# Changelog\n\n## [Unreleased]\n").unwrap();
    set_mode(&named, 0o660);
    repo.arc_under_umask(&repo.root, "022")
        .args(["changelog", "--write"])
        .assert()
        .success();
    assert_eq!(
        fs::read_link(&changelog).unwrap(),
        Path::new("docs/CHANGES.md")
    );
    assert!(fs::read_to_string(&named).unwrap().contains("- kept mode"));
    assert_eq!(mode_of(&named), 0o660);
}

#[test]
fn command_renderer_keeps_an_existing_mode_and_creates_under_the_umask() {
    let repo = command_renderer_repo("cat > /dev/null\necho news\n", "");
    let news = repo.root.join("NEWS");
    repo.arc_under_umask(&repo.root, "027")
        .args(["changelog", "--write"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&news).unwrap(), "news\n");
    assert_eq!(mode_of(&news), 0o640);

    set_mode(&news, 0o666);
    repo.arc_under_umask(&repo.root, "022")
        .args(["changelog", "--write"])
        .assert()
        .success();
    assert_eq!(mode_of(&news), 0o666);
}
