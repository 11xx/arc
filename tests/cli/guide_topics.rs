use super::common::*;

fn operator_artifact(repo: &Repo, topic: &str, body: &str) -> (PathBuf, String) {
    let source = repo.home.join(format!("{topic}.md"));
    fs::write(&source, body).unwrap();
    let printed = stdout(repo.arc(&repo.home).args([
        "journal",
        "note",
        "--global",
        topic,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    let path = PathBuf::from(printed.trim());
    (
        path.parent().unwrap().to_path_buf(),
        path.file_name().unwrap().to_string_lossy().into(),
    )
}

fn mark(repo: &Repo, file: &str, name: &str, global: bool) {
    let mut command = repo.arc(&repo.root);
    command.args(["journal", "pin", file, "--guide", name]);
    if global {
        command.arg("--global");
    }
    stdout(&mut command);
}

fn body(output: &str) -> &str {
    output.split_once("\n\n").expect("guide header separator").1
}

/// Claim: guide marking is a persistent pin, supports idempotence and renaming,
/// and unpin removes the route. Rejects a missing guide event field, lost guide
/// names on plain repins, duplicate events, or routes surviving unpinning.
#[test]
fn guide_pins_persist_rename_and_unmark() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "method", "todo", "# Method\n\nChoose a bound.\n");
    mark(&repo, &file, "team-method", false);
    let events = journal_event_log(&dir);
    let pin = events
        .iter()
        .find(|event| event["event"] == "pinned")
        .unwrap();
    assert_eq!(
        pin["guide"], "team-method",
        "guide marking must be persisted"
    );
    assert_eq!(pin["schema"], "journal-events/2");
    mark(&repo, &file, "team-method", false);
    assert_eq!(
        journal_event_log(&dir).len(),
        events.len(),
        "same guide pin must write nothing"
    );
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, "--reason", "standing method"]),
    );
    let shown = stdout(repo.arc(&repo.root).args(["guide", "team-method"]));
    assert_eq!(body(&shown), "# Method\n\nChoose a bound.\n");
    let open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(!open.contains(&file), "a guide pin is a standing reference");
    mark(&repo, &file, "team-practice", false);
    repo.arc(&repo.root)
        .args(["guide", "team-method"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown guide topic"));
    assert!(stdout(repo.arc(&repo.root).args(["guide", "team-practice"])).contains(&file));
    stdout(repo.arc(&repo.root).args(["journal", "unpin", &file]));
    repo.arc(&repo.root)
        .args(["guide", "team-practice"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown guide topic"));
    assert!(!stdout(repo.arc(&repo.root).arg("guide")).contains("[project]"));
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();
}

/// Claim: the index labels journal ownership, uses corrected titles and the
/// body's edit age, and reading preserves exactly the body journal show prints.
/// Rejects stale prose, omitted appended corrections or evidence, an unlabelled
/// contract, and pin metadata masquerading as a body edit.
#[test]
fn index_and_topic_use_current_journal_text_and_edit_age() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "handbook", "note", "# First title\n\nA practice.\n");
    mark(&repo, &file, "team-handbook", false);
    let source = repo.home.join("append.md");
    fs::write(&source, "The second paragraph.\n").unwrap();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "evidence",
        &file,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    stdout(repo.arc(&repo.root).args([
        "journal",
        "correct",
        &file,
        "--target",
        "artifact",
        "--field",
        "title",
        "--value",
        "Corrected title",
    ]));
    let expected = stdout(repo.arc(&repo.root).args(["journal", "show", &file]));
    assert!(expected.contains("The second paragraph."));
    assert!(expected.contains("Corrected title"));
    fs::File::open(dir.join(&file))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3 * 3600))
        .unwrap();
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, "--reason", "keep in view"]),
    );
    let events = fs::read(dir.join("events.jsonl")).unwrap();
    let index = stdout(repo.arc(&repo.root).arg("guide"));
    let row = index
        .lines()
        .find(|line| line.contains("team-handbook [project]"))
        .expect(&index);
    assert!(
        row.contains("Corrected title"),
        "index must use the corrected title: {row}"
    );
    assert!(
        row.contains("last edit: 3h ago"),
        "index must age the body's last edit: {row}"
    );
    assert!(index.find("Run-time topics").unwrap() > index.find("Command contracts:").unwrap());
    let shown = stdout(repo.arc(&repo.root).args(["guide", "team-handbook"]));
    assert!(shown.starts_with("Project's text (not arc's contract): arc guide team-handbook\n"));
    assert!(shown.contains(&format!("Artifact: {file}\n")));
    assert!(shown.contains("Last edit: 3h ago"));
    assert_eq!(
        body(&shown),
        expected,
        "guide must print journal show's current body"
    );
    assert_eq!(
        fs::read(dir.join("events.jsonl")).unwrap(),
        events,
        "guide reads must not write"
    );
}

/// Claim: built-in topics, topic families, command paths and aliases cannot be
/// shadowed and names are kebab-case. Rejects publishing a user route that looks
/// like a binary contract or accepting malformed topic names.
#[test]
fn guide_names_cannot_shadow_compiled_routes() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "shadow", "note", "# Shadow\n");
    for name in [
        "overview",
        "identity",
        "guide-topics",
        "identity-options",
        "gate-rounds",
        "journal",
        "journal-pin",
        "journal-append",
        "workspace-gates",
        "help",
    ] {
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, "--guide", name])
            .assert()
            .failure()
            .stderr(predicates::str::contains(
                "shadows a built-in topic or command route",
            ));
    }
    for name in [
        "Upper",
        "two words",
        "bad--name",
        "trailing-",
        "-leading",
        "",
        "bad/name",
        "é",
    ] {
        repo.arc(&repo.root)
            .args(["journal", "pin", &file, &format!("--guide={name}")])
            .assert()
            .failure()
            .stderr(predicates::str::contains("guide topic must be kebab-case"));
    }
    assert!(
        !journal_event_log(&dir)
            .iter()
            .any(|event| event["event"] == "pinned"),
        "refused marks must write no pins"
    );
}

/// Claim: one topic name has at most one pinned artifact in a scope. Rejects an
/// ambiguous lookup or a second artifact silently stealing a standing route.
#[test]
fn duplicate_names_in_one_scope_refuse() {
    let repo = Repo::new();
    let (_, first) = journal_artifact(&repo, "first-method", "note", "# First\n");
    let (_, second) = journal_artifact(&repo, "second-method", "note", "# Second\n");
    mark(&repo, &first, "shared-method", false);
    repo.arc(&repo.root)
        .args(["journal", "pin", &second, "--guide", "shared-method"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(format!(
            "already pinned to {first}"
        )));
    assert_eq!(
        body(&stdout(
            repo.arc(&repo.root).args(["guide", "shared-method"])
        )),
        "# First\n"
    );
}

/// Claim: project and operator names coexist with both labelled in the index and
/// search; project text wins locally and operator text reads outside a project.
/// Rejects hiding a scope, ambiguous precedence, or requiring Git to read rules.
#[test]
fn scoped_names_coexist_and_project_text_takes_precedence() {
    let repo = Repo::new();
    let (_, global) = operator_artifact(
        &repo,
        "operator-rules",
        "# Operator practice\n\nScopephrase.\n",
    );
    let (_, project) = journal_artifact(
        &repo,
        "project-rules",
        "note",
        "# Project practice\n\nScopephrase.\n",
    );
    mark(&repo, &global, "local-method", true);
    mark(&repo, &project, "local-method", false);
    let index = stdout(repo.arc(&repo.root).arg("guide"));
    assert!(
        index.contains("local-method [operator]  Operator practice"),
        "{index}"
    );
    assert!(
        index.contains("local-method [project]  Project practice"),
        "{index}"
    );
    let shown = stdout(repo.arc(&repo.root).args(["guide", "local-method"]));
    assert_eq!(
        body(&shown),
        "# Project practice\n\nScopephrase.\n",
        "project text must win locally"
    );
    let outside = stdout(repo.arc(&repo.home).args(["guide", "local-method"]));
    assert!(outside.starts_with("Operator's text (not arc's contract)"));
    assert_eq!(body(&outside), "# Operator practice\n\nScopephrase.\n");
    let outside_index = stdout(repo.arc(&repo.home).arg("guide"));
    assert!(outside_index.contains("local-method [operator]"));
    assert!(!outside_index.contains("[project]"));
    let search = stdout(
        repo.arc(&repo.root)
            .args(["guide", "--search", "SCOPEPHRASE"]),
    );
    assert!(
        search.contains(&format!("arc guide local-method [project; {project}]")),
        "{search}"
    );
    assert!(
        search.contains(&format!("arc guide local-method [operator; {global}]")),
        "{search}"
    );
    let all = stdout(repo.arc(&repo.root).args(["guide", "--all"]));
    assert!(all.contains("Project's text (not arc's contract): arc guide local-method"));
    assert!(all.contains("Operator's text (not arc's contract): arc guide local-method"));
}

/// Claim: literal case-insensitive search includes current titles and appended
/// journal text without interpreting regex. Rejects a compiled-only search or
/// stale journal indexing.
#[test]
fn search_reads_current_runtime_text_literally() {
    let repo = Repo::new();
    let (_, file) = operator_artifact(
        &repo,
        "search-method",
        "# Searchable title\n\nUnique A.*B.\n",
    );
    mark(&repo, &file, "search-practice", true);
    let source = repo.home.join("search-evidence.md");
    fs::write(&source, "Appended Searchneedle.\n").unwrap();
    stdout(repo.arc(&repo.home).args([
        "journal",
        "evidence",
        "--global",
        &file,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    assert!(stdout(
        repo.arc(&repo.home)
            .args(["guide", "--search", "searchneedle"])
    )
    .contains("search-practice [operator;"));
    let found = stdout(repo.arc(&repo.home).args(["guide", "--search", "a.*b"]));
    assert!(
        found.contains(&format!("arc guide search-practice [operator; {file}]")),
        "search must label the journal hit: {found}"
    );
    assert!(found.contains("Unique A.*B."));
    assert!(stdout(
        repo.arc(&repo.home)
            .args(["guide", "--search", "Searchable title"])
    )
    .contains("search-practice [operator;"));
    assert!(stdout(
        repo.arc(&repo.home)
            .args(["guide", "--search", "Unique AxxxxB"])
    )
    .starts_with("No instruction matches"));
}

/// Claim: ordinary pins and removed guide pins add no bytes to compiled guide
/// output, even with broken project config and constrained roles. Rejects empty
/// run-time headers, changed built-in output, or journal resolution blocking reads.
#[test]
fn no_runtime_topics_preserve_builtin_bytes() {
    let repo = Repo::new();
    let queries: &[&[&str]] = &[
        &["guide"],
        &["guide", "--all"],
        &["guide", "overview"],
        &["guide", "gate-rounds"],
        &["guide", "--search", "Git"],
        &["guide", "--search", "no-topic-phrase-xyz"],
    ];
    let baseline: Vec<_> = queries
        .iter()
        .map(|query| stdout(repo.arc(&repo.root).args(*query)))
        .collect();
    for (query, expected) in queries.iter().zip(&baseline) {
        assert_eq!(
            &stdout(repo.arc(&repo.home).args(*query)),
            expected,
            "built-in bytes must also match outside a repository"
        );
    }
    assert!(
        !baseline[0].contains("\nRun-time topics (operator and project text"),
        "built-in bytes include an empty run-time header"
    );
    let (_, file) = journal_artifact(&repo, "plain-ref", "note", "# Plain reference\n");
    stdout(repo.arc(&repo.root).args(["journal", "pin", &file]));
    for (query, expected) in queries.iter().zip(&baseline) {
        assert_eq!(
            &stdout(repo.arc(&repo.root).args(*query)),
            expected,
            "plain pins must preserve built-in bytes"
        );
    }
    mark(&repo, &file, "removed-practice", false);
    stdout(repo.arc(&repo.root).args(["journal", "unpin", &file]));
    fs::write(repo.home.join(".local/ai/arc/config.toml"), "not [toml").unwrap();
    for (query, expected) in queries.iter().zip(&baseline) {
        assert_eq!(
            &stdout(
                repo.arc(&repo.root)
                    .args(*query)
                    .args(["--role", "reviewer"])
            ),
            expected,
            "absent topics must preserve built-in bytes with broken config"
        );
    }
}

/// Claim: over-ceiling run-time text remains listed and prints in full with its
/// UTF-8 byte size. Rejects applying compiled-topic refusal or truncation to the
/// operator's text, or counting characters as bytes.
#[test]
fn oversized_runtime_text_is_readable_with_a_size_notice() {
    let repo = Repo::new();
    let text = format!("# Large practice\n\n{}\n", "é".repeat(9000));
    let (_, file) = operator_artifact(&repo, "large-text", &text);
    mark(&repo, &file, "large-practice", true);
    let notice = format!(
        "size notice: {} UTF-8 bytes exceeds the 16384-byte built-in topic ceiling",
        text.len()
    );
    assert!(
        stdout(repo.arc(&repo.home).arg("guide")).contains(&notice),
        "index must report the UTF-8 byte size"
    );
    let shown = stdout(repo.arc(&repo.home).args(["guide", "large-practice"]));
    assert!(
        shown.contains(&notice),
        "topic must report the UTF-8 byte size"
    );
    assert_eq!(body(&shown), text, "operator text must remain complete");
}

/// Claim: a bound directory without Git has project guide topics. Rejects using
/// repository discovery as a prerequisite for the journal's guide projection.
#[test]
fn project_topics_resolve_without_git() {
    let repo = Repo::new();
    let rootless = repo.home.join("rootless-project");
    fs::create_dir(&rootless).unwrap();
    stdout(repo.arc(&rootless).args(["journal", "bind"]));
    let source = repo.home.join("rootless.md");
    fs::write(&source, "# Rootless practice\n").unwrap();
    let printed = stdout(repo.arc(&rootless).args([
        "journal",
        "note",
        "practice",
        "--body-file",
        source.to_str().unwrap(),
    ]));
    let file = Path::new(printed.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    stdout(
        repo.arc(&rootless)
            .args(["journal", "pin", file, "--guide", "rootless-practice"]),
    );
    let shown = stdout(repo.arc(&rootless).args(["guide", "rootless-practice"]));
    assert!(shown.starts_with("Project's text (not arc's contract)"));
    assert_eq!(body(&shown), "# Rootless practice\n");
}
