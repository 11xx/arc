use super::common::*;

fn journal_dir(repo: &Repo) -> PathBuf {
    PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim())
}

/// The last line a command printed: where a workbench command puts the path.
fn last_line(text: &str) -> PathBuf {
    PathBuf::from(text.lines().last().unwrap_or_default())
}

fn open_workbench(repo: &Repo, slug: &str, purpose: &str) -> PathBuf {
    last_line(&stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        slug,
        "--purpose",
        purpose,
    ])))
}

fn workbench_events(dir: &Path) -> Vec<serde_json::Value> {
    journal_event_log(dir)
        .into_iter()
        .filter(|event| {
            event["event"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("workbench-"))
        })
        .collect()
}

fn listed(repo: &Repo, all: bool) -> Vec<serde_json::Value> {
    let mut args = vec!["journal", "workbench", "list", "--json"];
    if all {
        args.push("--all");
    }
    let listing = json_stdout(repo.arc(&repo.root).args(&args));
    assert_eq!(listing["schema"], "arc-journal-workbenches/1");
    listing["workbenches"].as_array().unwrap().clone()
}

/// The stderr of a command that must fail.
fn refused(repo: &Repo, args: &[&str]) -> String {
    let output = repo.arc(&repo.root).args(args).assert().failure();
    String::from_utf8_lossy(&output.get_output().stderr).to_string()
}

/// Claim: `journal workbench <slug> --purpose` creates
/// `workbench/<slug>-<YYYYMMDD>/` with its index, records a
/// `workbench-opened` event naming it and its purpose, and prints the
/// absolute path as the last line. Rejects an open that prints anything else
/// last, creates no directory, or records nothing.
#[test]
fn opening_creates_the_directory_records_it_and_prints_its_path_last() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "gate-rounds", "measure gate rounds");
    let dir = journal_dir(&repo);
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    let name = format!("gate-rounds-{today}");
    assert_eq!(path, dir.join("workbench").join(&name));
    assert!(path.is_absolute());
    assert!(path.is_dir());
    let index = fs::read_to_string(path.join("WORKBENCH.md")).unwrap();
    assert!(index.contains("Purpose: measure gate rounds"), "{index}");
    assert!(index.contains("State: open"), "{index}");

    let events = workbench_events(&dir);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["event"], "workbench-opened");
    assert_eq!(events[0]["workbench"], name.as_str());
    assert_eq!(events[0]["topic"], "gate-rounds");
    assert_eq!(events[0]["purpose"], "measure gate rounds");
    assert!(events[0].get("file").is_none(), "{:?}", events[0]);

    let rows = listed(&repo, false);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], name.as_str());
    assert_eq!(rows[0]["state"], "open");
    assert_eq!(rows[0]["files"], 0);
    assert_eq!(rows[0]["path"], path.to_str().unwrap());
}

/// Claim: opening a slug already open with the same purpose records nothing
/// and prints the same path, so a session can rerun it to find its
/// workbench; another purpose refuses, naming the open one. Rejects a second
/// workbench for one open slug.
#[test]
fn reopening_an_open_slug_returns_it_and_another_purpose_refuses() {
    let repo = Repo::new();
    let first = open_workbench(&repo, "probe-run", "probe the gate");
    let again = open_workbench(&repo, "probe-run", "probe the gate");
    assert_eq!(first, again);
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "probe-run",
            "--purpose",
            "something else",
        ],
    );
    assert!(
        stderr.contains("probe-run is already open as workbench"),
        "{stderr}"
    );
    assert_eq!(workbench_events(&journal_dir(&repo)).len(), 1);
}

/// Claim: a slug must be kebab-case and a purpose one non-empty line, and
/// both are checked before anything is created. Rejects a workbench whose
/// name or purpose a listing could not print on one line.
#[test]
fn opening_refuses_a_malformed_slug_or_purpose() {
    let repo = Repo::new();
    let stderr = refused(
        &repo,
        &["journal", "workbench", "Bad_Slug", "--purpose", "x"],
    );
    assert!(stderr.contains("must be kebab-case"), "{stderr}");
    let stderr = refused(
        &repo,
        &["journal", "workbench", "fine", "--purpose", "two\nlines"],
    );
    assert!(
        stderr.contains("--purpose must be a single line"),
        "{stderr}"
    );
    let stderr = refused(&repo, &["journal", "workbench", "fine", "--purpose", "  "]);
    assert!(stderr.contains("--purpose must say something"), "{stderr}");
    assert!(!journal_dir(&repo).join("workbench").exists());
}

/// Claim: linking records a `workbench-linked` event per change or artifact,
/// a repeated link records nothing, the links are listed and written to the
/// index, and a change or artifact that does not exist refuses. Rejects a
/// link that is not recorded, recorded twice, or names nothing.
#[test]
fn links_are_recorded_listed_once_and_must_name_something() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "lane-fanout", "lane indexes and reports");
    let change = begin_change(&repo, "fanout-fix", None);
    let (dir, artifact) = journal_artifact(&repo, "fanout", "plan", "# Fan-out plan\n");

    let linked = stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "link",
        "lane-fanout",
        "--change",
        "fanout-fix",
    ]));
    assert!(linked.contains(&format!("to change {change}")), "{linked}");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "link",
        "lane-fanout",
        "--artifact",
        &artifact,
    ]));
    let again = stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "link",
        "lane-fanout",
        "--artifact",
        &artifact,
    ]));
    assert!(again.starts_with("already linked"), "{again}");

    let links: Vec<_> = workbench_events(&dir)
        .into_iter()
        .filter(|event| event["event"] == "workbench-linked")
        .collect();
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(links[0]["changes"], serde_json::json!([change]));
    assert_eq!(links[1]["artifacts"], serde_json::json!([artifact]));
    let rows = listed(&repo, false);
    assert_eq!(rows[0]["changes"], serde_json::json!([change]));
    assert_eq!(rows[0]["artifacts"], serde_json::json!([artifact]));
    let index = fs::read_to_string(path.join("WORKBENCH.md")).unwrap();
    assert!(
        index.contains(&format!("Linked changes: {change}")),
        "{index}"
    );
    assert!(
        index.contains(&format!("Linked artifacts: {artifact}")),
        "{index}"
    );

    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "link",
            "lane-fanout",
            "--change",
            "nope",
        ],
    );
    assert!(stderr.contains("no change matches"), "{stderr}");
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "link",
            "lane-fanout",
            "--artifact",
            "20260101T000000Z-absent-note.md",
        ],
    );
    assert!(
        stderr.contains("no artifact 20260101T000000Z-absent-note.md"),
        "{stderr}"
    );
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "link",
            "no-such-bench",
            "--change",
            "fanout-fix",
        ],
    );
    assert!(
        stderr.contains("no workbench named no-such-bench"),
        "{stderr}"
    );
    assert_eq!(
        workbench_events(&dir)
            .iter()
            .filter(|event| event["event"] == "workbench-linked")
            .count(),
        2
    );
}

/// Claim: closing records a `workbench-closed` event naming the changes and
/// artifacts it produced with the workbench's size; a closed workbench
/// leaves `journal open` and `catchup` and `list --all` names its outputs.
/// Rejects a close that records no outputs or leaves the workbench listed as
/// open.
#[test]
fn closing_names_its_outputs_and_leaves_the_open_views() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "research", "draft the proposal");
    fs::write(path.join("draft.md"), "draft\n").unwrap();
    let change = begin_change(&repo, "proposal", None);
    let (dir, artifact) = journal_artifact(&repo, "proposal", "feature-request", "# Proposal\n");

    let closed = stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        "research",
        "--produced",
        &artifact,
        "--produced",
        "proposal",
        "--note",
        "filed and begun",
    ]));
    assert!(closed.contains("closed: research-"), "{closed}");
    let closing = workbench_events(&dir)
        .into_iter()
        .find(|event| event["event"] == "workbench-closed")
        .unwrap();
    assert_eq!(closing["artifacts"], serde_json::json!([artifact]));
    assert_eq!(closing["changes"], serde_json::json!([change]));
    assert_eq!(closing["files"], 1);
    assert_eq!(closing["bytes"], 6);

    assert!(listed(&repo, false).is_empty());
    let all = listed(&repo, true);
    assert_eq!(all[0]["state"], "closed");
    assert_eq!(all[0]["closed"]["artifacts"], serde_json::json!([artifact]));
    assert_eq!(all[0]["closed"]["changes"], serde_json::json!([change]));
    assert_eq!(all[0]["closed"]["note"], "filed and begun");
    let text = stdout(
        repo.arc(&repo.root)
            .args(["journal", "workbench", "list", "--all"]),
    );
    assert!(
        text.contains(&format!("produced change {change}, artifact {artifact}")),
        "{text}"
    );
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert_eq!(open["workbenches"], serde_json::json!([]));
    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(!catchup.contains("research-"), "{catchup}");
    let index = fs::read_to_string(path.join("WORKBENCH.md")).unwrap();
    assert!(index.contains("State: closed"), "{index}");
    assert!(index.contains(&format!("Produced: change {change}, artifact {artifact}")));
}

/// Claim: a close must name an output or say why there is none, refuses an
/// output that names neither an artifact nor a change, and refuses a second
/// close. Rejects a closed workbench that says nothing about its outcome, and
/// a closure rewritten after the fact.
#[test]
fn closing_refuses_no_outcome_an_unknown_output_and_a_second_close() {
    let repo = Repo::new();
    open_workbench(&repo, "spike", "try an approach");
    let stderr = refused(&repo, &["journal", "workbench", "close", "spike"]);
    assert!(
        stderr.contains("a close names what the workbench produced"),
        "{stderr}"
    );
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "close",
            "spike",
            "--produced",
            "nothing-like-this",
        ],
    );
    assert!(stderr.contains("names neither an artifact"), "{stderr}");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        "spike",
        "--note",
        "the approach failed",
    ]));
    let stderr = refused(
        &repo,
        &["journal", "workbench", "close", "spike", "--note", "again"],
    );
    assert!(stderr.contains("was closed at"), "{stderr}");
    let closes = workbench_events(&journal_dir(&repo))
        .into_iter()
        .filter(|event| event["event"] == "workbench-closed")
        .count();
    assert_eq!(closes, 1);
}

/// Claim: `catchup` and `journal open` list an open workbench with its
/// purpose, its size counted from its files, and its age. Rejects a catchup
/// or open queue that leaves working material in progress out.
#[test]
fn catchup_and_open_list_an_open_workbench_with_purpose_size_and_age() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "walltime", "gate walltime notebook");
    fs::create_dir(path.join("raw")).unwrap();
    fs::write(path.join("raw/run-1.json"), vec![b'x'; 2048]).unwrap();
    fs::write(path.join("notebook.org"), "* TODO measure\n").unwrap();

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    let section = catchup
        .find("workbenches (1 open):")
        .unwrap_or_else(|| panic!("{catchup}"));
    let rows = &catchup[section..];
    assert!(rows.contains("walltime-"), "{catchup}");
    assert!(rows.contains("in 2 files, opened "), "{catchup}");
    assert!(
        rows.contains("purpose: gate walltime notebook"),
        "{catchup}"
    );

    let open = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(open.contains("workbenches (open, newest first):"), "{open}");
    assert!(open.contains("purpose: gate walltime notebook"), "{open}");
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let row = &open["workbenches"][0];
    assert_eq!(row["purpose"], "gate walltime notebook");
    assert_eq!(row["files"], 2);
    assert_eq!(row["bytes"], 2048 + 15);
    assert!(row["age_seconds"].is_u64(), "{row}");
    let catchup = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(
        catchup["journal"]["workbenches"][0]["purpose"],
        "gate walltime notebook"
    );
}

/// Move every entry under `root`, and `root` itself, `seconds` into the past.
fn age_files(root: &Path, seconds: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
    let mut pending = vec![root.to_path_buf()];
    let mut paths = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            paths.push(path);
        }
    }
    for path in paths {
        fs::File::open(&path).unwrap().set_modified(when).unwrap();
    }
}

/// Claim: an open workbench with no recorded act and no changed file for
/// more than 14 days is stale, marked in its listing and counted by
/// `catchup`, and a file written in it makes it fresh again. Rejects
/// staleness that ignores the files arc does not record, and a stale
/// workbench that nothing surfaces.
#[test]
fn an_open_workbench_untouched_past_the_threshold_is_stale() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "old-run", "a run left behind");
    fs::write(path.join("notes.txt"), "left\n").unwrap();
    let fifteen_days = 15 * 86_400;
    age_journal_events(&journal_dir(&repo), fifteen_days as i64);
    age_files(&path, fifteen_days);

    let rows = listed(&repo, false);
    assert_eq!(rows[0]["stale"], true, "{:?}", rows[0]);
    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains("untouched 15d (stale)"), "{catchup}");
    assert!(
        catchup.contains("1 open workbenches untouched for >14 days"),
        "{catchup}"
    );

    fs::write(path.join("notes.txt"), "picked up again\n").unwrap();
    let rows = listed(&repo, false);
    assert_eq!(rows[0]["stale"], false, "{:?}", rows[0]);
    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(!catchup.contains("(stale)"), "{catchup}");
}

/// Claim: `journal archive <name>` refuses an open workbench, moves a closed
/// one to the cold archive by rename and records it, and `journal unarchive
/// <name>` brings it back. Rejects archiving work in progress and a move the
/// listing does not follow.
#[test]
fn archive_moves_a_closed_workbench_and_refuses_an_open_one() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    let stderr = refused(&repo, &["journal", "archive", &name]);
    assert!(stderr.contains("is open; close it"), "{stderr}");
    assert!(path.is_dir());

    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        &name,
        "--note",
        "kept for reference",
    ]));
    let cold = last_line(&stdout(
        repo.arc(&repo.root).args(["journal", "archive", &name]),
    ));
    let dir = journal_dir(&repo);
    assert_eq!(
        cold,
        PathBuf::from(format!("{}-archive", dir.display()))
            .join("workbench")
            .join(&name)
    );
    assert!(!path.exists());
    assert_eq!(
        fs::read_to_string(cold.join("result.txt")).unwrap(),
        "done\n"
    );
    let rows = listed(&repo, true);
    assert_eq!(rows[0]["storage"], "archived");
    assert_eq!(rows[0]["files"], 1);

    let back = last_line(&stdout(repo.arc(&repo.root).args([
        "journal",
        "unarchive",
        &name,
    ])));
    assert_eq!(back, path);
    assert_eq!(listed(&repo, true)[0]["storage"], "hot");
    let kinds: Vec<_> = workbench_events(&dir)
        .iter()
        .map(|event| event["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        [
            "workbench-opened",
            "workbench-closed",
            "workbench-archived",
            "workbench-unarchived"
        ]
    );
}

/// Claim: `journal doctor` reports a directory under `workbench/` that no
/// event records and a recorded workbench whose directory is gone. Rejects a
/// journal whose workbenches and records disagree while doctor says clean.
#[test]
fn doctor_reports_workbenches_and_records_that_disagree() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "kept", "kept work");
    let dir = journal_dir(&repo);
    let handmade = dir.join("workbench/handmade");
    fs::create_dir(&handmade).unwrap();
    fs::write(handmade.join("notes.txt"), "by hand\n").unwrap();
    let report = json_stdout_any_status(repo.arc(&repo.root).args(["journal", "doctor", "--json"]));
    let codes: Vec<_> = report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| problem["code"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes, ["unrecorded-workbench"], "{report}");

    fs::remove_dir_all(&handmade).unwrap();
    repo.arc(&repo.root)
        .args(["journal", "doctor"])
        .assert()
        .success();

    fs::remove_dir_all(&path).unwrap();
    let report = json_stdout_any_status(repo.arc(&repo.root).args(["journal", "doctor", "--json"]));
    let problems = report["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{report}");
    assert_eq!(problems[0]["code"], "missing-workbench");
}

/// Claim: journal exchange carries no workbench: a workbench name is not an
/// exportable artifact, and an exported artifact a workbench links carries
/// none of the workbench's events. Rejects a bundle that ships workbench
/// records a receiving replica has no directory for.
#[test]
fn journal_exchange_carries_no_workbench() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["replica", "init", "laptop"])
        .assert()
        .success();
    let path = open_workbench(&repo, "carried", "material that stays");
    let (_, artifact) = journal_artifact(&repo, "result", "note", "# Result\n");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "link",
        "carried",
        "--artifact",
        &artifact,
    ]));
    let bundle = repo.home.join("bundle.json");
    stdout(repo.arc(&repo.root).args([
        "journal",
        "export",
        &artifact,
        "--output",
        bundle.to_str().unwrap(),
    ]));
    let text = fs::read_to_string(&bundle).unwrap();
    assert!(text.contains(&artifact));
    assert!(!text.contains("workbench"), "{text}");
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    let stderr = refused(
        &repo,
        &[
            "journal",
            "export",
            &name,
            "--output",
            bundle.to_str().unwrap(),
        ],
    );
    assert!(
        stderr.contains("is not a journal artifact name"),
        "{stderr}"
    );
}
