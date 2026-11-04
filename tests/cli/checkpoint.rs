//! Checkpoints: the revision somebody is testing and what was open at it,
//! read back against what landed, closed and was filed since.

use crate::common::*;
use crate::retention::{collect_garbage, has_object};
use serde_json::Value;

fn todo(repo: &Repo, topic: &str) -> String {
    journal_artifact(repo, topic, "todo", &format!("# {topic}\n")).1
}

/// Open a change with its own checkout, which `complete_change` commits in.
fn open(repo: &Repo, slug: &str) -> String {
    opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])))
}

fn show(repo: &Repo, name: &str) -> Value {
    json_stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "show", name, "--json"]),
    )
}

fn list(repo: &Repo) -> Value {
    json_stdout(repo.arc(&repo.root).args(["checkpoint", "list", "--json"]))
}

fn strings(value: &Value, field: &str) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap().to_string())
        .collect()
}

/// Claim: a checkpoint holds exactly the changes and todos open when it was
/// recorded, at the revision named, and `show` reads each against now: the
/// in-flight change as integrated outside the checkpoint and landed since,
/// one todo as consumed since and the other as still open, a todo filed
/// afterwards as filed since and not part of the checkpoint, and a change
/// opened afterwards and integrated as landed since. Rejects: a checkpoint
/// missing or adding an item, a change integrated before the checkpoint
/// read as landed since (timestamps instead of ancestry), a later todo
/// counted as part of the checkpoint, and a consumption left unreported.
#[test]
fn show_sorts_what_was_open_at_the_checkpoint_from_what_happened_since() {
    let repo = Repo::new();
    open(&repo, "early");
    complete_change(&repo, "early");
    let first = todo(&repo, "first-finding");
    let second = todo(&repo, "second-finding");
    let (flight, flight_worktree, flight_head) = change_with_patchset(&repo, "flight");
    let revision = git_out(&repo.root, &["rev-parse", "master"]);

    let recorded = stdout(repo.arc(&repo.root).args([
        "checkpoint",
        "record",
        "beta-1",
        "--rev",
        "master",
        "--note",
        "operator build",
    ]));
    assert!(recorded.contains("open changes: 1"), "{recorded}");
    assert!(recorded.contains("open todos: 2"), "{recorded}");

    let at = show(&repo, "beta-1");
    assert_eq!(at["schema"], "arc-checkpoint/1");
    assert_eq!(at["revision"], revision.as_str());
    assert_eq!(at["branch"], "master");
    assert_eq!(at["note"], "operator build");
    assert_eq!(strings(&at["changes"], "change_id"), [flight.as_str()]);
    assert_eq!(at["changes"][0]["head"], flight_head.as_str());
    assert_eq!(at["changes"][0]["status"], "open");
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort();
    assert_eq!(strings(&at["todos"], "file"), expected);
    assert_eq!(at["todos"][0]["title"], "first-finding");
    assert!(at["landed_since"].as_array().unwrap().is_empty(), "{at}");
    assert!(at["filed_since"].as_array().unwrap().is_empty(), "{at}");

    repo.arc(&flight_worktree)
        .args(["review", "flight", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "flight"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["journal", "consume", &first, "--outcome", "done"])
        .assert()
        .success();
    let third = todo(&repo, "third-finding");
    let late = open(&repo, "late");
    complete_change(&repo, "late");

    let now = show(&repo, "beta-1");
    assert_eq!(strings(&now["changes"], "change_id"), [flight.as_str()]);
    assert_eq!(now["changes"][0]["status"], "integrated");
    assert_eq!(now["changes"][0]["in_checkpoint"], false);
    assert!(now["changes"][0]["integrated_commit"].is_string(), "{now}");

    let landed = strings(&now["landed_since"], "change_id");
    assert_eq!(landed.len(), 2, "{now}");
    assert!(landed.contains(&flight) && landed.contains(&late), "{now}");
    for entry in now["landed_since"].as_array().unwrap() {
        let open_then = entry["change_id"] == flight.as_str();
        assert_eq!(entry["open_at_checkpoint"], open_then, "{entry}");
        assert_eq!(entry["target_branch"], "master");
    }

    let todos = now["todos"].as_array().unwrap();
    assert_eq!(strings(&now["todos"], "file"), expected);
    let consumed = todos.iter().find(|t| t["file"] == first.as_str()).unwrap();
    assert_eq!(consumed["status"], "consumed");
    assert_eq!(consumed["outcome"], "done");
    assert!(consumed["consumed_at"].is_string(), "{consumed}");
    let open = todos.iter().find(|t| t["file"] == second.as_str()).unwrap();
    assert_eq!(open["status"], "open");

    assert_eq!(strings(&now["filed_since"], "file"), [third.as_str()]);
    assert_eq!(now["filed_since"][0]["status"], "open");

    let text = stdout(repo.arc(&repo.root).args(["checkpoint", "show", "beta-1"]));
    assert!(text.contains("todos open at the checkpoint:"), "{text}");
    assert!(text.contains("consumed done at"), "{text}");
    assert!(
        text.contains("filed since (not part of the checkpoint):"),
        "{text}"
    );

    let listed = list(&repo);
    assert_eq!(listed["schema"], "arc-checkpoint-list/1");
    assert_eq!(listed["checkpoints"][0]["name"], "beta-1");
    assert_eq!(listed["checkpoints"][0]["open_changes"], 1);
    assert_eq!(listed["checkpoints"][0]["open_todos"], 2);
}

/// Claim: `--tag` writes the annotated tag `checkpoint/<name>` on the
/// recorded revision; a tag already standing there refuses before anything
/// is recorded, and a second record under one name refuses naming the
/// first. Rejects: a lightweight or misplaced tag, an event written beside
/// a tag that could not be made, and two checkpoints sharing a name.
#[test]
fn tags_and_names_are_unique_and_a_standing_tag_records_nothing() {
    let repo = Repo::new();
    let revision = git_out(&repo.root, &["rev-parse", "HEAD"]);
    repo.commit(&repo.root, "later.txt", "later\n", "later");

    stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "record", "rc", "--rev", &revision, "--tag"]),
    );
    assert_eq!(
        git_out(&repo.root, &["cat-file", "-t", "refs/tags/checkpoint/rc"]),
        "tag"
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "checkpoint/rc^{commit}"]),
        revision
    );
    assert_eq!(show(&repo, "rc")["tag"], "checkpoint/rc");

    let again = repo
        .arc(&repo.root)
        .args(["checkpoint", "record", "rc"])
        .output()
        .unwrap();
    assert!(!again.status.success());
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("already recorded"), "{stderr}");
    assert!(stderr.contains(&revision), "{stderr}");

    git(&repo.root, &["tag", "checkpoint/standing", &revision]);
    let refused = repo
        .arc(&repo.root)
        .args(["checkpoint", "record", "standing", "--tag"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("already exists"), "{stderr}");

    let listed = list(&repo);
    assert_eq!(strings(&listed["checkpoints"], "name"), ["rc"]);
    assert_eq!(listed["checkpoints"][0]["tag"], "checkpoint/rc");
}

/// Claim: a handoff naming a checkpoint carries one line naming it and its
/// revision, and a name this repository does not record refuses with
/// nothing written. Rejects: a dangling checkpoint reference in a handoff.
#[test]
fn a_handoff_names_a_recorded_checkpoint_and_its_revision() {
    let repo = Repo::new();
    let revision = git_out(&repo.root, &["rev-parse", "HEAD"]);
    stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "record", "beta-2"]),
    );
    let body = repo.home.join("handoff-body.md");
    std::fs::write(&body, "Waiting on the operator's findings.\n").unwrap();

    let missing = repo
        .arc(&repo.root)
        .args([
            "journal",
            "handoff",
            "testing",
            "--body-file",
            body.to_str().unwrap(),
            "--checkpoint",
            "beta-3",
        ])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no checkpoint named"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let dir =
        std::path::PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim());
    assert!(
        !std::fs::read_dir(&dir)
            .map(|entries| entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with("-handoff.md")))
            .unwrap_or(false),
        "a refused handoff wrote an artifact"
    );

    let path = stdout(repo.arc(&repo.root).args([
        "journal",
        "handoff",
        "testing",
        "--body-file",
        body.to_str().unwrap(),
        "--checkpoint",
        "beta-2",
    ]));
    let written = std::fs::read_to_string(path.trim()).unwrap();
    let line = format!("Checkpoint: beta-2 at {revision}");
    assert_eq!(written.matches(&line).count(), 1, "{written}");
    assert!(written.contains("Waiting on the operator's findings."));
}

/// Claim: when this repository no longer holds the checkpoint's revision,
/// `show` says ancestry cannot be judged against it and classifies nothing
/// by it. Rejects: a change integrated before the checkpoint listed as
/// landed since, and a change open at the checkpoint read as integrated
/// outside it, because a failed ancestry check was taken for "not an
/// ancestor".
#[test]
fn a_missing_checkpoint_revision_reports_the_gap_instead_of_landed_since() {
    let repo = Repo::new();
    let early = open(&repo, "early");
    complete_change(&repo, "early");
    let (flight, flight_worktree, _) = change_with_patchset(&repo, "flight");
    let tree = git_out(&repo.root, &["rev-parse", "master^{tree}"]);
    let parent = git_out(&repo.root, &["rev-parse", "master"]);
    let tested = git_out(
        &repo.root,
        &["commit-tree", &tree, "-p", &parent, "-m", "operator build"],
    );
    stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "record", "lost", "--rev", &tested]),
    );
    repo.arc(&flight_worktree)
        .args(["review", "flight", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "flight"])
        .assert()
        .success();
    let object = git_out(
        &repo.root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &format!("objects/{}/{}", &tested[..2], &tested[2..]),
        ],
    );
    std::fs::remove_file(&object).unwrap();

    let view = show(&repo, "lost");
    let landed: Vec<&str> = view["landed_since"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["change_id"].as_str().unwrap())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !landed.contains(&early.as_str()),
        "a change inside the checkpoint read as landed since: {view}"
    );
    assert!(view["landed_since"].is_null(), "{view}");
    let gap = view["ancestry_unknown"].as_str().unwrap_or_default();
    assert!(gap.contains(&tested), "{view}");
    assert_eq!(view["changes"][0]["change_id"], flight.as_str());
    assert_eq!(view["changes"][0]["status"], "integrated");
    assert!(view["changes"][0]["in_checkpoint"].is_null(), "{view}");
    assert!(
        view["changes"][0]["in_checkpoint_unknown"].is_string(),
        "{view}"
    );

    let text = stdout(repo.arc(&repo.root).args(["checkpoint", "show", "lost"]));
    assert!(text.contains("landed since: unknown"), "{text}");
    assert!(text.contains("inside the checkpoint unknown"), "{text}");
}

/// Claim: a todo filed and consumed before the checkpoint is not filed
/// since, even when its journal events carry the checkpoint's own second.
/// Rejects: classifying "filed since" by comparing journal and ledger
/// clocks, which lists such a todo as filed after the checkpoint.
#[test]
fn a_todo_consumed_before_the_checkpoint_is_not_filed_since() {
    let repo = Repo::new();
    let (dir, done) = journal_artifact(&repo, "done-before", "todo", "# done-before\n");
    repo.arc(&repo.root)
        .args(["journal", "consume", &done, "--outcome", "done"])
        .assert()
        .success();
    stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "record", "same-second"]),
    );
    let recorded_at = show(&repo, "same-second")["recorded_at"]
        .as_str()
        .unwrap()
        .to_string();
    let second = format!("{}Z", &recorded_at[..19]);
    let log = dir.join("events.jsonl");
    let rewritten: String = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| {
            let mut event: Value = serde_json::from_str(line).unwrap();
            if event["file"] == done.as_str() {
                event["ts"] = Value::String(second.clone());
            }
            format!("{event}\n")
        })
        .collect();
    std::fs::write(&log, rewritten).unwrap();
    let later = todo(&repo, "after-the-checkpoint");

    let view = show(&repo, "same-second");
    assert_eq!(
        strings(&view["filed_since"], "file"),
        [later.as_str()],
        "{view}"
    );
    assert_eq!(view["filed_since_exact"], true, "{view}");
    assert!(view["todos"].as_array().unwrap().is_empty(), "{view}");
}

/// Claim: a `checkpoint/<name>` tag that no checkpoint records, which an
/// interrupted `record --tag` leaves between writing the tag and its event,
/// refuses a retry naming the recovery, and once the tag is deleted with
/// `git tag -d` the retry records the checkpoint with a fresh tag. Rejects:
/// a refusal that names no way forward, and a record that adopts a tag it
/// did not write.
#[test]
fn an_unrecorded_checkpoint_tag_refuses_with_its_recovery() {
    let repo = Repo::new();
    let revision = git_out(&repo.root, &["rev-parse", "HEAD"]);
    git(
        &repo.root,
        &[
            "tag",
            "-a",
            "-m",
            "Checkpoint rc",
            "checkpoint/rc",
            &revision,
        ],
    );
    let refused = repo
        .arc(&repo.root)
        .args(["checkpoint", "record", "rc", "--tag"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("no checkpoint records it"), "{stderr}");
    assert!(stderr.contains("git tag -d checkpoint/rc"), "{stderr}");
    assert!(list(&repo)["checkpoints"].as_array().unwrap().is_empty());

    git(&repo.root, &["tag", "-d", "checkpoint/rc"]);
    stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "record", "rc", "--tag"]),
    );
    assert_eq!(show(&repo, "rc")["tag"], "checkpoint/rc");
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "checkpoint/rc^{commit}"]),
        revision
    );
}

fn record_untagged(repo: &Repo, name: &str, revision: &str) -> String {
    let recorded =
        stdout(
            repo.arc(&repo.root)
                .args(["checkpoint", "record", name, "--rev", revision]),
        );
    recorded
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap_or_else(|| panic!("{recorded}"))
        .to_string()
}

/// Claim: a recorded rewrite that replaces a checkpoint's revision holds the
/// revision under a retention ref, so garbage collection keeps it and `show`
/// still judges ancestry from it. Rejects: a checkpoint recorded without
/// `--tag` whose revision only the rewritten branch held, which the next
/// collection takes.
#[test]
fn a_rewrite_retains_the_revision_an_untagged_checkpoint_names() {
    let repo = Repo::new();
    let (flight, worktree, _) = change_with_patchset(&repo, "flight");
    repo.commit(&worktree, "build.txt", "one\n", "feat: tested build");
    let tested = repo.head(&worktree);
    let event = record_untagged(&repo, "beta", &tested);
    git(
        &worktree,
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--date",
            "2020-01-01T00:00:00Z",
        ],
    );
    let replacement = repo.head(&worktree);
    assert_ne!(replacement, tested);
    let map = repo.home.join("map");
    std::fs::write(&map, format!("{tested} {replacement}\n")).unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "amended after the checkpoint",
        ])
        .assert()
        .success();

    collect_garbage(&repo.root);
    assert!(
        has_object(&repo.root, &tested),
        "garbage collection took it"
    );
    let held = git_out(
        &repo.root,
        &[
            "rev-parse",
            &format!("refs/arc/retained/repository/{event}/{tested}"),
        ],
    );
    assert_eq!(held, tested);

    let view = show(&repo, "beta");
    assert_eq!(view["revision"], tested.as_str());
    assert_eq!(view["current_revision"], replacement.as_str());
    assert!(view["ancestry_unknown"].is_null(), "{view}");
    assert!(view["landed_since"].is_array(), "{view}");
    assert_eq!(view["changes"][0]["change_id"], flight.as_str());
    assert_eq!(view["changes"][0]["status"], "open");
}
