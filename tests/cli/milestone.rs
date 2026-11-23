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
    let imported = report(&recipient, &file);
    for field in [
        "file",
        "title",
        "follows",
        "advisory_waves",
        "checkpoints",
        "closure",
    ] {
        assert_eq!(imported[field], expected[field], "mismatch on {field}");
    }
    assert_eq!(
        imported["waves"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (&w["id"], &w["title"], &w["gated"]))
            .collect::<Vec<_>>(),
        expected["waves"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| (&w["id"], &w["title"], &w["gated"]))
            .collect::<Vec<_>>(),
    );
    assert_eq!(imported["tracks"][0]["file"], expected["tracks"][0]["file"]);
    assert_eq!(
        imported["tracks"][0]["project"],
        expected["tracks"][0]["project"]
    );
    assert_eq!(imported["tracks"][0]["role"], expected["tracks"][0]["role"]);
    // Recipient does not have sibling repo `other` registered, so its track degrades cleanly.
    assert!(imported["tracks"][0]["unavailable"].is_string());
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

#[test]
fn milestone_matrix_separates_critical_from_sideband_and_enforces_wave_barriers() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");

    // Local critical track (`core`) with waves w0, w1, w2.
    let core = stdout(repo.arc(&repo.root).args([
        "progress",
        "start",
        "core",
        "--title",
        "Core track",
        "--wave",
        "w0=Baseline",
        "--wave",
        "w1=Cut",
        "--wave",
        "w2=Polish",
        "--item",
        "Core baseline",
        "--item",
        "Core cut",
        "--item",
        "Core polish",
        "--item-wave",
        "i1=w0",
        "--item-wave",
        "i2=w1",
        "--item-wave",
        "i3=w2",
    ]))
    .lines()
    .next()
    .unwrap()
    .trim()
    .to_string();

    // Cross-project critical track (`other::ui`) with waves w0, w1.
    let ui = stdout(repo.arc(&other).args([
        "progress",
        "start",
        "ui",
        "--title",
        "UI track",
        "--wave",
        "w0=Baseline",
        "--wave",
        "w1=Cut",
        "--item",
        "UI baseline",
        "--item",
        "UI cut",
        "--item-wave",
        "i1=w0",
        "--item-wave",
        "i2=w1",
    ]))
    .lines()
    .next()
    .unwrap()
    .trim()
    .to_string();

    // Local sideband track (`cleanup`) with an incomplete w0 item — must NOT
    // count toward milestone critical % or block critical w1 items.
    let sideband = stdout(repo.arc(&repo.root).args([
        "progress",
        "start",
        "cleanup",
        "--title",
        "Sideband cleanup",
        "--wave",
        "w0=Baseline",
        "--item",
        "Opportunistic refactor",
        "--item-wave",
        "i1=w0",
    ]))
    .lines()
    .next()
    .unwrap()
    .trim()
    .to_string();

    let ms = arc(
        &repo,
        &[
            "milestone",
            "start",
            "rc3",
            "--title",
            "Release Candidate 3",
            "--wave",
            "w0=Baseline",
            "--wave",
            "w1=Parallel Cuts",
        ],
    )
    .trim()
    .to_string();
    arc(
        &repo,
        &[
            "milestone",
            "wave",
            "add",
            &ms,
            "w2",
            "--title",
            "Polish",
            "--soft",
        ],
    );
    arc(
        &repo,
        &["milestone", "track", "add", &ms, &core, "--label", "core"],
    );
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &ms,
            &format!("other::{ui}"),
            "--label",
            "ui",
        ],
    );
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &ms,
            &sideband,
            "--role",
            "sideband",
            "--label",
            "cleanup",
        ],
    );

    let doc = json_stdout(repo.arc(&repo.root).args(["milestone", &ms, "--json"]));
    assert_eq!(doc["schema"], "arc-milestone/1");
    let m = &doc["milestones"][0];

    // Critical counts exclude sideband (3 core + 2 ui = 5 critical, 1 sideband).
    assert_eq!(m["counts"]["critical"]["items"], 5);
    assert_eq!(m["counts"]["critical"]["complete"], 0);
    assert_eq!(m["counts"]["critical"]["percent"], 0);
    assert_eq!(m["counts"]["sideband"]["items"], 1);
    assert_eq!(m["counts"]["sideband"]["complete"], 0);

    // w1 is gated: both core:i2 and ui:i2 are blocked on w0 critical items.
    // w2 is --soft: core:i3 is NOT blocked by milestone wave barriers!
    assert_eq!(m["tracks"][0]["items"][0]["readiness"], "ready"); // core:i1 (w0)
    assert_eq!(m["tracks"][0]["items"][1]["readiness"], "blocked"); // core:i2 (w1 gated)
    assert_eq!(m["tracks"][0]["items"][2]["readiness"], "ready"); // core:i3 (w2 soft)
    assert_eq!(m["tracks"][1]["items"][0]["readiness"], "ready"); // ui:i1 (w0)
    assert_eq!(m["tracks"][1]["items"][1]["readiness"], "blocked"); // ui:i2 (w1 gated)
    assert_eq!(m["tracks"][2]["items"][0]["readiness"], "ready"); // sideband:i1

    // Finish core:i1 — ui:i1 in w0 is still open, so cross-track barrier still holds core:i2!
    arc(&repo, &["progress", "done", &core, "i1"]);
    let mid = report(&repo, &ms);
    assert_eq!(mid["counts"]["critical"]["complete"], 1);
    assert_eq!(mid["counts"]["critical"]["percent"], 20);
    assert_eq!(mid["tracks"][0]["items"][1]["readiness"], "blocked");
    assert_eq!(mid["tracks"][0]["items"][1]["blocked_by"][0]["wave"], "w0");
    assert_eq!(mid["tracks"][0]["items"][1]["blocked_by"][0]["series"], ui);

    // `--ignore-waves` unblocks w1 items at read time.
    let ignored = json_stdout(repo.arc(&repo.root).args([
        "milestone",
        &ms,
        "--ready",
        "--ignore-waves",
        "--json",
    ]));
    let ready_ids: Vec<(&str, &str)> = ignored["milestones"][0]["ready"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["track_label"].as_str().unwrap(),
                r["item"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(ready_ids.contains(&("core", "i2")));
    assert!(ready_ids.contains(&("ui", "i2")));

    // Finish ui:i1 in `other` — even though sideband w0 is STILL open, both critical w1 items unblock!
    stdout(repo.arc(&other).args(["progress", "done", &ui, "i1"]));
    let after = report(&repo, &ms);
    assert_eq!(after["counts"]["critical"]["complete"], 2);
    assert_eq!(after["counts"]["critical"]["percent"], 40);
    assert_eq!(after["tracks"][0]["items"][1]["readiness"], "ready");
    assert_eq!(after["tracks"][1]["items"][1]["readiness"], "ready");

    // Compact matrix output shows critical/sideband summary and wave columns.
    let compact = arc(&repo, &["milestone", &ms]);
    assert!(
        compact.contains("Critical: 2 of 5 complete (40%)"),
        "{compact}"
    );
    assert!(
        compact.contains("Sideband: 0 of 1 complete (0%)"),
        "{compact}"
    );
    assert!(compact.contains("core [critical]"), "{compact}");
    assert!(compact.contains("cleanup [sideband]"), "{compact}");
}

#[test]
fn milestone_degrades_gracefully_when_foreign_track_is_unreadable() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    let local = stdout(repo.arc(&repo.root).args([
        "progress",
        "start",
        "local",
        "--title",
        "Local",
        "--wave",
        "w0",
        "--wave",
        "w1",
        "--item",
        "Done w0",
        "--item",
        "Gated w1",
        "--item-wave",
        "i1=w0",
        "--item-wave",
        "i2=w1",
    ]))
    .lines()
    .next()
    .unwrap()
    .trim()
    .to_string();
    arc(&repo, &["progress", "done", &local, "i1"]);
    let remote = start_series(&repo, &other, "remote");

    let ms = arc(
        &repo,
        &[
            "milestone",
            "start",
            "degrade",
            "--title",
            "Graceful degradation",
            "--wave",
            "w0",
            "--wave",
            "w1",
        ],
    )
    .trim()
    .to_string();
    arc(&repo, &["milestone", "track", "add", &ms, &local]);
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &ms,
            &format!("other::{remote}"),
        ],
    );

    // Corrupt foreign journal events.jsonl so reading `other` fails.
    let other_dir = stdout(repo.arc(&other).args(["journal", "dir"]));
    fs::write(
        Path::new(other_dir.trim()).join("events.jsonl"),
        "not-json\n",
    )
    .unwrap();

    // Milestone read succeeds, marks foreign track unavailable, and holds gated w1 conservatively.
    let m = report(&repo, &ms);
    assert_eq!(m["counts"]["unavailable_tracks"], 1);
    assert!(m["tracks"][1]["unavailable"].is_string());
    assert_eq!(m["tracks"][0]["items"][1]["readiness"], "blocked");
    assert_eq!(
        m["tracks"][0]["items"][1]["blocked_by"][0]["stage"],
        "unknown"
    );
}

#[test]
fn milestone_defer_transfers_slice_across_milestones_and_preserves_provenance() {
    let repo = Repo::new();
    let rc2_core = stdout(repo.arc(&repo.root).args([
        "progress",
        "start",
        "rc2-core",
        "--title",
        "RC2 Core",
        "--wave",
        "w0",
        "--wave",
        "w1",
        "--item",
        "Ship now",
        "--item",
        "Defer me",
        "--item-wave",
        "i1=w0",
        "--item-wave",
        "i2=w1",
    ]))
    .lines()
    .next()
    .unwrap()
    .trim()
    .to_string();
    let rc3_core = stdout(repo.arc(&repo.root).args([
        "progress", "start", "rc3-core", "--title", "RC3 Core", "--wave", "w0", "--wave", "w1",
    ]))
    .lines()
    .next()
    .unwrap()
    .trim()
    .to_string();

    let rc2 = arc(
        &repo,
        &[
            "milestone",
            "start",
            "rc2",
            "--title",
            "RC2",
            "--wave",
            "w0",
            "--wave",
            "w1",
        ],
    )
    .trim()
    .to_string();
    let rc3 = arc(
        &repo,
        &[
            "milestone",
            "start",
            "rc3",
            "--title",
            "RC3",
            "--wave",
            "w0",
            "--wave",
            "w1",
        ],
    )
    .trim()
    .to_string();
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &rc2,
            &rc2_core,
            "--label",
            "core",
        ],
    );
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &rc3,
            &rc3_core,
            "--label",
            "core",
        ],
    );

    let out = arc(
        &repo,
        &[
            "milestone",
            "defer",
            "rc2",
            "core:i2",
            "--to",
            "rc3",
            "--wave",
            "w0",
            "--reason",
            "moved to rc3 wave 0",
        ],
    );
    assert!(out.contains("core:i2 ->"), "{out}");
    assert!(out.contains("core:i1 (wave w0)"), "{out}");

    let r2 = report(&repo, &rc2);
    let r3 = report(&repo, &rc3);
    assert_eq!(r2["counts"]["critical"]["items"], 1);
    assert_eq!(r2["tracks"][0]["items"][1]["stage"], "transferred");
    assert_eq!(
        r2["tracks"][0]["items"][1]["transferred_to"]["series"],
        rc3_core
    );
    assert_eq!(r3["counts"]["critical"]["items"], 1);
    assert_eq!(r3["tracks"][0]["items"][0]["title"], "Defer me");
    assert_eq!(r3["tracks"][0]["items"][0]["wave"], "w0");
    assert_eq!(
        r3["tracks"][0]["items"][0]["transferred_from"]["series"],
        rc2_core
    );

    arc(&repo, &["journal", "doctor"]);
}

#[test]
fn unavailable_track_edits_unassigned_wave_collision_and_symlinked_archive_refusal() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "vanishing");
    let start_s = |cwd: &Path, topic: &str, extra: &[&str]| -> String {
        let mut args = vec!["progress", "start", topic, "--title", topic];
        args.extend_from_slice(extra);
        stdout(repo.arc(cwd).args(&args))
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    let start_ms = |topic: &str, extra: &[&str]| -> String {
        let mut args = vec!["milestone", "start", topic];
        args.extend_from_slice(extra);
        arc(&repo, &args).lines().next().unwrap().to_string()
    };
    let jdir = |cwd: &Path| -> PathBuf {
        PathBuf::from(
            json_stdout(repo.arc(cwd).args(["progress", "--json"]))["journal_dir"]
                .as_str()
                .unwrap(),
        )
    };

    // 1. An unavailable foreign critical track can still be demoted and removed
    // by its recorded reference or label without opening the foreign journal.
    let local_s = start_s(
        &repo.root,
        "local-s",
        &[
            "--wave",
            "w0",
            "--wave",
            "w1",
            "--item",
            "Item 1",
            "--item-wave",
            "i1=w1",
        ],
    );
    let foreign_s = start_s(
        &other,
        "foreign-s",
        &[
            "--wave",
            "w0",
            "--item",
            "Foreign 1",
            "--item-wave",
            "i1=w0",
        ],
    );
    let ms = start_ms(
        "recover",
        &["--title", "Recover", "--wave", "w0", "--wave", "w1"],
    );
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &ms,
            &local_s,
            "--label",
            "local",
        ],
    );
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "add",
            &ms,
            &format!("vanishing::{foreign_s}"),
            "--label",
            "ext",
        ],
    );

    // Delete the foreign journal so the track becomes unavailable.
    let other_journal = jdir(&other);
    fs::remove_dir_all(&other_journal).unwrap();

    // Unavailable critical track blocks w1.
    let blocked = report(&repo, &ms);
    assert_eq!(blocked["counts"]["unavailable_tracks"], 1);
    assert_eq!(blocked["tracks"][0]["items"][0]["readiness"], "blocked");

    // Demote unavailable track to sideband by label -> w1 unblocks!
    arc(
        &repo,
        &["milestone", "track", "role", &ms, "ext", "sideband"],
    );
    let demoted = report(&repo, &ms);
    assert_eq!(demoted["tracks"][0]["items"][0]["readiness"], "ready");

    // Remove unavailable track by exact stored reference -> succeeds!
    let other_slug = other_journal
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    arc(
        &repo,
        &[
            "milestone",
            "track",
            "remove",
            &ms,
            &format!("{other_slug}::{foreign_s}"),
        ],
    );
    let cleaned = report(&repo, &ms);
    assert_eq!(cleaned["tracks"].as_array().unwrap().len(), 1);

    // 2. A declared wave literally named `unassigned` stays distinct from items
    // with no wave assignment (`wave: null`).
    let un_s = start_s(
        &repo.root,
        "unassigned-collision",
        &["--item", "Truly unwaved"],
    );
    let un_ms = start_ms(
        "unassigned-ms",
        &["--title", "Unassigned wave test", "--wave", "unassigned"],
    );
    arc(&repo, &["milestone", "track", "add", &un_ms, &un_s]);
    let un_rep = report(&repo, &un_ms);
    // Declared wave `unassigned` must have 0 items, NOT count the unwaved item!
    assert_eq!(un_rep["waves"][0]["id"], "unassigned");
    assert_eq!(un_rep["waves"][0]["critical"]["items"], 0);
    // Track wave cells keep declared `"unassigned"` (0 items) separate from `null` (1 item).
    let cells = un_rep["tracks"][0]["waves"].as_array().unwrap();
    let declared_cell = cells
        .iter()
        .find(|c| c["wave"] == "unassigned")
        .expect("declared unassigned cell");
    assert_eq!(declared_cell["counts"]["items"], 0);
    assert_eq!(declared_cell["declared"], true);
    let unwaved_cell = cells
        .iter()
        .find(|c| c["wave"].is_null())
        .expect("unwaved null cell");
    assert_eq!(unwaved_cell["counts"]["items"], 1);
    assert_eq!(unwaved_cell["declared"], false);

    // 3. `milestone close --by <conclusion>` refuses to traverse a symlinked
    // `<journal>-archive` directory.
    #[cfg(unix)]
    {
        let dir = jdir(&repo.root);
        let outside = tempfile::tempdir().unwrap();
        let fake_conclusion = "20260101T000000Z-escaped-conclusion.md";
        fs::write(outside.path().join(fake_conclusion), "# Outside secret\n").unwrap();
        let mut cold = dir.as_os_str().to_os_string();
        cold.push("-archive");
        let cold_path = std::path::PathBuf::from(cold);
        let _ = fs::remove_dir_all(&cold_path);
        std::os::unix::fs::symlink(outside.path(), &cold_path).unwrap();

        repo.arc(&repo.root)
            .args(["milestone", "close", &un_ms, "--by", fake_conclusion])
            .assert()
            .failure()
            .stderr(predicates::str::contains("symlink"));
    }
}
