use super::common::*;
use super::{
    journal_exchange::pair,
    progress::{sibling_project, without_timestamps},
};
use serde_json::Value;

fn arc(repo: &Repo, args: &[&str]) -> String {
    stdout(repo.arc(&repo.root).args(args))
}

fn start_series(repo: &Repo, cwd: &Path, topic: &str) -> String {
    stdout(
        repo.arc(cwd)
            .args(["progress", "start", topic, "--title", topic]),
    )
    .lines()
    .next()
    .unwrap()
    .to_string()
}

fn report(repo: &Repo, file: &str) -> Value {
    json_stdout(repo.arc(&repo.root).args(["milestone", file, "--json"]))["milestones"][0].clone()
}

fn refuses(repo: &Repo, args: &[&str], why: &str) {
    repo.arc(&repo.root)
        .args(args)
        .assert()
        .failure()
        .stderr(predicates::str::contains(why.to_string()));
}

/// An open milestone replays ordered wave and track edits, binds the recorded
/// checkpoint identity, and survives exchange after closure. Rejects ignored
/// transitions, an unbound checkpoint, and writes to a closed milestone.
#[test]
fn milestone_lifecycle_replays_through_exchange() {
    let source = Repo::new();
    let recipient = Repo::new();
    pair(&source, &recipient, "author", "recipient");
    let other = sibling_project(&source, "other");
    let local = start_series(&source, &source.root, "local");
    let remote = start_series(&source, &other, "remote");
    let remote_ref = format!("other::{remote}");
    let (_, plan) = journal_artifact(&source, "roadmap", "plan", "# Roadmap\n");
    let (_, conclusion) = journal_artifact(&source, "complete", "conclusion", "# Complete\n");

    let file = arc(
        &source,
        &[
            "milestone",
            "start",
            "shipping",
            "--title",
            "Shipping milestone",
            "--follows",
            &plan,
            "--wave",
            "w0=Prepare",
            "--wave",
            "w1",
            "--advisory-waves",
        ],
    )
    .trim()
    .to_string();
    let initial = report(&source, &file);
    assert_eq!(initial["title"], "Shipping milestone");
    assert_eq!(initial["follows"], plan);
    assert_eq!(initial["advisory_waves"], true);
    assert_eq!(initial["waves"][0]["title"], "Prepare");

    arc(
        &source,
        &[
            "milestone",
            "wave",
            "add",
            "shipping",
            "phase-1",
            "--before",
            "w1",
            "--gated",
        ],
    );
    assert_eq!(report(&source, &file)["waves"][1]["id"], "phase-1");
    arc(
        &source,
        &["milestone", "wave", "rename", &file, "phase-1", "Release"],
    );
    arc(
        &source,
        &[
            "milestone",
            "wave",
            "move",
            &file,
            "phase-1",
            "--after",
            "w1",
        ],
    );
    arc(&source, &["milestone", "wave", "remove", &file, "w0"]);
    arc(
        &source,
        &["milestone", "wave", "add", &file, "follow_up", "--soft"],
    );
    refuses(
        &source,
        &["milestone", "wave", "move", &file, "w1", "--after", "w1"],
        "relative to itself",
    );

    arc(
        &source,
        &[
            "milestone",
            "track",
            "add",
            &file,
            "local",
            "--label",
            "Main",
        ],
    );
    arc(
        &source,
        &[
            "--role",
            "implementer",
            "milestone",
            "track",
            "add",
            &file,
            "other::remote",
            "--before",
            "local",
            "--role",
            "sideband",
        ],
    );
    let tracks = report(&source, &file)["tracks"].clone();
    assert_eq!(tracks[0]["file"], remote);
    assert_eq!(tracks[0]["role"], "sideband");
    let remote_dir = stdout(source.arc(&other).args(["journal", "dir"]));
    let remote_slug = Path::new(remote_dir.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(tracks[0]["project"], remote_slug);
    assert_eq!(tracks[1]["role"], "critical");
    refuses(
        &source,
        &["milestone", "track", "add", &file, &remote_ref],
        "already exists",
    );
    arc(
        &source,
        &["milestone", "track", "role", &file, &remote_ref, "critical"],
    );
    arc(
        &source,
        &["milestone", "track", "role", &file, "local", "sideband"],
    );
    arc(
        &source,
        &[
            "milestone",
            "track",
            "move",
            &file,
            &remote_ref,
            "--after",
            "local",
        ],
    );
    let tracks = report(&source, &file)["tracks"].clone();
    assert_eq!(tracks[0]["file"], local);
    assert_eq!(tracks[0]["label"], "Main");
    assert_eq!(tracks[1]["role"], "critical");
    arc(&source, &["milestone", "track", "remove", &file, "local"]);
    arc(
        &source,
        &["milestone", "track", "remove", &file, &remote_ref],
    );
    assert_eq!(report(&source, &file)["tracks"], serde_json::json!([]));
    arc(&source, &["milestone", "track", "add", &file, &remote_ref]);

    refuses(
        &source,
        &["milestone", "checkpoint", &file, "unrecorded"],
        "no checkpoint named",
    );
    arc(&source, &["checkpoint", "record", "preview"]);
    arc(&source, &["milestone", "checkpoint", &file, "preview"]);
    arc(&source, &["checkpoint", "record", "release"]);
    refuses(&source, &["milestone", "close", &file], "required");
    arc(
        &source,
        &[
            "milestone",
            "close",
            &file,
            "--checkpoint",
            "release",
            "--by",
            &conclusion,
            "--note",
            "Shipped",
        ],
    );

    let expected = report(&source, &file);
    let waves = expected["waves"].as_array().unwrap();
    assert_eq!(
        waves
            .iter()
            .map(|wave| wave["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["w1", "phase-1", "follow_up"]
    );
    assert_eq!(waves[1]["title"], "Release");
    assert_eq!(waves[1]["gated"], true);
    assert_eq!(waves[2]["gated"], false);
    assert_eq!(expected["tracks"][0]["file"], remote);
    assert_eq!(expected["closure"]["by"], conclusion);
    assert_eq!(expected["closure"]["outcome"], "superseded");
    assert_eq!(expected["closure"]["note"], "Shipped");
    assert_eq!(expected["checkpoints"].as_array().unwrap().len(), 2);
    assert_eq!(expected["checkpoints"][0]["name"], "preview");
    assert_eq!(expected["checkpoints"][1]["name"], "release");
    for checkpoint in expected["checkpoints"].as_array().unwrap() {
        assert_eq!(
            checkpoint["revision"],
            git_out(&source.root, &["rev-parse", "HEAD"])
        );
        assert!(!checkpoint["event_id"].as_str().unwrap().is_empty());
    }
    assert_eq!(
        expected["closure"]["checkpoint"],
        expected["checkpoints"][1]
    );
    let plain = arc(&source, &["milestone", "shipping", "--plain"]);
    assert!(plain.contains("Wave w1 [advisory]"), "{plain}");
    assert!(plain.contains("Wave phase-1 [gated] Release"), "{plain}");
    assert!(plain.contains("At checkpoint release"), "{plain}");
    assert_eq!(
        json_stdout(source.arc(&source.root).args(["milestone", "--json"]))["milestones"],
        serde_json::json!([])
    );

    for cmd in [
        vec!["wave", "add", &file, "w9"],
        vec!["wave", "rename", &file, "w1", "Closed"],
        vec!["wave", "move", &file, "w1", "--after", "phase-1"],
        vec!["wave", "remove", &file, "w1"],
        vec!["track", "add", &file, "local"],
        vec!["track", "role", &file, &remote_ref, "sideband"],
        vec!["track", "move", &file, &remote_ref, "--after", "local"],
        vec!["track", "remove", &file, &remote_ref],
        vec!["checkpoint", &file, "release"],
        vec!["close", &file, "--note", "Already closed"],
    ] {
        refuses(
            &source,
            &[&["milestone"], cmd.as_slice()].concat(),
            "closed and read-only",
        );
    }
    arc(&source, &["journal", "doctor"]);
    let bundle = source.home.join("milestone.json");
    let bundle = bundle.to_str().unwrap();
    arc(
        &source,
        &[
            "journal", "export", &file, &plan, &local, "--output", bundle,
        ],
    );
    arc(&recipient, &["journal", "import", bundle]);
    arc(&recipient, &["journal", "doctor"]);
    assert_eq!(report(&recipient, &file), expected);
}

/// Milestones can be pulled by name without entering the actionable queues.
/// Rejects a report that pushes an open milestone or its waves into a queue.
#[test]
fn milestones_are_pull_only() {
    let repo = Repo::new();
    let change = begin_change(&repo, "quiet", None);
    let file = arc(
        &repo,
        &[
            "milestone",
            "start",
            "quiet-milestone",
            "--title",
            "Unique milestone title",
        ],
    );
    let read = |args: &[&str]| {
        without_timestamps(&without_clock(&stdout_any_status(
            repo.arc(&repo.root).args(args),
        )))
    };
    let reads = [
        &["status", &change][..],
        &["check", &change][..],
        &["catchup"][..],
        &["catchup", "--json"][..],
        &["journal", "open"][..],
    ];
    let before = reads.map(read);
    assert!(!before[4].contains(file.trim()), "{}", before[4]);
    arc(
        &repo,
        &[
            "milestone",
            "wave",
            "add",
            file.trim(),
            "w0",
            "--title",
            "Unique wave title",
        ],
    );
    assert_eq!(reads.map(read), before);
    assert!(arc(&repo, &["milestone"]).contains("Unique wave title"));
    arc(
        &repo,
        &["milestone", "close", "quiet-milestone", "--note", "Done"],
    );
    assert_eq!(report(&repo, file.trim())["closure"]["outcome"], "done");
}

/// Invalid starts and track references leave no milestone transition behind.
/// Rejects partial starts and a symlink accepted as a series artifact.
#[test]
fn invalid_start_and_track_references_do_not_append() {
    let repo = Repo::new();
    refuses(
        &repo,
        &[
            "milestone",
            "start",
            "invalid",
            "--title",
            "Invalid",
            "--wave",
            "w0",
            "--wave",
            "w0=Duplicate",
        ],
        "already exists",
    );
    refuses(
        &repo,
        &[
            "milestone",
            "start",
            "invalid",
            "--title",
            "Invalid",
            "--wave",
            "phase--1",
        ],
        "wave id",
    );
    assert_eq!(
        json_stdout(repo.arc(&repo.root).args(["milestone", "--json"]))["milestones"],
        serde_json::json!([])
    );
    let file = arc(&repo, &["milestone", "start", "valid", "--title", "Valid"]);
    let file = file.trim();
    let local = start_series(&repo, &repo.root, "local");
    repo.arc(&repo.root)
        .env("ARC_ROLE", "invalid")
        .args([
            "milestone",
            "track",
            "add",
            file,
            &local,
            "--role",
            "sideband",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid execution role"));
    let project = repo.root.file_name().unwrap().to_str().unwrap();
    let self_ref = format!("{project}::{local}");
    refuses(
        &repo,
        &["milestone", "track", "add", file, &self_ref],
        "is this project",
    );
    let dir = arc(&repo, &["journal", "dir"]);
    let local_body = Path::new(dir.trim()).join(&local);
    let outside = repo.home.join("outside.md");
    fs::rename(&local_body, &outside).unwrap();
    std::os::unix::fs::symlink(outside, local_body).unwrap();
    refuses(
        &repo,
        &["milestone", "track", "add", file, &local],
        "no regular artifact",
    );
    assert_eq!(report(&repo, file)["tracks"], serde_json::json!([]));
}
