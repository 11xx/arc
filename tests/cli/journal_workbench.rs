use super::common::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

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

fn adopt_refused(repo: &Repo, path: &Path) -> String {
    refused(
        repo,
        &[
            "journal",
            "workbench",
            "adopt",
            path.to_str().unwrap(),
            "--purpose",
            "old material",
        ],
    )
}

/// Every file and link under `root` by relative path: a digest of a file's
/// bytes with its mode, or a link's target. What a move must keep.
fn digests(root: &Path) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().display().to_string();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                found.insert(format!("{relative}/"), format!("{:o}", metadata.mode()));
                pending.push(path);
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                found.insert(relative, format!("link {}", target.display()));
            } else {
                let digest = hex::encode(Sha256::digest(fs::read(&path).unwrap()));
                found.insert(relative, format!("{digest} {:o}", metadata.mode()));
            }
        }
    }
    found
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

/// A loose directory the way sessions leave them: nested files, binary and
/// empty files, an executable, and a link inside it.
fn loose_entry(root: &Path) {
    fs::create_dir_all(root.join("lanes/a")).unwrap();
    fs::write(root.join("lanes/a/index.json"), "{\"lane\":\"a\"}\n").unwrap();
    fs::write(root.join("SHA256SUMS"), "abc  lanes/a/index.json\n").unwrap();
    fs::write(
        root.join("blob.bin"),
        (0..=255u8).cycle().take(70_000).collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(root.join("empty"), "").unwrap();
    fs::write(root.join("run.sh"), "#!/bin/sh\necho run\n").unwrap();
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("lanes/a/index.json", root.join("current")).unwrap();
}

/// Claim: adopting a `loose/` entry renames the directory into
/// `workbench/` keeping every file's bytes and mode and every link,
/// adds only the index, keeps the date its name carries, and records a
/// `workbench-adopted` event with where it was and its files and bytes.
/// Rejects an adoption that copies and alters, drops or adds content, or
/// leaves the source behind.
#[test]
fn adopting_a_loose_directory_preserves_every_byte() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("loose/attribution-repair-20261009");
    loose_entry(&source);
    let before = digests(&source);

    let printed = stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "adopt",
        source.to_str().unwrap(),
        "--purpose",
        "attribution repair run",
    ]));
    let target = last_line(&printed);
    assert_eq!(target, dir.join("workbench/attribution-repair-20261009"));
    assert!(!source.exists(), "the source stayed behind");
    let mut after = digests(&target);
    assert!(after.remove("WORKBENCH.md").is_some(), "no index written");
    assert_eq!(before, after);

    let adopted = &workbench_events(&dir)[0];
    assert_eq!(adopted["event"], "workbench-adopted");
    assert_eq!(adopted["topic"], "attribution-repair");
    assert_eq!(adopted["adopted_from"], source.to_str().unwrap());
    assert_eq!(adopted["files"], 6);
    assert_eq!(adopted["bytes"], 13 + 24 + 70_000 + 19);
    let rows = listed(&repo, false);
    assert_eq!(rows[0]["adopted_from"], source.to_str().unwrap());
    assert_eq!(rows[0]["purpose"], "attribution repair run");
}

/// Claim: adopting a single file, such as a notebook kept outside any
/// journal, moves it unchanged into a workbench directory of its own named
/// after the file. Rejects an adoption that leaves the file in place or
/// changes its bytes.
#[test]
fn adopting_a_file_moves_it_into_its_own_workbench() {
    let repo = Repo::new();
    let notebook = repo.home.join("walltime-notes.org");
    fs::write(&notebook, "* TODO measure the gate\n** finding\n").unwrap();
    let digest = hex::encode(Sha256::digest(fs::read(&notebook).unwrap()));

    let target = last_line(&stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "adopt",
        notebook.to_str().unwrap(),
        "--purpose",
        "walltime investigation notebook",
    ])));
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    assert_eq!(
        target.file_name().unwrap().to_str().unwrap(),
        format!("walltime-notes-{today}")
    );
    assert!(!notebook.exists());
    let moved = fs::read(target.join("walltime-notes.org")).unwrap();
    assert_eq!(hex::encode(Sha256::digest(moved)), digest);
    let adopted = &workbench_events(&journal_dir(&repo))[0];
    assert_eq!(adopted["files"], 1);
}

/// Claim: adoption refuses material a Git repository tracks, a file or a
/// directory, and moves nothing. Rejects an adoption that deletes files from
/// a checkout.
#[test]
fn adopt_refuses_material_a_repository_tracks() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("notes")).unwrap();
    repo.commit(&repo.root, "notes/plan.org", "* plan\n", "notes");
    let stderr = adopt_refused(&repo, &repo.root.join("notes/plan.org"));
    assert!(
        stderr.contains("is tracked by the Git repository at"),
        "{stderr}"
    );
    let stderr = adopt_refused(&repo, &repo.root.join("notes"));
    assert!(
        stderr.contains("is tracked by the Git repository at"),
        "{stderr}"
    );
    assert!(repo.root.join("notes/plan.org").is_file());
    assert!(workbench_events(&journal_dir(&repo)).is_empty());
}

/// Claim: adoption refuses a directory that holds a Git repository, and a
/// path inside a `.git` directory. Rejects moving a repository, or a
/// ledger, into the journal.
#[test]
fn adopt_refuses_a_repository_or_its_git_directory() {
    let repo = Repo::new();
    let material = repo.home.join("material");
    fs::create_dir_all(material.join("clone")).unwrap();
    git(&material.join("clone"), &["init", "-q"]);
    let stderr = adopt_refused(&repo, &material);
    assert!(stderr.contains("holds a Git repository"), "{stderr}");
    let stderr = adopt_refused(&repo, &repo.root.join(".git"));
    assert!(stderr.contains("is inside a `.git` directory"), "{stderr}");
    assert!(material.join("clone/.git").is_dir());
}

/// Claim: adoption refuses a symbolic link given as the path, and a
/// directory holding a relative link that leads out of it, which the move
/// would point elsewhere. Rejects an adoption that moves a link instead of
/// material or silently breaks a link's meaning.
#[test]
fn adopt_refuses_links_a_move_would_break() {
    let repo = Repo::new();
    let real = repo.home.join("real");
    fs::create_dir(&real).unwrap();
    let link = repo.home.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let stderr = adopt_refused(&repo, &link);
    assert!(stderr.contains("is a symbolic link"), "{stderr}");

    let leaking = repo.home.join("leaking");
    fs::create_dir(&leaking).unwrap();
    std::os::unix::fs::symlink("../real", leaking.join("out")).unwrap();
    let stderr = adopt_refused(&repo, &leaking);
    assert!(
        stderr.contains("is a relative link leading out of"),
        "{stderr}"
    );
    assert!(leaking.join("out").exists());
}

/// Claim: adoption refuses a directory already holding `WORKBENCH.md`, the
/// index arc writes. Rejects overwriting a file of the material.
#[test]
fn adopt_refuses_a_directory_holding_the_index_name() {
    let repo = Repo::new();
    let material = repo.home.join("material");
    fs::create_dir(&material).unwrap();
    fs::write(material.join("WORKBENCH.md"), "mine\n").unwrap();
    let stderr = adopt_refused(&repo, &material);
    assert!(stderr.contains("is not an index arc wrote"), "{stderr}");
    assert_eq!(
        fs::read_to_string(material.join("WORKBENCH.md")).unwrap(),
        "mine\n"
    );
}

/// Claim: adoption refuses the journal's own records and anything holding
/// the journal: its event log, an artifact, its lock directory, the journal
/// itself and its archive. Rejects moving the only copy of journal history
/// out from under its events.
#[test]
fn adopt_refuses_the_storage_arc_keeps() {
    let repo = Repo::new();
    let (dir, artifact) = journal_artifact(&repo, "kept", "note", "# Kept\n");
    stdout(repo.arc(&repo.root).args(["journal", "archive", &artifact]));
    for (path, refusal) in [
        (dir.join("events.jsonl"), "is part of the journal arc keeps"),
        (dir.join(".locks"), "is part of the journal arc keeps"),
        (dir.clone(), "holds this journal"),
        (
            PathBuf::from(format!("{}-archive", dir.display())),
            "this journal's archive",
        ),
    ] {
        let stderr = adopt_refused(&repo, &path);
        assert!(stderr.contains(refusal), "{}: {stderr}", path.display());
    }
    let (_, live) = journal_artifact(&repo, "live", "note", "# Live\n");
    let stderr = adopt_refused(&repo, &dir.join(&live));
    assert!(
        stderr.contains("is part of the journal arc keeps"),
        "{stderr}"
    );
    assert!(dir.join(&live).is_file());
    assert!(workbench_events(&dir).is_empty());
}

/// Claim: adoption refuses a path that does not exist and anything that is
/// neither a regular file nor a directory. Rejects recording a workbench of
/// nothing.
#[test]
fn adopt_refuses_a_missing_path_and_special_files() {
    let repo = Repo::new();
    let stderr = adopt_refused(&repo, &repo.home.join("absent"));
    assert!(stderr.contains("does not exist"), "{stderr}");
    let socket = repo.home.join("socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let stderr = adopt_refused(&repo, &socket);
    assert!(
        stderr.contains("is neither a regular file nor a directory"),
        "{stderr}"
    );
}

/// Claim: adoption refuses material on another filesystem than the
/// journal, which no rename can move atomically, and leaves it in place.
/// Rejects a copy-and-delete that could leave half the material in each
/// place. The fixture journal and the build's scratch directory must sit on
/// two filesystems for this test to exercise the refusal.
#[test]
fn adopt_refuses_material_on_another_filesystem() {
    let repo = Repo::new();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&scratch).unwrap();
    if fs::metadata(&scratch).unwrap().dev() == fs::metadata(&repo.home).unwrap().dev() {
        eprintln!(
            "coverage limit: {} and the fixture share a filesystem; the refusal is not exercised",
            scratch.display()
        );
        return;
    }
    let holder = tempfile::tempdir_in(&scratch).unwrap();
    let material = holder.path().join("elsewhere");
    fs::create_dir(&material).unwrap();
    fs::write(material.join("data"), "kept\n").unwrap();
    let stderr = adopt_refused(&repo, &material);
    assert!(stderr.contains("is on another filesystem than"), "{stderr}");
    assert!(material.join("data").is_file());
}

/// Claim: adopting under a slug already open refuses. Rejects two open
/// workbenches answering to one slug.
#[test]
fn adopt_refuses_a_slug_already_open() {
    let repo = Repo::new();
    open_workbench(&repo, "notes", "running notes");
    let material = repo.home.join("notes");
    fs::create_dir(&material).unwrap();
    let stderr = adopt_refused(&repo, &material);
    assert!(
        stderr.contains("notes is already open as workbench"),
        "{stderr}"
    );
    assert!(material.is_dir());
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
/// event records and a recorded workbench whose directory is gone, and
/// adopting the unrecorded directory where it stands records it. Rejects a
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

    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "adopt",
        handmade.to_str().unwrap(),
        "--purpose",
        "notes made by hand",
    ]));
    assert!(handmade.join("notes.txt").is_file());
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

/// The problem codes `journal doctor` reports.
fn doctor_codes(repo: &Repo) -> Vec<String> {
    let report = json_stdout_any_status(repo.arc(&repo.root).args(["journal", "doctor", "--json"]));
    report["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| problem["code"].as_str().unwrap().to_string())
        .collect()
}

/// A directory of loose material holding one file.
fn material(at: &Path) -> PathBuf {
    fs::create_dir_all(at).unwrap();
    fs::write(at.join("notes.txt"), "loose notes\n").unwrap();
    at.to_path_buf()
}

/// What a workbench holds of its material: every entry but arc's index.
fn material_digests(root: &Path) -> BTreeMap<String, String> {
    let mut found = digests(root);
    found.remove("WORKBENCH.md");
    found
}

/// Claim: adoption refuses a file named `WORKBENCH.md`, and a directory
/// under `workbench/` holding one arc did not write, and leaves its bytes as
/// they were. Rejects an adoption that moves such a file to where arc writes
/// the index and then replaces or deletes it.
#[test]
fn adopt_refuses_a_file_named_like_the_index_wherever_it_stands() {
    let repo = Repo::new();
    let loose = repo.home.join("loose");
    fs::create_dir(&loose).unwrap();
    let own = loose.join("WORKBENCH.md");
    fs::write(&own, "my own notes\n").unwrap();
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "adopt",
            own.to_str().unwrap(),
            "--slug",
            "notebook",
            "--purpose",
            "notes",
        ],
    );
    assert_eq!(fs::read_to_string(&own).unwrap(), "my own notes\n");
    assert!(stderr.contains("is named like the index"), "{stderr}");

    open_workbench(&repo, "first", "makes the workbench directory");
    let dir = journal_dir(&repo);
    let stray = dir.join("workbench/stray");
    fs::create_dir(&stray).unwrap();
    fs::write(stray.join("WORKBENCH.md"), "written by hand\n").unwrap();
    let stderr = adopt_refused(&repo, &stray);
    assert_eq!(
        fs::read_to_string(stray.join("WORKBENCH.md")).unwrap(),
        "written by hand\n"
    );
    assert!(stderr.contains("is not an index arc wrote"), "{stderr}");
    assert_eq!(workbench_events(&dir).len(), 1);
}

/// Claim: when an adoption's record is prepared but cannot be published,
/// adoption exits non-zero, leaves the material where that record puts it
/// and says where; the next journal write publishes the record and the
/// journal agrees with the disk. Rejects moving the material back while its
/// record stays prepared, which later publishes a workbench with no
/// directory.
#[test]
fn an_adoption_whose_record_is_only_prepared_keeps_the_material_where_it_names() {
    let repo = Repo::new();
    open_workbench(&repo, "first", "starts the event log");
    let dir = journal_dir(&repo);
    let source = material(&repo.home.join("loose-notes"));
    let before = digests(&source);
    let log = dir.join("events.jsonl");
    fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();
    let stderr = adopt_refused(&repo, &source);
    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    let name = format!("loose-notes-{today}");
    let target = dir.join("workbench").join(&name);
    assert!(!source.exists(), "{stderr}");
    assert_eq!(material_digests(&target), before);
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(stderr.contains("Its record is prepared"), "{stderr}");
    assert!(stderr.contains(target.to_str().unwrap()), "{stderr}");

    open_workbench(&repo, "second", "publishes what was prepared");
    let rows = listed(&repo, false);
    let adopted = rows.iter().find(|row| row["name"] == name.as_str());
    assert_eq!(adopted.map(|row| &row["storage"]), Some(&"hot".into()));
    assert_eq!(doctor_codes(&repo), Vec::<String>::new());
}

/// Claim: an archive whose record is prepared but cannot be published
/// leaves the workbench in the archive, where that record puts it, and
/// exits non-zero; once published, the journal agrees with the disk.
/// Rejects moving it back into the journal while its archived record stays
/// prepared, which leaves the record and the directory disagreeing.
#[test]
fn an_archive_whose_record_is_only_prepared_keeps_the_workbench_archived() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        &name,
        "--note",
        "kept for reference",
    ]));
    let dir = journal_dir(&repo);
    let log = dir.join("events.jsonl");
    fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();
    let stderr = refused(&repo, &["journal", "archive", &name]);
    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
    let cold = PathBuf::from(format!("{}-archive", dir.display()))
        .join("workbench")
        .join(&name);
    assert!(!path.exists(), "{stderr}");
    assert!(cold.join("result.txt").is_file(), "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(stderr.contains("Its record is prepared"), "{stderr}");

    open_workbench(&repo, "second", "publishes what was prepared");
    let rows = listed(&repo, true);
    let archived = rows.iter().find(|row| row["name"] == name.as_str());
    assert_eq!(
        archived.map(|row| &row["storage"]),
        Some(&"archived".into())
    );
    assert_eq!(doctor_codes(&repo), Vec::<String>::new());
}

/// Claim: when an adoption's record is never written, the material stays
/// where it moved, every byte kept; adoption exits non-zero naming that
/// path, `journal doctor` reports it, and adopting it where it stands
/// records it. Rejects taking a move back, which can destroy what another
/// writer did meanwhile, and a failure that hides where the material went.
#[test]
fn an_adoption_left_unrecorded_stays_moved_until_adopted_in_place() {
    let repo = Repo::new();
    open_workbench(&repo, "first", "starts the exchange directory");
    let dir = journal_dir(&repo);
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    let directory = material(&repo.home.join("loose-notes"));
    let file = repo.home.join("plan.md");
    fs::write(&file, "# Plan\n").unwrap();
    for (source, name) in [(directory, "loose-notes"), (file, "plan")] {
        let before = if source.is_dir() {
            digests(&source)
        } else {
            BTreeMap::from([("plan.md".to_string(), String::new())])
        };
        let exchange = dir.join("exchange");
        fs::set_permissions(&exchange, fs::Permissions::from_mode(0o555)).unwrap();
        let stderr = adopt_refused(&repo, &source);
        fs::set_permissions(&exchange, fs::Permissions::from_mode(0o755)).unwrap();
        let target = dir.join("workbench").join(format!("{name}-{today}"));
        assert!(!source.exists(), "{stderr}");
        if source.extension().is_none() {
            assert_eq!(material_digests(&target), before, "{stderr}");
        } else {
            assert_eq!(
                fs::read_to_string(target.join("plan.md")).unwrap(),
                "# Plan\n"
            );
        }
        assert!(stderr.contains("moved, not recorded"), "{stderr}");
        assert!(stderr.contains(target.to_str().unwrap()), "{stderr}");
        assert_eq!(doctor_codes(&repo), ["unrecorded-workbench"]);

        stdout(repo.arc(&repo.root).args([
            "journal",
            "workbench",
            "adopt",
            target.to_str().unwrap(),
            "--purpose",
            "recorded where it stands",
        ]));
        assert_eq!(doctor_codes(&repo), Vec::<String>::new());
    }
    assert_eq!(workbench_events(&dir).len(), 3);
}

/// Claim: an archive whose record is never written leaves the workbench in
/// the archive and exits non-zero naming it; doctor reports it misplaced,
/// and rerunning the archive records it without moving it again. Rejects
/// moving it back, and a retry that refuses because the directory already
/// moved.
#[test]
fn an_archive_left_unrecorded_is_recorded_by_retrying_it() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        &name,
        "--note",
        "kept for reference",
    ]));
    let dir = journal_dir(&repo);
    let exchange = dir.join("exchange");
    fs::set_permissions(&exchange, fs::Permissions::from_mode(0o555)).unwrap();
    let stderr = refused(&repo, &["journal", "archive", &name]);
    fs::set_permissions(&exchange, fs::Permissions::from_mode(0o755)).unwrap();
    let cold = PathBuf::from(format!("{}-archive", dir.display()))
        .join("workbench")
        .join(&name);
    assert!(cold.join("result.txt").is_file(), "{stderr}");
    assert!(!path.exists(), "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert_eq!(doctor_codes(&repo), ["misplaced-workbench"]);

    stdout(repo.arc(&repo.root).args(["journal", "archive", &name]));
    assert!(cold.join("result.txt").is_file());
    assert_eq!(doctor_codes(&repo), Vec::<String>::new());
    assert_eq!(listed(&repo, true)[0]["storage"], "archived");
}

/// Claim: a later act rewrites a workbench's index only while it is the
/// index arc wrote; a `WORKBENCH.md` someone put in its place is left as it
/// is, with a warning. Rejects replacing a file arc did not write.
#[test]
fn a_rewrite_never_replaces_an_index_arc_did_not_write() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "notes", "notes");
    let index = fs::read_to_string(path.join("WORKBENCH.md")).unwrap();
    assert!(index.starts_with("<!-- arc writes this index"), "{index}");
    fs::write(path.join("WORKBENCH.md"), "my own index\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    let output = repo
        .arc(&repo.root)
        .args(["journal", "workbench", "close", &name, "--note", "done"])
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert_eq!(
        fs::read_to_string(path.join("WORKBENCH.md")).unwrap(),
        "my own index\n"
    );
    assert!(stderr.contains("not the index arc writes"), "{stderr}");
}

/// Claim: arc moves and creates nothing through a `workbench/` that is a
/// link, in the journal or in its archive, and leaves the material where it
/// was. Rejects an adoption, open or archive landing material outside the
/// journal while its records say it is inside.
#[test]
fn nothing_moves_through_a_workbench_directory_that_is_a_link() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let elsewhere = repo.home.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join("workbench")).unwrap();
    let source = material(&repo.home.join("loose"));
    let stderr = adopt_refused(&repo, &source);
    assert!(source.join("notes.txt").is_file(), "{stderr}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "{stderr}");
    assert!(
        stderr.contains("is not a directory of the journal"),
        "{stderr}"
    );
    let stderr = refused(
        &repo,
        &["journal", "workbench", "probe", "--purpose", "probe"],
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "{stderr}");
    assert!(
        stderr.contains("is not a directory of the journal"),
        "{stderr}"
    );

    fs::remove_file(dir.join("workbench")).unwrap();
    let path = open_workbench(&repo, "finished", "finished work");
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        &name,
        "--note",
        "kept for reference",
    ]));
    let cold = PathBuf::from(format!("{}-archive", dir.display()));
    fs::create_dir_all(&cold).unwrap();
    std::os::unix::fs::symlink(&elsewhere, cold.join("workbench")).unwrap();
    let stderr = refused(&repo, &["journal", "archive", &name]);
    assert!(path.is_dir(), "{stderr}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "{stderr}");
    assert!(
        stderr.contains("is not a directory of the journal"),
        "{stderr}"
    );
}

/// Claim: `journal doctor` reports a recorded workbench whose directory is
/// reached through a link out of the journal. Rejects a doctor that follows
/// the link and reports agreement while the material is outside.
#[test]
fn doctor_reports_a_workbench_reached_through_a_link() {
    let repo = Repo::new();
    open_workbench(&repo, "kept", "kept work");
    let dir = journal_dir(&repo);
    let moved = repo.home.join("moved");
    fs::rename(dir.join("workbench"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, dir.join("workbench")).unwrap();
    let codes = doctor_codes(&repo);
    assert!(
        codes.iter().any(|code| code == "workbench-outside-journal"),
        "{codes:?}"
    );
}

/// Claim: a path naming a link refuses as a link with or without a
/// trailing slash, and the link and what it leads to stay as they were.
/// Rejects following the link to check what it leads to, then moving the
/// link itself.
#[test]
fn adopt_refuses_a_link_named_with_a_trailing_slash() {
    let repo = Repo::new();
    let real = material(&repo.home.join("real"));
    let link = repo.home.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "adopt",
            &format!("{}/", link.display()),
            "--purpose",
            "old material",
        ],
    );
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "{stderr}"
    );
    assert!(real.join("notes.txt").is_file());
    assert!(stderr.contains("is a symbolic link"), "{stderr}");
}

/// Claim: adoption reads a file's name literally when asking Git whether it
/// is tracked. Rejects pathspec magic in a name, such as `:(literal)`,
/// making a tracked file look untracked and moving it out of its checkout.
#[test]
fn adopt_refuses_a_tracked_file_whose_name_is_pathspec_magic() {
    let repo = Repo::new();
    let name = ":(literal)notes.txt";
    fs::write(repo.root.join(name), "tracked\n").unwrap();
    git(&repo.root, &["--literal-pathspecs", "add", name]);
    git(
        &repo.root,
        &["commit", "-qm", "add a file with a magic name"],
    );
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "adopt",
            name,
            "--slug",
            "notes",
            "--purpose",
            "notes",
        ],
    );
    assert!(repo.root.join(name).is_file(), "{stderr}");
    assert!(
        stderr.contains("is tracked by the Git repository"),
        "{stderr}"
    );
}

/// Claim: adoption refuses a journal, registered or not, anything inside
/// one, and anything holding one, including a journal under this journal.
/// Rejects moving another project's journal because it sits under this one
/// or was never registered.
#[test]
fn adopt_refuses_another_journal_wherever_it_lies() {
    let repo = Repo::new();
    open_workbench(&repo, "first", "starts this journal");
    let dir = journal_dir(&repo);
    let body = repo.home.join("body.md");
    fs::write(&body, "# Other\n").unwrap();
    let write_journal = |at: &Path| {
        stdout(repo.arc(&repo.root).env("ARC_JOURNAL_DIR", at).args([
            "journal",
            "note",
            "other",
            "--kind",
            "note",
            "--body-file",
            body.to_str().unwrap(),
        ]));
    };
    let unregistered = repo.home.join("projects/other-journal");
    write_journal(&unregistered);
    let nested = dir.join("loose/other");
    write_journal(&nested);
    let registered = repo.home.join("registered/journal");
    write_journal(&registered);
    let config = repo.home.join(".local/ai/arc/config.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "[journals.dirs]\n\"{}\" = \"{}\"\n\"{}\" = \"{}\"\n",
        repo.home.join("elsewhere-a").display(),
        nested.display(),
        repo.home.join("elsewhere-b").display(),
        registered.display()
    ));
    fs::write(&config, text).unwrap();
    for (path, refusal) in [
        (unregistered.clone(), "is a journal"),
        (
            unregistered.parent().unwrap().to_path_buf(),
            "holds a journal",
        ),
        (unregistered.join("unrelated"), "lies inside the journal"),
        (nested.clone(), "lies inside the journal"),
        (dir.join("loose"), "holds the journal"),
        (registered.clone(), "lies inside the journal"),
    ] {
        fs::create_dir_all(&path).unwrap();
        let stderr = adopt_refused(&repo, &path);
        assert!(path.is_dir(), "{}: {stderr}", path.display());
        assert!(stderr.contains(refusal), "{}: {stderr}", path.display());
    }
    assert_eq!(workbench_events(&dir).len(), 1);
    assert_eq!(workbench_events(&nested).len(), 0);
    assert!(nested.join("events.jsonl").is_file());
}

/// Claim: adoption refuses a bare Git repository, a directory holding one,
/// and anything inside one. Rejects moving a repository because it has no
/// `.git` entry.
#[test]
fn adopt_refuses_a_bare_repository() {
    let repo = Repo::new();
    let holder = repo.home.join("material");
    fs::create_dir(&holder).unwrap();
    let bare = holder.join("store.git");
    git(&holder, &["init", "-q", "--bare", "store.git"]);
    for (path, refusal) in [
        (holder.clone(), "holds a Git repository"),
        (bare.clone(), "is a Git repository"),
        (bare.join("hooks"), "lies inside the Git repository"),
    ] {
        let stderr = adopt_refused(&repo, &path);
        assert!(path.is_dir(), "{}: {stderr}", path.display());
        assert!(stderr.contains(refusal), "{}: {stderr}", path.display());
    }
    assert!(bare.join("HEAD").is_file());
}

/// Claim: adoption refuses the current directory, the home directory and
/// any directory holding either, whatever else they hold. Rejects moving
/// the directory a caller stands in, or their home, because no journal or
/// repository happens to lie inside it.
#[test]
fn adopt_refuses_the_current_and_home_directories() {
    let repo = Repo::new();
    let standing = material(&repo.root.join("scratch/here"));
    let scratch = repo.root.join("scratch");
    for path in [".", "..", scratch.to_str().unwrap()] {
        let output = repo
            .arc(&standing)
            .args(["journal", "workbench", "adopt", path, "--purpose", "here"])
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
        assert!(standing.join("notes.txt").is_file(), "{path}: {stderr}");
        assert!(stderr.contains("is the current directory"), "{stderr}");
    }

    let homes = repo.home.parent().unwrap().join("homes");
    let home = material(&homes.join("someone"));
    for path in [home.clone(), homes.clone()] {
        let output = repo
            .arc(&repo.root)
            .env("HOME", &home)
            .args([
                "journal",
                "workbench",
                "adopt",
                path.to_str().unwrap(),
                "--purpose",
                "home",
            ])
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
        assert!(home.join("notes.txt").is_file(), "{stderr}");
        assert!(stderr.contains("is the home directory"), "{stderr}");
    }
}

/// Claim: adoption refuses a path whose record cannot state it on one line,
/// before anything moves. Rejects a reported success whose record the
/// journal cannot read back, which leaves an unlisted directory.
#[test]
fn adopt_refuses_a_path_its_record_cannot_state() {
    let repo = Repo::new();
    let source = material(&repo.home.join("two\nlines"));
    let stderr = refused(
        &repo,
        &[
            "journal",
            "workbench",
            "adopt",
            source.to_str().unwrap(),
            "--slug",
            "two-lines",
            "--purpose",
            "notes",
        ],
    );
    assert!(source.join("notes.txt").is_file(), "{stderr}");
    assert!(stderr.contains("cannot be recorded"), "{stderr}");
    let dir = journal_dir(&repo);
    assert!(workbench_events(&dir).is_empty());
    assert!(
        !dir.join("workbench").exists()
            || fs::read_dir(dir.join("workbench")).unwrap().count() == 0
    );
}

/// Claim: adoption refuses anything inside a `.arc` or `.git` directory,
/// which hold a checkout's ledger, spooled journal writes and repository.
/// Rejects moving pending journal writes away from where `journal spool`
/// reads them.
#[test]
fn adopt_refuses_a_checkouts_arc_directory() {
    let repo = Repo::new();
    let outbox = repo.root.join(".arc/outbox");
    fs::create_dir_all(&outbox).unwrap();
    fs::write(outbox.join("pending.json"), "{}\n").unwrap();
    let stderr = adopt_refused(&repo, &outbox);
    assert!(outbox.join("pending.json").is_file(), "{stderr}");
    assert!(stderr.contains("is inside a `.arc` directory"), "{stderr}");
}

/// Claim: adoption refuses a filesystem root and a mount point. Rejects
/// moving a whole mounted tree into the journal.
#[test]
fn adopt_refuses_a_filesystem_root_and_a_mount_point() {
    let repo = Repo::new();
    let stderr = adopt_refused(&repo, Path::new("/"));
    assert!(stderr.contains("is a filesystem root"), "{stderr}");
    let mount = Path::new("/proc");
    if fs::metadata(mount).map(|metadata| metadata.dev()).ok()
        == fs::metadata("/").map(|metadata| metadata.dev()).ok()
    {
        eprintln!("coverage limit: /proc is not a mount point here; the refusal is not exercised");
        return;
    }
    let stderr = adopt_refused(&repo, mount);
    assert!(stderr.contains("is a mount point"), "{stderr}");
}

/// An adoption of `source` paused at `phase`, which `finish` resumes.
fn adopt_paused_at(repo: &Repo, source: &Path, phase: &str) -> std::process::Child {
    spawn_arc_at_rendezvous(
        repo,
        &repo.root,
        &[
            "journal",
            "workbench",
            "adopt",
            source.to_str().unwrap(),
            "--purpose",
            "old material",
        ],
        phase,
        &[],
    )
}

/// Resume a paused command and return its success and stderr.
fn finish(mut child: std::process::Child) -> (bool, String) {
    release_lock_wait(&mut child);
    let output = child.wait_with_output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Claim: the index arc writes when a workbench is adopted never replaces
/// a `WORKBENCH.md` that appeared in the material after its checks; the
/// adoption stops as moved and not recorded, naming where the material is.
/// Rejects an index that overwrites the only copy of a file of the
/// material.
#[test]
fn the_first_index_never_replaces_a_file_made_during_adoption() {
    let repo = Repo::new();
    let source = material(&repo.home.join("loose-notes"));
    let child = adopt_paused_at(&repo, &source, "workbench-adopt-checked");
    fs::write(source.join("WORKBENCH.md"), "made meanwhile\n").unwrap();
    let (succeeded, stderr) = finish(child);
    let dir = journal_dir(&repo);
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    let target = dir.join("workbench").join(format!("loose-notes-{today}"));
    assert_eq!(
        fs::read_to_string(target.join("WORKBENCH.md")).unwrap(),
        "made meanwhile\n",
        "{stderr}"
    );
    assert!(!succeeded, "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(stderr.contains(target.to_str().unwrap()), "{stderr}");
    assert!(workbench_events(&dir).is_empty());
}

/// Claim: archiving moves nothing through an archive root that is a link,
/// and `journal doctor` reports that link. Rejects an archive that lands a
/// workbench outside the journal's storage while its record says archived.
#[test]
fn archive_never_moves_through_a_linked_archive_root() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        &name,
        "--note",
        "kept for reference",
    ]));
    let dir = journal_dir(&repo);
    let elsewhere = repo.home.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, format!("{}-archive", dir.display())).unwrap();
    let stderr = refused(&repo, &["journal", "archive", &name]);
    assert!(path.join("result.txt").is_file(), "{stderr}");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "{stderr}");
    assert!(
        stderr.contains("is not a directory of the journal"),
        "{stderr}"
    );
    let codes = doctor_codes(&repo);
    assert!(
        codes.iter().any(|code| code == "workbench-outside-journal"),
        "{codes:?}"
    );
}

/// Claim: adoption checks the material as it stands once it holds the
/// journal's transition lock, so a directory replaced by a link while it
/// waited refuses as a link. Rejects checking one object and moving
/// another, and writing the index through a link outside the journal.
#[test]
fn adopt_checks_the_source_as_it_stands_once_it_holds_the_lock() {
    let repo = Repo::new();
    open_workbench(&repo, "first", "starts this journal");
    let dir = journal_dir(&repo);
    let source = material(&repo.home.join("loose-notes"));
    let outside = material(&repo.home.join("outside"));
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(".locks/transition.lock"))
        .unwrap();
    held.lock().unwrap();
    let child = adopt_paused_at(&repo, &source, "transition-contended");
    fs::rename(&source, repo.home.join("moved-away")).unwrap();
    std::os::unix::fs::symlink(&outside, &source).unwrap();
    release_lock_wait_after(child, held, |succeeded, stderr| {
        assert!(!outside.join("WORKBENCH.md").exists(), "{stderr}");
        assert!(
            fs::symlink_metadata(&source)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{stderr}"
        );
        assert!(!succeeded, "{stderr}");
        assert!(stderr.contains("is a symbolic link"), "{stderr}");
    });
    assert_eq!(workbench_events(&dir).len(), 1);
}

/// Resume `child` paused at a lock rendezvous, release the lock it waits
/// on, and check its outcome.
fn release_lock_wait_after(
    mut child: std::process::Child,
    held: fs::File,
    check: impl FnOnce(bool, &str),
) {
    release_lock_wait(&mut child);
    drop(held);
    let output = child.wait_with_output().unwrap();
    check(
        output.status.success(),
        &String::from_utf8_lossy(&output.stderr),
    );
}

/// Claim: adoption moves only the object it checked: material replaced
/// after its checks refuses and nothing moves. Rejects moving a directory
/// no refusal ever looked at.
#[test]
fn adopt_refuses_material_replaced_after_its_checks() {
    let repo = Repo::new();
    let source = material(&repo.home.join("loose-notes"));
    let child = adopt_paused_at(&repo, &source, "workbench-adopt-checked");
    fs::rename(&source, repo.home.join("moved-away")).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("other.txt"), "not checked\n").unwrap();
    let (succeeded, stderr) = finish(child);
    assert!(source.join("other.txt").is_file(), "{stderr}");
    assert!(!succeeded, "{stderr}");
    assert!(stderr.contains("changed after it was checked"), "{stderr}");
    assert!(workbench_events(&journal_dir(&repo)).is_empty());
}

/// Claim: whether Git tracks the material is asked with repository
/// discovery the caller's environment cannot steer. Rejects a
/// `GIT_CEILING_DIRECTORIES` that hides the repository and lets a tracked
/// file move out of its checkout.
#[test]
fn adopt_refuses_tracked_material_whatever_discovery_the_caller_set() {
    let repo = Repo::new();
    let nested = repo.root.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("notes.txt"), "tracked\n").unwrap();
    git(&repo.root, &["add", "a/b/notes.txt"]);
    git(&repo.root, &["commit", "-qm", "add nested notes"]);
    let output = repo
        .arc(&repo.root)
        .env("GIT_CEILING_DIRECTORIES", repo.root.join("a"))
        .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "0")
        .args([
            "journal",
            "workbench",
            "adopt",
            "a/b/notes.txt",
            "--purpose",
            "notes",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(nested.join("notes.txt").is_file(), "{stderr}");
    assert!(
        stderr.contains("is tracked by the Git repository"),
        "{stderr}"
    );
}

/// Claim: a listing whose walk of an open workbench stopped at its entry
/// bound leaves staleness unknown: it calls nothing stale or untouched and
/// `catchup` counts nothing. Rejects declaring active material stale from
/// the part of it a bounded walk happened to see.
#[test]
fn a_partial_walk_never_calls_a_workbench_stale() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "big-run", "a run with many files");
    for index in 0..4 {
        fs::write(path.join(format!("{index}.txt")), "x\n").unwrap();
    }
    let fifteen_days = 15 * 86_400;
    age_journal_events(&journal_dir(&repo), fifteen_days as i64);
    age_files(&path, fifteen_days);
    let listing = json_stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_WORKBENCH_WALK_LIMIT", "2")
            .args(["journal", "workbench", "list", "--json"]),
    );
    let row = &listing["workbenches"][0];
    assert_eq!(row["size_partial"], true, "{row}");
    assert_eq!(row["stale"], serde_json::Value::Null, "{row}");
    let catchup = stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_WORKBENCH_WALK_LIMIT", "2")
            .args(["catchup"]),
    );
    assert!(!catchup.contains("(stale)"), "{catchup}");
    assert!(!catchup.contains("untouched"), "{catchup}");
    assert!(catchup.contains("staleness unknown"), "{catchup}");
}

/// Claim: adoption refuses unless Git answers whether the material is
/// tracked, or says exactly that no repository holds it; any other Git
/// failure refuses. Rejects a repository whose broken configuration makes
/// Git print the words "not a git repository" inside another error,
/// letting a tracked file move.
#[test]
fn adopt_refuses_when_git_cannot_say_whether_material_is_tracked() {
    let repo = Repo::new();
    let other = repo.home.join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-q"]);
    fs::write(other.join("notes.txt"), "tracked\n").unwrap();
    git(&other, &["add", "notes.txt"]);
    git(
        &other,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "add notes",
        ],
    );
    git(&other, &["config", "core.bare", "not a git repository"]);
    let stderr = adopt_refused(&repo, &other.join("notes.txt"));
    assert!(other.join("notes.txt").is_file(), "{stderr}");
    assert!(
        stderr.contains("cannot tell whether a repository tracks"),
        "{stderr}"
    );
}

/// Claim: an adoption stopped by a `WORKBENCH.md` arc did not write names
/// that file and the step that frees it, adopting in place says the same
/// while the file is there, and once it is moved aside adopting in place
/// records the workbench with that file untouched. Rejects a repair the
/// message names and the command then refuses, and arc moving or
/// overwriting the user's file itself.
#[test]
fn a_foreign_index_names_the_step_that_lets_the_repair_record() {
    let repo = Repo::new();
    let source = material(&repo.home.join("loose-notes"));
    let child = adopt_paused_at(&repo, &source, "workbench-adopt-checked");
    fs::write(source.join("WORKBENCH.md"), "made meanwhile\n").unwrap();
    let (_, stderr) = finish(child);
    let dir = journal_dir(&repo);
    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    let target = dir.join("workbench").join(format!("loose-notes-{today}"));
    let foreign = target.join("WORKBENCH.md");
    assert!(stderr.contains(foreign.to_str().unwrap()), "{stderr}");
    assert!(stderr.contains("move or rename it"), "{stderr}");

    let stderr = adopt_refused(&repo, &target);
    assert!(stderr.contains(foreign.to_str().unwrap()), "{stderr}");
    assert!(stderr.contains("move or rename it"), "{stderr}");
    assert_eq!(fs::read_to_string(&foreign).unwrap(), "made meanwhile\n");

    fs::rename(&foreign, target.join("mine.md")).unwrap();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "adopt",
        target.to_str().unwrap(),
        "--purpose",
        "recorded where it stands",
    ]));
    assert_eq!(
        fs::read_to_string(target.join("mine.md")).unwrap(),
        "made meanwhile\n"
    );
    assert_eq!(doctor_codes(&repo), Vec::<String>::new());
}

/// Claim: when `workbench/` leaves the journal while an adoption is about
/// to move into it, the adoption stops as moved and not recorded, names
/// where the material went, and records nothing. Rejects reporting success
/// for material that landed outside the journal.
#[test]
fn an_adoption_whose_destination_left_the_journal_records_nothing() {
    let repo = Repo::new();
    open_workbench(&repo, "first", "makes the workbench directory");
    let dir = journal_dir(&repo);
    let source = material(&repo.home.join("loose-notes"));
    let child = adopt_paused_at(&repo, &source, "workbench-adopt-moving");
    let saved = repo.home.join("saved");
    fs::rename(dir.join("workbench"), &saved).unwrap();
    let (succeeded, stderr) = finish(child);
    assert!(!succeeded, "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(stderr.contains(saved.to_str().unwrap()), "{stderr}");
    assert_eq!(workbench_events(&dir).len(), 1);
}

/// Claim: when the material is replaced just before an adoption moves it,
/// the adoption stops as moved and not recorded, saying what moved and
/// where. Rejects recording an object no check looked at.
#[test]
fn an_adoption_that_moved_a_replacement_records_nothing() {
    let repo = Repo::new();
    let source = material(&repo.home.join("loose-notes"));
    let child = adopt_paused_at(&repo, &source, "workbench-adopt-moving");
    fs::rename(&source, repo.home.join("moved-away")).unwrap();
    fs::create_dir(&source).unwrap();
    let (succeeded, stderr) = finish(child);
    assert!(!succeeded, "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(
        stderr.contains("not the material that was checked"),
        "{stderr}"
    );
    assert!(workbench_events(&journal_dir(&repo)).is_empty());
}

/// A closed workbench and its name, paused in an archive at `phase`.
fn archive_paused_at(repo: &Repo, phase: &str) -> (std::process::Child, PathBuf, String) {
    let path = open_workbench(repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        &name,
        "--note",
        "kept for reference",
    ]));
    let child =
        spawn_arc_at_rendezvous(repo, &repo.root, &["journal", "archive", &name], phase, &[]);
    (child, path, name)
}

fn archived_events(dir: &Path) -> usize {
    workbench_events(dir)
        .iter()
        .filter(|event| event["event"] == "workbench-archived")
        .count()
}

/// Claim: when the archive's `workbench/` leaves the archive while an
/// archive is about to move into it, the archive stops as moved and not
/// recorded, names where the workbench went, and records nothing. Rejects
/// recording a workbench archived that lies outside the journal's storage.
#[test]
fn an_archive_whose_destination_left_the_archive_records_nothing() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    let cold = PathBuf::from(format!("{}-archive", dir.display()));
    fs::create_dir_all(cold.join("workbench")).unwrap();
    let (child, _, _) = archive_paused_at(&repo, "workbench-archive-moving");
    let saved = repo.home.join("saved");
    fs::rename(cold.join("workbench"), &saved).unwrap();
    let (succeeded, stderr) = finish(child);
    assert!(!succeeded, "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(stderr.contains(saved.to_str().unwrap()), "{stderr}");
    assert_eq!(archived_events(&dir), 0);
}

/// Claim: when a workbench is replaced just before an archive moves it,
/// the archive stops as moved and not recorded and records nothing.
/// Rejects recording as archived a directory no measure looked at.
#[test]
fn an_archive_that_moved_a_replacement_records_nothing() {
    let repo = Repo::new();
    let (child, path, _) = archive_paused_at(&repo, "workbench-archive-moving");
    fs::rename(&path, repo.home.join("moved-away")).unwrap();
    fs::create_dir(&path).unwrap();
    let (succeeded, stderr) = finish(child);
    assert!(!succeeded, "{stderr}");
    assert!(stderr.contains("moved, not recorded"), "{stderr}");
    assert!(
        stderr.contains("not the material that was checked"),
        "{stderr}"
    );
    assert_eq!(archived_events(&journal_dir(&repo)), 0);
}

/// The project row a workspace view gives this fixture's repository.
fn workspace_project<'a>(view: &'a serde_json::Value, repo: &Repo) -> &'a serde_json::Value {
    let anchor = fs::canonicalize(&repo.root).unwrap();
    view["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["anchor"] == anchor.to_str().unwrap())
        .unwrap_or_else(|| panic!("no row for {}: {view}", anchor.display()))
}

/// Claim: `journal catchup`, `workspace backlog` and `workspace report`
/// each list an open workbench with its purpose, its size counted from its
/// files, and its age, in text and in JSON, and a project whose only fact
/// is an open workbench is not empty. Rejects a cross-project or journal
/// view that leaves working material in progress out.
#[test]
fn journal_catchup_and_the_workspace_views_list_an_open_workbench() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "walltime", "gate walltime notebook");
    fs::write(path.join("notebook.org"), vec![b'x'; 2048]).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();

    let catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    let section = catchup
        .find("workbenches (1 open):")
        .unwrap_or_else(|| panic!("{catchup}"));
    assert!(
        catchup[section..].contains(&format!("{name}  2K in 1 file, opened ")),
        "{catchup}"
    );
    assert!(
        catchup[section..].contains("purpose: gate walltime notebook"),
        "{catchup}"
    );
    let catchup = json_stdout(repo.arc(&repo.root).args(["journal", "catchup", "--json"]));
    let row = &catchup["workbenches"][0];
    assert_eq!(row["name"], name.as_str(), "{catchup}");
    assert_eq!(row["files"], 1, "{row}");
    assert_eq!(row["bytes"], 2048, "{row}");
    assert!(row["age_seconds"].is_u64(), "{row}");

    let backlog = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--json"]),
    );
    let project = workspace_project(&backlog, &repo);
    assert_eq!(
        project["workbenches"][0]["name"],
        name.as_str(),
        "{project}"
    );
    assert_eq!(project["workbenches"][0]["bytes"], 2048, "{project}");
    assert_eq!(backlog["summary"]["workbenches"], 1, "{backlog}");
    assert_eq!(backlog["summary"]["stale_workbenches"], 0, "{backlog}");
    let backlog = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(
        backlog.contains(&format!("  workbench     {name}  2K in 1 file, opened ")),
        "{backlog}"
    );
    assert!(backlog.contains("; gate walltime notebook"), "{backlog}");
    assert!(backlog.contains("; 1 open workbenches;"), "{backlog}");

    let report = json_stdout(repo.arc(&repo.root).args(["workspace", "report", "--json"]));
    assert_eq!(report["schema"], "arc-workspace-report/3");
    let row = &report["sections"]["workbenches"][0];
    assert_eq!(row["name"], name.as_str(), "{report}");
    assert_eq!(row["purpose"], "gate walltime notebook", "{row}");
    assert_eq!(row["bytes"], 2048, "{row}");
    assert_eq!(row["age_days"], 0, "{row}");
    assert_eq!(row["stale"], false, "{row}");
    assert_eq!(report["tallies"]["workbenches"]["value"], 1, "{report}");
    assert_eq!(workspace_project(&report, &repo)["workbenches"], 1);
    let report = stdout(repo.arc(&repo.root).args(["workspace", "report"]));
    assert!(report.contains("open workbenches 1 (0 stale)"), "{report}");
}

/// Claim: a stale open workbench is surfaced as stale by `journal catchup`,
/// counted by `workspace backlog`, and raised as `stale-workbench` by
/// `workspace report`; one whose walk stopped short shows its staleness as
/// unknown in each and raises nothing. Rejects stale material the
/// cross-project views keep quiet about, and a partial walk read as fresh
/// or as stale.
#[test]
fn the_workspace_views_surface_a_stale_workbench_and_an_unknown_one() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "old-run", "a run left behind");
    for index in 0..4 {
        fs::write(path.join(format!("{index}.txt")), "x\n").unwrap();
    }
    let fifteen_days = 15 * 86_400;
    age_journal_events(&journal_dir(&repo), fifteen_days as i64);
    age_files(&path, fifteen_days);
    let name = path.file_name().unwrap().to_str().unwrap().to_string();

    let catchup = stdout(repo.arc(&repo.root).args(["journal", "catchup"]));
    assert!(catchup.contains("untouched 15d (stale)"), "{catchup}");
    assert!(
        catchup.contains("1 open workbenches untouched for >14 days"),
        "{catchup}"
    );
    let backlog = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "backlog", "--json"]),
    );
    assert_eq!(backlog["summary"]["stale_workbenches"], 1, "{backlog}");
    let backlog = stdout(repo.arc(&repo.root).args(["workspace", "backlog"]));
    assert!(backlog.contains("untouched 15d (stale)"), "{backlog}");
    assert!(
        backlog.contains("; 1 open workbenches (1 stale);"),
        "{backlog}"
    );
    let report = json_stdout(repo.arc(&repo.root).args(["workspace", "report", "--json"]));
    assert_eq!(report["sections"]["workbenches"][0]["stale"], true);
    let raised: Vec<&serde_json::Value> = report["attention"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["rule"] == "stale-workbench")
        .collect();
    assert_eq!(raised.len(), 1, "{report}");
    assert_eq!(raised[0]["subject"], name.as_str());
    let report = stdout(repo.arc(&repo.root).args(["workspace", "report"]));
    assert!(report.contains("open workbenches 1 (1 stale)"), "{report}");
    assert!(report.contains("attention  stale-workbench  "), "{report}");

    let partial = |args: &[&str]| {
        let mut command = repo.arc(&repo.root);
        command.env("ARC_TEST_WORKBENCH_WALK_LIMIT", "2").args(args);
        command
    };
    let catchup = stdout(&mut partial(&["journal", "catchup"]));
    assert!(!catchup.contains("(stale)"), "{catchup}");
    assert!(catchup.contains("staleness unknown"), "{catchup}");
    let backlog = stdout(&mut partial(&["workspace", "backlog"]));
    assert!(!backlog.contains("(stale)"), "{backlog}");
    assert!(backlog.contains("staleness unknown"), "{backlog}");
    let backlog = json_stdout(&mut partial(&["workspace", "backlog", "--json"]));
    assert_eq!(backlog["summary"]["stale_workbenches"], 0, "{backlog}");
    let report = json_stdout(&mut partial(&["workspace", "report", "--json"]));
    let row = &report["sections"]["workbenches"][0];
    assert_eq!(row["stale"], serde_json::Value::Null, "{row}");
    assert_eq!(row["size_partial"], true, "{row}");
    assert!(
        !report["attention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["rule"] == "stale-workbench"),
        "{report}"
    );
    let report = stdout(&mut partial(&["workspace", "report"]));
    assert!(
        report.contains("open workbenches 1 (0 stale, 1 staleness unknown)"),
        "{report}"
    );
}

/// Claim: where no rename that refuses to replace exists, adopting a
/// directory or a file and archiving a workbench each refuse, naming the
/// missing rename, and leave the material where it was, every byte kept,
/// with no workbench directory made and nothing recorded. Rejects a move
/// that falls back to a rename able to replace what stands at the
/// destination, and a refused file adoption that leaves its empty
/// directory behind.
#[test]
fn without_a_no_replace_rename_nothing_moves() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let unsupported = |args: &[&str]| {
        let output = repo
            .arc(&repo.root)
            .env("ARC_TEST_WORKBENCH_NO_RENAME_NOREPLACE", "1")
            .args(args)
            .assert()
            .failure();
        String::from_utf8_lossy(&output.get_output().stderr).to_string()
    };
    let missing = "no rename that refuses to replace, so arc moves nothing here";

    let source = dir.join("loose/sweep-20261009");
    loose_entry(&source);
    let notebook = repo.home.join("notes.org");
    fs::write(&notebook, "* notes\n").unwrap();
    let before = (digests(&source), fs::read(&notebook).unwrap());
    for path in [&source, &notebook] {
        let stderr = unsupported(&[
            "journal",
            "workbench",
            "adopt",
            path.to_str().unwrap(),
            "--purpose",
            "old material",
        ]);
        assert!(stderr.contains(missing), "{stderr}");
        assert!(stderr.contains("nothing moved"), "{stderr}");
    }
    assert_eq!(before, (digests(&source), fs::read(&notebook).unwrap()));
    let holding: Vec<_> = fs::read_dir(dir.join("workbench"))
        .map(|entries| entries.map(|entry| entry.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(holding.is_empty(), "{holding:?}");
    assert!(workbench_events(&dir).is_empty());

    let path = open_workbench(&repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "workbench", "close", &name, "--note", "kept"]),
    );
    let stderr = unsupported(&["journal", "archive", &name]);
    assert!(stderr.contains(missing), "{stderr}");
    assert_eq!(
        fs::read_to_string(path.join("result.txt")).unwrap(),
        "done\n"
    );
    assert!(!PathBuf::from(format!("{}-archive", dir.display()))
        .join("workbench")
        .join(&name)
        .exists());
    assert_eq!(listed(&repo, true)[0]["storage"], "hot");
}

/// The drafts a rewrite left beside the index at `path`: every entry whose
/// name begins with the reserved draft name.
fn drafts(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".WORKBENCH.md.arc-draft"))
        .collect();
    names.sort();
    names
}

const MARKER: &str = "<!-- arc writes this index from the journal's records -->";

/// Claim: a rewrite of a workbench's index never alters the file a reader
/// already has open: that reader reads the old index whole while the path
/// holds the new one. A rewrite interrupted at any point before its draft
/// takes the index's place, the draft just created, holding part of arc's
/// marker, or written whole, leaves the old index whole, and the next act
/// removes every draft arc abandoned and publishes. A file under the draft
/// names that is not arc's is left as it is and stops nothing. Rejects an
/// index truncated and rewritten in place, and an abandoned or foreign
/// draft that keeps the index out of date.
#[test]
fn an_index_rewrite_leaves_the_old_index_whole_until_the_new_one_replaces_it() {
    use std::io::Read;
    let repo = Repo::new();
    let path = open_workbench(&repo, "lane-fanout", "lane indexes and reports");
    let index = path.join("WORKBENCH.md");
    let opened = fs::read_to_string(&index).unwrap();
    let mut reader = fs::File::open(&index).unwrap();
    let (_, first) = journal_artifact(&repo, "first", "plan", "# First\n");
    let link = |artifact: &str| {
        let mut command = repo.arc(&repo.root);
        command.args([
            "journal",
            "workbench",
            "link",
            "lane-fanout",
            "--artifact",
            artifact,
        ]);
        command
    };
    stdout(&mut link(&first));
    let mut held = String::new();
    reader.read_to_string(&mut held).unwrap();
    assert_eq!(held, opened, "the open index changed under its reader");
    let linked = fs::read_to_string(&index).unwrap();
    assert!(
        linked.contains(&format!("Linked artifacts: {first}")),
        "{linked}"
    );

    // A draft a rewrite abandoned under the bare reserved name, holding
    // half of the marker, and one a foreign hand left under the pattern.
    fs::write(
        path.join(".WORKBENCH.md.arc-draft"),
        &MARKER[..MARKER.len() / 2],
    )
    .unwrap();
    fs::write(path.join(".WORKBENCH.md.arc-draft-mine"), "my own notes\n").unwrap();
    let mut linked_artifacts = vec![first.clone()];
    for (topic, stage) in [
        ("second", "created"),
        ("third", "partial"),
        ("fourth", "written"),
    ] {
        let (_, artifact) = journal_artifact(&repo, topic, "plan", "# Plan\n");
        let interrupted = link(&artifact)
            .env("ARC_TEST_WORKBENCH_INDEX_INTERRUPT", stage)
            .output()
            .unwrap();
        assert_eq!(
            interrupted.status.code(),
            Some(9),
            "{stage}: {interrupted:?}"
        );
        assert_eq!(fs::read_to_string(&index).unwrap(), linked, "{stage}");
        // The draft this attempt abandoned, beside the foreign one: each
        // attempt removed what earlier ones abandoned.
        let left = drafts(&path);
        assert_eq!(left.len(), 2, "{stage}: {left:?}");
        assert!(left.contains(&".WORKBENCH.md.arc-draft-mine".to_string()));
        linked_artifacts.push(artifact);
    }
    stdout(repo.arc(&repo.root).args([
        "journal",
        "workbench",
        "close",
        "lane-fanout",
        "--note",
        "done",
    ]));
    let closed = fs::read_to_string(&index).unwrap();
    assert!(
        closed.contains(&format!(
            "Linked artifacts: {}",
            linked_artifacts.join(", ")
        )),
        "{closed}"
    );
    assert!(closed.contains("State: closed"), "{closed}");
    assert_eq!(drafts(&path), [".WORKBENCH.md.arc-draft-mine"]);
    assert_eq!(
        fs::read_to_string(path.join(".WORKBENCH.md.arc-draft-mine")).unwrap(),
        "my own notes\n"
    );
}

fn mkfifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Claim: a named pipe where arc reads a file inside a workbench, under a
/// draft name or at `WORKBENCH.md`, never makes a rewrite, an adoption in
/// place or a clone wait on it: each finishes, leaves the pipe as it is,
/// and treats it as a file arc did not write. Rejects opening a pipe for
/// reading by name, which blocks until a writer appears.
#[test]
fn a_named_pipe_in_a_workbench_never_blocks_arc() {
    let repo = Repo::new();
    let timeout = std::time::Duration::from_secs(20);
    let path = open_workbench(&repo, "pipes", "pipe fixtures");
    mkfifo(&path.join(".WORKBENCH.md.arc-draft-pipe"));
    let (_, artifact) = journal_artifact(&repo, "piped", "plan", "# Plan\n");
    repo.arc(&repo.root)
        .args([
            "journal",
            "workbench",
            "link",
            "pipes",
            "--artifact",
            &artifact,
        ])
        .timeout(timeout)
        .assert()
        .success();
    let index = fs::read_to_string(path.join("WORKBENCH.md")).unwrap();
    assert!(index.contains(&artifact), "{index}");

    fs::remove_file(path.join("WORKBENCH.md")).unwrap();
    mkfifo(&path.join("WORKBENCH.md"));
    let output = repo
        .arc(&repo.root)
        .args(["journal", "workbench", "close", "pipes", "--note", "done"])
        .timeout(timeout)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(stderr.contains("is not a file arc wrote"), "{stderr}");

    let dir = journal_dir(&repo);
    let unrecorded = dir.join("workbench/stray-20261009");
    fs::create_dir_all(&unrecorded).unwrap();
    mkfifo(&unrecorded.join("WORKBENCH.md"));
    let output = repo
        .arc(&repo.root)
        .args([
            "journal",
            "workbench",
            "adopt",
            unrecorded.to_str().unwrap(),
            "--purpose",
            "stray",
        ])
        .timeout(timeout)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(stderr.contains("is not an index arc wrote"), "{stderr}");
    fs::remove_dir_all(&unrecorded).unwrap();

    let prefix = repo.home.join("boxed");
    let report = json_stdout(
        repo.arc(&repo.root)
            .args(["sandbox", "clone", prefix.to_str().unwrap(), "--json"])
            .timeout(timeout),
    );
    let copied = PathBuf::from(report["journal"].as_str().unwrap())
        .join("workbench")
        .join(path.file_name().unwrap());
    assert!(copied.is_dir());
    assert!(fs::symlink_metadata(copied.join("WORKBENCH.md")).is_err());
    let whole = repo.home.join("whole");
    repo.arc(&repo.root)
        .args([
            "sandbox",
            "clone",
            whole.to_str().unwrap(),
            "--workbench-content",
        ])
        .timeout(timeout)
        .assert()
        .success();
    assert!(path.join("WORKBENCH.md").exists());
}

/// Claim: a rewrite keeps the index's permission bits and its access ACL:
/// an index its owner made private stays private after arc rewrites it, and
/// one shared with a named user stays shared with exactly that user. Rejects
/// a rewrite that replaces a `0600` index with a draft made under the
/// umask, widening it, or that drops an ACL, which leaves the ACL's mask as
/// the owning group's permission. The ACL half runs where the fixture's
/// filesystem takes one.
#[test]
fn an_index_rewrite_keeps_its_mode() {
    let repo = Repo::new();
    let path = open_workbench(&repo, "private", "private notes");
    let index = path.join("WORKBENCH.md");
    fs::set_permissions(&index, fs::Permissions::from_mode(0o600)).unwrap();
    let (_, artifact) = journal_artifact(&repo, "secret", "plan", "# Plan\n");
    repo.arc_under_umask(&repo.root, "022")
        .args([
            "journal",
            "workbench",
            "link",
            "private",
            "--artifact",
            &artifact,
        ])
        .assert()
        .success();
    let rewritten = fs::read_to_string(&index).unwrap();
    assert!(rewritten.contains(&artifact), "{rewritten}");
    assert_eq!(fs::metadata(&index).unwrap().mode() & 0o7777, 0o600);

    let acl = |args: &[&str]| {
        std::process::Command::new(args[0])
            .args(&args[1..])
            .output()
    };
    let index_path = index.to_str().unwrap();
    let shared = acl(&["setfacl", "-m", "u:nobody:r", index_path]);
    if !shared.is_ok_and(|output| output.status.success()) {
        return;
    }
    let before = acl(&["getfacl", "-c", index_path]).unwrap().stdout;
    let mode = fs::metadata(&index).unwrap().mode();
    let (_, other) = journal_artifact(&repo, "shared", "plan", "# Plan\n");
    repo.arc_under_umask(&repo.root, "022")
        .args([
            "journal",
            "workbench",
            "link",
            "private",
            "--artifact",
            &other,
        ])
        .assert()
        .success();
    assert!(fs::read_to_string(&index).unwrap().contains(&other));
    let after = acl(&["getfacl", "-c", index_path]).unwrap().stdout;
    assert_eq!(
        String::from_utf8_lossy(&after),
        String::from_utf8_lossy(&before)
    );
    assert!(String::from_utf8_lossy(&after).contains("user:nobody:r--"));
    assert_eq!(fs::metadata(&index).unwrap().mode(), mode);
}

/// Claim: where `/dev/fd` or `/proc/self/fd` is missing, adopting, closing
/// and archiving refuse before reading or moving anything, naming the
/// missing directory, and leave the material and the records as they were;
/// a listing gives the size as unknown, naming it. Rejects a failure deep in
/// a walk that blames the material's own path, and a move made where arc
/// could not say where an escaped directory went.
#[test]
fn without_descriptor_directories_workbench_acts_refuse_clearly() {
    let repo = Repo::new();
    let dir = journal_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    let path = open_workbench(&repo, "finished", "finished work");
    fs::write(path.join("result.txt"), "done\n").unwrap();
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    let source = dir.join("loose/sweep-20261009");
    loose_entry(&source);
    let before = digests(&source);

    // Neither directory, then `/dev/fd` alone.
    let bare = repo.home.join("bare-root");
    let partial = repo.home.join("partial-root");
    fs::create_dir_all(partial.join("dev")).unwrap();
    std::os::unix::fs::symlink("/dev/fd", partial.join("dev/fd")).unwrap();
    for (root, missing) in [(&bare, "dev/fd"), (&partial, "proc/self/fd")] {
        let missing = format!("{} is not available here", root.join(missing).display());
        let refused = |args: &[&str]| {
            let output = repo
                .arc(&repo.root)
                .env("ARC_TEST_WORKBENCH_DESCRIPTOR_ROOT", root)
                .args(args)
                .assert()
                .failure();
            let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
            assert!(stderr.contains(&missing), "{args:?}: {stderr}");
            assert!(!stderr.contains("cannot read"), "{args:?}: {stderr}");
        };
        refused(&[
            "journal",
            "workbench",
            "adopt",
            source.to_str().unwrap(),
            "--purpose",
            "old material",
        ]);
        refused(&["journal", "workbench", "close", &name, "--note", "done"]);
    }
    assert_eq!(digests(&source), before);
    let kinds: Vec<_> = workbench_events(&dir)
        .iter()
        .map(|event| event["event"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["workbench-opened"]);

    let listing = json_stdout(
        repo.arc(&repo.root)
            .env("ARC_TEST_WORKBENCH_DESCRIPTOR_ROOT", &bare)
            .args(["journal", "workbench", "list", "--json"]),
    );
    let row = &listing["workbenches"][0];
    assert_eq!(row["files"], serde_json::Value::Null, "{row}");
    assert!(
        row["size_error"]
            .as_str()
            .unwrap()
            .contains("dev/fd is not available here"),
        "{row}"
    );

    stdout(
        repo.arc(&repo.root)
            .args(["journal", "workbench", "close", &name, "--note", "done"]),
    );
    let output = repo
        .arc(&repo.root)
        .env("ARC_TEST_WORKBENCH_DESCRIPTOR_ROOT", &partial)
        .args(["journal", "archive", &name])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&output.get_output().stderr).to_string();
    assert!(
        stderr.contains("proc/self/fd is not available here"),
        "{stderr}"
    );
    assert!(path.join("result.txt").is_file());
    assert_eq!(listed(&repo, true)[0]["storage"], "hot");
}
