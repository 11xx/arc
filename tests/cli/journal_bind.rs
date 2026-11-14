use super::common::*;

fn slug(path: &Path) -> String {
    path.to_string_lossy().replace(['/', '.'], "-")
}

fn proposed(repo: &Repo, anchor: &Path) -> PathBuf {
    repo.home.join(".local/ai/journals").join(slug(anchor))
}

fn plain(repo: &Repo) -> PathBuf {
    let dir = repo.home.join("project");
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

fn assert_journal_project(repo: &Repo, anchor: &Path, directory: &Path, git: bool) {
    let explanation = stdout(repo.arc(anchor).args(["journal", "dir", "--explain"]));
    assert_eq!(
        explanation,
        format!(
            "source: {}\nanchor: {}\ndirectory: {}\n",
            if git { "git" } else { "recorded-anchor" },
            anchor.display(),
            directory.display()
        )
    );
    let bindings = fs::read_to_string(directory.join("bindings.jsonl")).unwrap();
    let binding: serde_json::Value =
        serde_json::from_str(bindings.lines().next().unwrap()).unwrap();
    assert_eq!(binding["schema"], "journal-binding/1");
    assert_eq!(binding["event"], "bound");
    assert_eq!(binding["anchor"], anchor.display().to_string());

    let todo = stdout(
        repo.arc(anchor)
            .args(["journal", "todo", "queued", "--title", "Queued"]),
    );
    let pin =
        stdout(
            repo.arc(anchor)
                .args(["journal", "memory", "reference", "--title", "Reference"]),
        );
    let pin_file = Path::new(pin.trim()).file_name().unwrap().to_str().unwrap();
    repo.arc(anchor)
        .args(["journal", "pin", pin_file, "--reason", "Standing reference"])
        .assert()
        .success();

    let backlog = json_stdout(repo.arc(anchor).args(["workspace", "backlog", "--json"]));
    assert_eq!(backlog["collection"]["failed"], 0, "{backlog}");
    assert_eq!(backlog["summary"]["projects"], 1);
    assert_eq!(backlog["summary"]["open_items"], if git { 2 } else { 1 });
    assert_eq!(
        backlog["projects"][0]["anchor"],
        anchor.display().to_string()
    );
    assert_eq!(
        backlog["projects"][0]["journal_dir"],
        directory.display().to_string()
    );
    for (name, rows) in backlog["projects"][0]["changes"].as_object().unwrap() {
        if name != "schema" {
            assert!(rows.as_array().unwrap().is_empty(), "{name}: {rows}");
        }
    }
    let pins = json_stdout(repo.arc(anchor).args(["workspace", "pins", "--json"]));
    assert_eq!(pins["projects"][0]["pins"][0]["file"], pin_file, "{pins}");
    let report = json_stdout(repo.arc(anchor).args(["workspace", "report", "--json"]));
    assert_eq!(report["collection"]["failed"], 0, "{report}");
    assert_eq!(report["projects"][0]["work"], if git { 2 } else { 1 });
    assert_eq!(report["projects"][0]["in_flight"], 0);
    assert_eq!(report["projects"][0]["review_owed"], 0);
    let gates = json_stdout(repo.arc(anchor).args(["workspace", "gates", "--json"]));
    assert_eq!(gates["collection"]["failed"], 0, "{gates}");
    assert_eq!(gates["projects"][0]["declares_gates"], false);
    assert!(gates["projects"][0]["report"]["gates"]
        .as_array()
        .unwrap()
        .is_empty());

    let catchup = json_stdout(
        repo.arc(anchor)
            .args(["catchup", "--sessions", "0", "--json"]),
    );
    assert_eq!(catchup["schema"], "arc-catchup/12");
    assert_eq!(
        catchup["journal"]["dir"],
        directory.display().to_string(),
        "{catchup}"
    );
    assert!(
        catchup["journal"]["project_journal_error"].is_null(),
        "{catchup}"
    );
    if !git {
        for field in ["ledger", "replica", "worktrees"] {
            assert!(catchup[field].is_null(), "{catchup}");
        }
        let text = stdout(repo.arc(anchor).args(["catchup", "--sessions", "0"]));
        assert!(
            text.contains("ledger: none (journal project without Git)"),
            "{text}"
        );
        assert!(text.contains("Queued"), "{text}");
        assert!(!anchor.join(".git").exists());
        assert!(!anchor.join(".arc").exists());
        let list = json_stdout(repo.arc(anchor).args(["workspace", "list", "--json"]));
        assert!(list["repos"].as_array().unwrap().is_empty(), "{list}");
    }
    assert!(Path::new(todo.trim()).is_file());
}

/// An empty plain directory binds explicitly and remains a journal project on
/// fresh invocations; rejects a transient path or a missing workspace journal.
#[test]
fn empty_directory_binds_durably() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    let directory = proposed(&repo, &anchor);
    let path = stdout(repo.arc(&anchor).args(["journal", "bind"]));
    assert_eq!(path.trim(), directory.to_str().unwrap());
    assert_journal_project(&repo, &anchor, &directory, false);
    assert!(fs::read_dir(&anchor).unwrap().next().is_none());
}

/// An explicit relative directory binds without altering its existing files;
/// rejects binding the invoking Git checkout or needing config afterwards.
#[test]
fn nonempty_directory_binds_durably() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    fs::write(anchor.join("notes.txt"), "Kept content\n").unwrap();
    fs::write(anchor.join("ordinary-project-notes.md"), "# Notes\n").unwrap();
    let path = stdout(
        repo.arc(&repo.root)
            .args(["journal", "bind", "../home/project"]),
    );
    let directory = proposed(&repo, &anchor);
    assert_eq!(path.trim(), directory.to_str().unwrap());
    assert_journal_project(&repo, &anchor, &directory, false);
    assert_eq!(
        fs::read_to_string(anchor.join("notes.txt")).unwrap(),
        "Kept content\n"
    );
}

fn assert_git_implies_journal(repo: &Repo, anchor: &Path) {
    let directory = proposed(repo, anchor);
    let explanation = stdout(repo.arc(anchor).args(["journal", "dir", "--explain"]));
    assert!(explanation.starts_with("source: git\n"), "{explanation}");
    assert!(!directory.exists());
    repo.arc(anchor)
        .args(["journal", "bind"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already has a journal at"))
        .stderr(predicates::str::contains(directory.display().to_string()));
    assert!(!directory.exists());
    repo.arc(anchor)
        .args(["journal", "todo", "first", "--title", "First"])
        .assert()
        .success();
    assert_journal_project(repo, anchor, &directory, true);
}

/// An unborn Git repository already implies its journal and registers on its
/// first write; rejects HEAD-dependent discovery and redundant binding.
#[test]
fn unborn_repository_implies_journal() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    git(&anchor, &["init", "-q"]);
    assert_git_implies_journal(&repo, &anchor);
}

/// A committed repository needs no ledger to imply and register a journal;
/// rejects a ledger prerequisite or bind taking over the Git journal.
#[test]
fn committed_repository_without_ledger_implies_journal() {
    let repo = Repo::new();
    let anchor = fs::canonicalize(&repo.root).unwrap();
    assert!(!anchor.join(".git/arc").exists());
    assert_git_implies_journal(&repo, &anchor);
}

/// First writes show the durable command and exact proposed journal without
/// binding silently; rejects an unanchored error with no actionable target.
#[test]
fn first_write_names_bind_and_target_without_creating_it() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    for populated in [false, true] {
        if populated {
            fs::write(anchor.join("notes.txt"), "Keep\n").unwrap();
            fs::write(anchor.join("ordinary-project-notes.md"), "# Notes\n").unwrap();
        }
        repo.arc(&anchor)
            .args(["journal", "todo", "first", "--title", "First"])
            .assert()
            .failure()
            .stderr(predicates::str::contains("arc journal bind"))
            .stderr(predicates::str::contains(
                proposed(&repo, &anchor).display().to_string(),
            ));
        assert!(!proposed(&repo, &anchor).exists());
    }
}

/// Existing bindings identify their journal without appending another record;
/// rejects a duplicate binding or an unhelpful already-bound refusal.
#[test]
fn rebinding_names_existing_journal() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    let directory = PathBuf::from(stdout(repo.arc(&anchor).args(["journal", "bind"])).trim());
    let before = fs::read(directory.join("bindings.jsonl")).unwrap();
    repo.arc(&anchor)
        .args(["journal", "bind"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already bound to"))
        .stderr(predicates::str::contains(directory.display().to_string()));
    assert_eq!(fs::read(directory.join("bindings.jsonl")).unwrap(), before);
}

/// A journal and its descendants cannot become project anchors, even when
/// stored outside arc's home; rejects nested journals and their registrations.
#[test]
fn refuses_inside_another_journal() {
    let repo = Repo::new();
    let directory = repo.home.join("external-journal");
    repo.arc(&repo.root)
        .env("ARC_JOURNAL_DIR", &directory)
        .args(["journal", "todo", "first", "--title", "First"])
        .assert()
        .success();
    let child = directory.join("nested");
    fs::create_dir_all(&child).unwrap();
    for anchor in [&directory, &child] {
        repo.arc(&repo.root)
            .args(["journal", "bind", anchor.to_str().unwrap()])
            .assert()
            .failure()
            .stderr(predicates::str::contains("inside journal"));
        assert!(!proposed(&repo, anchor).exists());
    }
}

/// Arc's storage home cannot become a journal project, including through a
/// symlink; rejects registrations anchored in arc's own state directories.
#[test]
fn refuses_inside_arc_home() {
    let repo = Repo::new();
    let anchor = repo.home.join(".local/ai/arc");
    repo.arc(&anchor)
        .args(["journal", "bind"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("inside arc's home"));
    let alias = repo.home.join("alias");
    std::os::unix::fs::symlink(&anchor, &alias).unwrap();
    repo.arc(&repo.root)
        .args(["journal", "bind", alias.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("inside arc's home"));
}

/// Binding requires an existing directory and creates no missing anchor;
/// rejects inventing a project path or accepting a regular file.
#[test]
fn refuses_missing_directory_and_regular_file() {
    let repo = Repo::new();
    let anchor = repo.home.join("missing");
    repo.arc(&repo.root)
        .args(["journal", "bind", anchor.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("it must exist"));
    assert!(!anchor.exists());
    fs::write(&anchor, "File\n").unwrap();
    repo.arc(&repo.root)
        .args(["journal", "bind", anchor.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a directory"));
}

/// Environment-directed writes remain temporary and help names the durable
/// route; rejects recording a binding from an override or claiming it does.
#[test]
fn environment_write_does_not_bind() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    let directory = repo.home.join("override");
    repo.arc(&anchor)
        .env("ARC_JOURNAL_DIR", &directory)
        .args(["journal", "todo", "first", "--title", "First"])
        .assert()
        .success();
    assert!(!directory.join("bindings.jsonl").exists());
    repo.arc(&anchor)
        .args(["journal", "dir", "--explain"])
        .assert()
        .failure();
    for args in [
        vec!["journal", "--help"],
        vec!["journal", "dir", "--help"],
        vec!["journal", "bind", "--help"],
    ] {
        let help = stdout(repo.arc(&anchor).args(args));
        assert!(help.contains("ARC_JOURNAL_DIR"), "{help}");
        assert!(help.contains("binding"), "{help}");
    }
    repo.arc(&anchor)
        .env("ARC_JOURNAL_DIR", &directory)
        .args(["journal", "bind"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "requires ARC_JOURNAL_DIR to be unset",
        ));
    repo.arc(&anchor)
        .args(["journal", "bind"])
        .assert()
        .success();
    assert!(!directory.join("bindings.jsonl").exists());
}

/// Global scope cannot be converted into a directory binding; rejects bind
/// silently ignoring --global and registering a project anyway.
#[test]
fn refuses_global_binding() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    repo.arc(&anchor)
        .args(["journal", "bind", "--global"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--global is not supported"));
    assert!(!proposed(&repo, &anchor).exists());
}

/// A configured journal retains its authority and path; rejects a bind that
/// creates a default-root journal hidden by the configured resolution.
#[test]
fn refuses_configured_journal_scope() {
    let repo = Repo::new();
    let anchor = plain(&repo);
    let directory = repo.home.join("configured");
    fs::write(
        repo.home.join(".local/ai/arc/config.toml"),
        format!(
            "[journals.dirs]\n{:?} = {:?}\n",
            anchor.to_str().unwrap(),
            directory.to_str().unwrap()
        ),
    )
    .unwrap();
    repo.arc(&anchor)
        .args(["journal", "bind"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already has a journal at"))
        .stderr(predicates::str::contains(directory.display().to_string()));
    assert!(!proposed(&repo, &anchor).exists());
    assert!(!directory.exists());
}

/// Colliding path slugs never transfer a journal between projects; rejects
/// binding another directory onto an existing journal's recorded owner.
#[test]
fn refuses_slug_collision() {
    let repo = Repo::new();
    let first = repo.home.join("project.name");
    let second = repo.home.join("project-name");
    for anchor in [&first, &second] {
        fs::create_dir_all(anchor).unwrap();
    }
    let directory = PathBuf::from(stdout(repo.arc(&first).args(["journal", "bind"])).trim());
    let before = fs::read(directory.join("bindings.jsonl")).unwrap();
    repo.arc(&second)
        .args(["journal", "bind"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already bound to"))
        .stderr(predicates::str::contains(first.display().to_string()));
    assert_eq!(fs::read(directory.join("bindings.jsonl")).unwrap(), before);
}
