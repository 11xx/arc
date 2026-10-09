//! A fresh write never records a placeholder or a session id where a model
//! coordinate or an identity belongs. Each write site refuses before anything
//! is written, under `--dry-run` too, and names the remedy; historical
//! records stay readable as they are.

use crate::common::*;

const UUID: &str = "019f0000-0000-7000-8000-00000000000a";

fn open(repo: &Repo, slug: &str) -> (String, PathBuf) {
    let out = stdout(repo.arc(&repo.root).args(["begin", slug]));
    (
        opened_change_id(&out),
        repo.home.join(".worktrees").join(format!("repo-{slug}")),
    )
}

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

fn journal_events(repo: &Repo) -> Vec<u8> {
    fs::read(journal_dir(repo).join("events.jsonl")).unwrap_or_default()
}

fn journal_files(repo: &Repo) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(journal_dir(repo))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn refused(cmd: &mut AssertCommand) -> String {
    let output = cmd.output().unwrap();
    assert!(
        !output.status.success(),
        "{:?} succeeded\nstdout:\n{}",
        cmd.get_args().collect::<Vec<_>>(),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn placeholder_declared_identity_refuses_every_write_before_it_records() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "declared");
    let before = event_count(&repo, &change);
    let journal_before = journal_events(&repo);

    // (environment, flag, value, words the refusal must carry)
    let cases: &[(&str, &str, &str, &[&str])] = &[
        (
            "ARC_MODEL",
            "",
            "unknown",
            &["unset ARC_MODEL", "observed model"],
        ),
        ("ARC_MODEL", "", "None#high", &["unset ARC_MODEL"]),
        ("ARC_MODEL", "", "gpt-6#unknown", &["unset ARC_MODEL"]),
        ("ARC_MODEL", "", UUID, &["session id", "unset ARC_MODEL"]),
        ("ARC_MODEL", "", "session-a", &["session id"]),
        (
            "",
            "--model",
            "<synthetic>",
            &["drop --model", "observed model"],
        ),
        ("ARC_HARNESS", "", "unknown", &["ARC_HARNESS"]),
        ("", "--harness", "N/A", &["--harness"]),
        ("ARC_SESSION", "", "none", &["ARC_SESSION"]),
        ("", "--session", "?", &["--session"]),
        ("ARC_ACTOR", "", "unknown", &["ARC_ACTOR"]),
        ("", "--actor", "pi:unknown", &["--actor"]),
        ("ARC_ACTOR", "", "claude:<synthetic>", &["ARC_ACTOR"]),
        ("ARC_ON_BEHALF_OF", "", "tbd", &["--on-behalf-of"]),
        ("", "--on-behalf-of", "pi:unknown", &["--on-behalf-of"]),
    ];
    for (variable, flag, value, words) in cases {
        let mut command = repo.arc(&repo.root);
        if !variable.is_empty() {
            command.env(variable, value);
        } else {
            command.args([flag, value]);
        }
        command.args(["roles", &change, "--planned-by", "lead:planner"]);
        let stderr = refused(&mut command);
        assert!(stderr.contains(value), "{variable}{flag}={value}: {stderr}");
        for word in *words {
            assert!(
                stderr.contains(word),
                "{variable}{flag}={value} lacks {word:?}: {stderr}"
            );
        }

        // A journal write refuses the same declaration.
        let mut command = repo.arc(&repo.root);
        if !variable.is_empty() {
            command.env(variable, value);
        } else {
            command.args([flag, value]);
        }
        refused(command.args(["journal", "log", "declared", "never recorded"]));
    }
    assert_eq!(event_count(&repo, &change), before);
    assert_eq!(journal_events(&repo), journal_before);

    // `begin` refuses before it creates a branch.
    refused(
        repo.arc(&repo.root)
            .env("ARC_MODEL", "unknown")
            .args(["begin", "never-opened"]),
    );
    assert!(git_out(&repo.root, &["branch", "--list", "arc/never-opened"]).is_empty());
}

#[test]
fn placeholder_declared_identity_refuses_under_dry_run() {
    let repo = Repo::new();
    let (change, worktree) = open(&repo, "dry");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    let head = repo.head(&worktree);
    let before = event_count(&repo, &change);

    let stderr = refused(repo.arc(&worktree).env("ARC_MODEL", "unknown").args([
        "rewrite",
        "trailers",
        "--from",
        "HEAD",
        "--append",
        "Planned-by: lead:planner",
        "--no-sign",
        "--dry-run",
    ]));
    assert!(stderr.contains("unset ARC_MODEL"), "{stderr}");
    refused(repo.arc(&worktree).env("ARC_SESSION", "unknown").args([
        "integrate",
        "dry",
        "--dry-run",
    ]));
    assert_eq!(repo.head(&worktree), head);
    assert_eq!(event_count(&repo, &change), before);
}

#[test]
fn fresh_declarations_and_session_form_actors_are_recorded() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "accepted");
    let before = event_count(&repo, &change);
    for (variable, value) in [
        ("ARC_MODEL", "gpt-6.1-sol#xhigh"),
        ("ARC_MODEL", "unknown-harness#low"),
        ("ARC_ACTOR", &format!("codex:{UUID}")),
        ("ARC_ON_BEHALF_OF", &format!("codex:{UUID}")),
        ("ARC_ACTOR", "Ada Lovelace <ada@example.org>"),
    ] {
        repo.arc(&repo.root)
            .env(variable, value)
            .args(["roles", &change, "--planned-by", "lead:planner"])
            .assert()
            .success();
    }
    assert_eq!(event_count(&repo, &change), before + 5);
    let events = stdout(repo.arc(&repo.root).args(["events", "--change", &change]));
    assert!(events.contains("gpt-6.1-sol#xhigh"), "{events}");
}

#[test]
fn reads_succeed_under_a_placeholder_declaration() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "readable");
    for args in [
        vec!["show", change.as_str()],
        vec!["status", change.as_str()],
        vec!["events", "--change", change.as_str()],
        vec!["trailers", change.as_str()],
        vec!["journal", "list"],
        vec!["instructions", "git"],
    ] {
        repo.arc(&repo.root)
            .env("ARC_MODEL", "unknown")
            .env("ARC_SESSION", "unknown")
            .env("ARC_ACTOR", "pi:unknown")
            .args(&args)
            .assert()
            .success();
    }
}

#[test]
fn role_assertions_refuse_values_that_are_not_fresh_coordinates() {
    let repo = Repo::new();
    let (change, worktree) = open(&repo, "roles");
    let before = event_count(&repo, &change);
    let session_form = format!("codex:{UUID}");
    for (flag, value) in [
        ("--planned-by", "codex:unknown"),
        ("--planned-by", session_form.as_str()),
        ("--orchestrated-by", "claude:<synthetic>"),
        ("--orchestrated-by", "codex:gpt-6#none"),
        ("--planned-by", "Ada <ada>"),
        ("--planned-by", "test:session-a"),
    ] {
        let stderr = refused(repo.arc(&repo.root).args(["roles", &change, flag, value]));
        assert!(stderr.contains(value), "{value}: {stderr}");
        assert!(stderr.contains("omit"), "{value}: {stderr}");
    }
    let brief = repo.home.join("brief.md");
    fs::write(&brief, "Implement the bounded task.\n").unwrap();
    refused(repo.arc(&worktree).args([
        "brief",
        "roles",
        "--planned-by",
        "codex:unknown",
        "--body-file",
        brief.to_str().unwrap(),
    ]));
    assert_eq!(event_count(&repo, &change), before);

    refused(
        repo.arc(&repo.root)
            .args(["begin", "unopened", "--planned-by", "pi:unknown"]),
    );
    assert!(git_out(&repo.root, &["branch", "--list", "arc/unopened"]).is_empty());

    // Accepted controls.
    for value in [
        "codex:gpt-6.1-sol#xhigh",
        "claude:claude-haiku-5-5#high",
        "Ada Lovelace <ada@example.org>",
    ] {
        repo.arc(&repo.root)
            .args(["roles", &change, "--planned-by", value])
            .assert()
            .success();
    }
    assert_eq!(event_count(&repo, &change), before + 3);
}

#[test]
fn a_historical_placeholder_assertion_can_still_be_withdrawn() {
    let repo = Repo::new();
    let (change, _) = open(&repo, "withdraw");
    // Seed an assertion recorded before fresh coordinates were required by
    // rewriting the value of an accepted one in the ledger.
    stdout(
        repo.arc(&repo.root)
            .args(["roles", &change, "--planned-by", "pi:seed-model"]),
    );
    let dir = event_dir(&repo, &change);
    let mut seeded = false;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        if text.contains("pi:seed-model") {
            fs::write(&path, text.replace("pi:seed-model", "pi:unknown")).unwrap();
            seeded = true;
        }
    }
    assert!(seeded);
    let trailers = stdout(repo.arc(&repo.root).args(["trailers", &change]));
    assert!(trailers.contains("Planned-by: pi:unknown"), "{trailers}");

    repo.arc(&repo.root)
        .args(["roles", &change, "--withdraw", "Planned-by=pi:unknown"])
        .assert()
        .success();
    let trailers = stdout(repo.arc(&repo.root).args(["trailers", &change]));
    assert!(!trailers.contains("pi:unknown"), "{trailers}");
}

#[test]
fn an_omitted_role_value_refuses_a_placeholder_observed_model() {
    let repo = Repo::new();
    codex_recording(&repo, UUID, "unknown");
    let stderr = refused(
        repo.arc(&repo.root)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", UUID)
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .args(["begin", "observed", "--orchestrated-by"]),
    );
    assert!(stderr.contains("codex:unknown"), "{stderr}");
    assert!(git_out(&repo.root, &["branch", "--list", "arc/observed"]).is_empty());
}

#[test]
fn contributors_refuse_placeholder_actors_and_keep_session_forms() {
    let repo = Repo::new();
    let (change, worktree) = open(&repo, "contrib");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    let before = event_count(&repo, &change);
    for value in ["unknown", "pi:unknown", "claude:<synthetic>", "lead,N/A"] {
        let stderr =
            refused(
                repo.arc(&worktree)
                    .args(["snapshot", "contrib", "--contributors", value]),
            );
        assert!(stderr.contains("contributor"), "{value}: {stderr}");
    }
    assert_eq!(event_count(&repo, &change), before);

    // The checkout's Git identity is the fallback actor `--solo` records.
    git(&worktree, &["config", "user.name", "unknown"]);
    let stderr = refused(
        repo.arc(&worktree)
            .env_remove("ARC_ACTOR")
            .env_remove("ARC_HARNESS")
            .args(["snapshot", "contrib", "--solo"]),
    );
    assert!(stderr.contains("--solo"), "{stderr}");
    git(&worktree, &["config", "user.name", "Tester"]);
    assert_eq!(event_count(&repo, &change), before);

    repo.arc(&worktree)
        .args([
            "snapshot",
            "contrib",
            "--contributors",
            &format!("codex:{UUID}"),
        ])
        .assert()
        .success();
    assert!(event_count(&repo, &change) > before);
}

#[test]
fn planner_json_refuses_placeholders_and_session_id_models() {
    let repo = Repo::new();
    let body = repo.home.join("plan.md");
    fs::write(&body, "# Plan\n\nBody\n").unwrap();
    let journal_before = journal_events(&repo);
    let files_before = journal_files(&repo);
    let session_model = format!(r#"{{"harness":"codex","model":"{UUID}#high"}}"#);
    let refusals = [
        r#"{"model":"unknown"}"#,
        r#"{"harness":"codex","model":"gpt-6#none"}"#,
        session_model.as_str(),
        r#"{"model":"sess-1","session":"sess-1"}"#,
        r#"{"harness":"unknown","model":"gpt-6"}"#,
        r#"{"session":"N/A"}"#,
        r#"{"actor":"pi:unknown"}"#,
        r#"{"actor":"tbd"}"#,
    ];
    for planner in refusals {
        let stderr = refused(repo.arc(&repo.root).args([
            "journal",
            "plan",
            "credit",
            "--planned-by",
            planner,
            "--body-file",
            body.to_str().unwrap(),
        ]));
        assert!(stderr.contains("planner"), "{planner}: {stderr}");
    }

    // A planner header in the body is a planner this write records.
    let headed = repo.home.join("headed.md");
    fs::write(
        &headed,
        "# Plan\n\nplanned-by: {\"harness\":\"pi\",\"model\":\"unknown\"}\n\nBody\n",
    )
    .unwrap();
    refused(repo.arc(&repo.root).args([
        "journal",
        "plan",
        "headed",
        "--body-file",
        headed.to_str().unwrap(),
    ]));
    assert_eq!(journal_events(&repo), journal_before);
    assert_eq!(journal_files(&repo), files_before);

    let path = stdout(repo.arc(&repo.root).args([
        "journal",
        "plan",
        "credit",
        "--planned-by",
        r#"{"harness":"codex","model":"gpt-6.1-sol#xhigh"}"#,
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let file = Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // The correction path refuses the same values.
    let before = journal_events(&repo);
    for planner in refusals {
        refused(repo.arc(&repo.root).args([
            "journal",
            "correct",
            &file,
            "--target",
            "artifact",
            "--field",
            "planners",
            "--value",
            &format!("[{planner}]"),
            "--note",
            "fix",
        ]));
    }
    assert_eq!(journal_events(&repo), before);
    repo.arc(&repo.root)
        .args([
            "journal",
            "correct",
            &file,
            "--target",
            "artifact",
            "--field",
            "planners",
            "--value",
            r#"[{"harness":"claude","model":"claude-haiku-5-5#high"}]"#,
            "--note",
            "fix",
        ])
        .assert()
        .success();
}

#[test]
fn a_plan_successor_refuses_placeholder_planners_under_dry_run_too() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "successor", "todo", "# Successor\n");
    let before = journal_events(&repo);
    let files_before = journal_files(&repo);
    for dry_run in [true, false] {
        let mut command = repo.arc(&repo.root);
        command.args([
            "journal",
            "transition",
            &file,
            "--to",
            "plan",
            "--planned-by",
            r#"{"harness":"codex","model":"unknown"}"#,
        ]);
        if dry_run {
            command.arg("--dry-run");
        }
        let stderr = refused(&mut command);
        assert!(stderr.contains("planner"), "{stderr}");
    }
    assert_eq!(journal_events(&repo), before);
    assert_eq!(journal_files(&repo), files_before);

    repo.arc(&repo.root)
        .args([
            "journal",
            "transition",
            &file,
            "--to",
            "plan",
            "--planned-by",
            r#"{"harness":"codex","model":"gpt-6.1-sol#xhigh"}"#,
        ])
        .assert()
        .success();
}

#[test]
fn appended_role_trailers_refuse_placeholders_before_any_commit_moves() {
    let repo = Repo::new();
    let (change, worktree) = open(&repo, "append");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    let head = repo.head(&worktree);
    let before = event_count(&repo, &change);
    let session_line = format!("Implemented-by: codex:{UUID}");
    for line in [
        "Planned-by: codex:unknown",
        session_line.as_str(),
        "Orchestrated-by: claude:<synthetic>",
        "reviewed-by: Ada <ada>",
    ] {
        for dry_run in [true, false] {
            let mut command = repo.arc(&worktree);
            command.args([
                "rewrite",
                "trailers",
                "--from",
                "HEAD",
                "--append",
                line,
                "--no-sign",
            ]);
            if dry_run {
                command.arg("--dry-run");
            }
            let stderr = refused(&mut command);
            assert!(
                stderr.contains(line.split_once(": ").unwrap().1),
                "{stderr}"
            );
            assert!(stderr.contains("omit"), "{stderr}");
        }
    }
    assert_eq!(repo.head(&worktree), head);
    assert_eq!(event_count(&repo, &change), before);

    // A key outside the roles travels as written.
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--from",
            "HEAD",
            "--append",
            "Assisted-by: pi:unknown",
            "--no-sign",
            "--dry-run",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--from",
            "HEAD",
            "--append",
            "Planned-by: codex:gpt-6.1-sol#xhigh",
            "--no-sign",
        ])
        .assert()
        .success();
    let message = git_out(&worktree, &["log", "-1", "--format=%B"]);
    assert!(
        message.contains("Planned-by: codex:gpt-6.1-sol#xhigh"),
        "{message}"
    );
}

#[test]
fn reattribution_refuses_replacement_values_that_are_not_fresh() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(&repo, "repair", "todo", "# Repair\n");
    let before = journal_events(&repo);
    for (flag, value) in [
        ("--set-model", "unknown"),
        ("--set-model", "gpt-6#none"),
        ("--set-model", UUID),
        ("--set-harness", "none"),
        ("--set-session", "unknown"),
        ("--set-actor", "pi:unknown"),
        ("--set-actor", "N/A"),
    ] {
        for dry_run in [true, false] {
            let mut command = repo.arc(&repo.root);
            command.args(["journal", "reattribute", &file, flag, value]);
            if dry_run {
                command.arg("--dry-run");
            }
            let stderr = refused(&mut command);
            assert!(stderr.contains(flag), "{flag} {value}: {stderr}");
            assert!(stderr.contains(value), "{flag} {value}: {stderr}");
        }
    }
    assert_eq!(journal_events(&repo), before);

    repo.arc(&repo.root)
        .args([
            "journal",
            "reattribute",
            &file,
            "--set-model",
            "gpt-6.1-sol#xhigh",
            "--set-actor",
            &format!("codex:{UUID}"),
        ])
        .assert()
        .success();
}

#[test]
fn identity_corrections_refuse_values_that_are_not_fresh() {
    let repo = Repo::new();
    let (_, file) = journal_artifact(
        &repo,
        "argued",
        "discussion",
        "# Discussion\n\n## Positions\n",
    );
    repo.arc(&repo.root)
        .args([
            "journal",
            "position",
            &file,
            "--stance",
            "for",
            "--body-file",
            "-",
        ])
        .write_stdin("An argument.\n")
        .assert()
        .success();
    let position = String::from_utf8(journal_events(&repo))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["event"] == "position")
        .unwrap()["position_id"]
        .as_str()
        .unwrap()
        .to_string();
    let before = journal_events(&repo);
    for (field, value) in [
        ("model", "unknown"),
        ("model", UUID),
        ("actor", "pi:unknown"),
        ("actor", "none"),
    ] {
        let stderr = refused(repo.arc(&repo.root).args([
            "journal", "correct", &file, "--target", &position, "--field", field, "--value", value,
        ]));
        assert!(stderr.contains(value), "{field} {value}: {stderr}");
    }
    assert_eq!(journal_events(&repo), before);
    for (field, value) in [("model", "gpt-6.1-sol#xhigh"), ("actor", "the-real-author")] {
        repo.arc(&repo.root)
            .args([
                "journal", "correct", &file, "--target", &position, "--field", field, "--value",
                value,
            ])
            .assert()
            .success();
    }
}

#[test]
fn a_plan_successor_refuses_an_inherited_placeholder_planner() {
    let repo = Repo::new();
    let (dir, file) = journal_artifact(&repo, "inherited", "todo", "# Inherited\n\nBody\n");
    // An older body carries the header a write now refuses.
    fs::write(
        dir.join(&file),
        "# Inherited\n\nplanned-by: {\"harness\":\"pi\",\"model\":\"unknown\"}\n\nBody\n",
    )
    .unwrap();
    let before = journal_events(&repo);
    let stderr = refused(repo.arc(&repo.root).args([
        "journal",
        "transition",
        &file,
        "--to",
        "plan",
        "--dry-run",
    ]));
    assert!(stderr.contains("--no-planner"), "{stderr}");
    assert!(stderr.contains("journal correct"), "{stderr}");
    assert_eq!(journal_events(&repo), before);

    repo.arc(&repo.root)
        .args([
            "journal",
            "transition",
            &file,
            "--to",
            "plan",
            "--no-planner",
        ])
        .assert()
        .success();
}

#[test]
fn a_hook_records_no_identity_and_runs_under_a_placeholder_declaration() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "hooked", "--no-worktree"]),
    );
    git(&repo.root, &["checkout", "-q", "arc/hooked"]);
    let message = repo.home.join("COMMIT_EDITMSG");
    fs::write(&message, "feat: work\n").unwrap();
    repo.arc(&repo.root)
        .env("ARC_MODEL", "unknown")
        .args(["hook-run", "prepare-commit-msg", message.to_str().unwrap()])
        .assert()
        .success();
    let written = fs::read_to_string(&message).unwrap();
    assert!(written.contains("Arc-Change: "), "{written}");
}
