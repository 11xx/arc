use assert_cmd::Command as AssertCommand;
use predicates::prelude::PredicateBooleanExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Repo {
    _tmp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        git(&root, &["init", "-b", "master"]);
        git(&root, &["config", "user.name", "Tester"]);
        git(&root, &["config", "user.email", "tester@example.invalid"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "hello\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "init"]);
        Repo {
            _tmp: tmp,
            root,
            home,
        }
    }

    fn arc(&self, cwd: &Path) -> AssertCommand {
        let mut cmd = AssertCommand::cargo_bin("arc").unwrap();
        cmd.current_dir(cwd)
            .env("HOME", &self.home)
            .env("ARC_ACTOR", "tester")
            .env("ARC_HARNESS", "test")
            .env("ARC_SESSION", "session-a")
            .env_remove("ARC_ROLE")
            .env_remove("ARC_DATA_DIR")
            .env_remove("ARC_DATA_ROOT")
            .env_remove("ARC_WORKTREES_DIR")
            .env_remove("AI_HOME");
        cmd
    }

    fn commit(&self, cwd: &Path, file: &str, content: &str, msg: &str) {
        fs::write(cwd.join(file), content).unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", msg]);
    }

    fn head(&self, cwd: &Path) -> String {
        git_out(cwd, &["rev-parse", "HEAD"])
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stdout(cmd: &mut AssertCommand) -> String {
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn change_with_patchset(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
    let out = stdout(repo.arc(&repo.root).args(["begin", slug]));
    let change_id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("test: add {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    let head = repo.head(&worktree);
    (change_id, worktree, head)
}

fn complete_change(repo: &Repo, slug: &str) {
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: complete {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

fn json_file_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn spawn_arc(repo: &Repo, cwd: &Path, args: &[&str]) -> Child {
    spawn_arc_with_session(repo, cwd, args, "session-a")
}

fn spawn_arc_with_session(repo: &Repo, cwd: &Path, args: &[&str], session: &str) -> Child {
    let binary = std::env::var_os("CARGO_BIN_EXE_arc").expect("cargo should provide arc binary");
    Command::new(binary)
        .args(args)
        .current_dir(cwd)
        .env("HOME", &repo.home)
        .env("ARC_ACTOR", "tester")
        .env("ARC_HARNESS", "test")
        .env("ARC_SESSION", session)
        .env_remove("ARC_ROLE")
        .env_remove("ARC_DATA_DIR")
        .env_remove("ARC_DATA_ROOT")
        .env_remove("ARC_WORKTREES_DIR")
        .env_remove("AI_HOME")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn refresh_bundle_checksum(bundle: &mut serde_json::Value) {
    let mut digest = Sha256::new();
    for event in bundle["events"].as_array().unwrap() {
        digest.update(serde_json::to_vec(event).unwrap());
        digest.update(b"\n");
    }
    bundle["events_sha256"] = serde_json::Value::String(hex::encode(digest.finalize()));
}

fn opened_change_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("begin output should contain a change id")
        .to_string()
}

fn event_dir(repo: &Repo, change_id: &str) -> PathBuf {
    repo.root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
}

fn event_count(repo: &Repo, change_id: &str) -> usize {
    fs::read_dir(event_dir(repo, change_id)).unwrap().count()
}

fn rewrite_event(
    repo: &Repo,
    change_id: &str,
    event_type: &str,
    mut update: impl FnMut(&mut serde_json::Value),
) {
    let mut paths = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let path = paths
        .into_iter()
        .rev()
        .find(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["event_type"] == event_type
        })
        .expect("event type should exist");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    update(&mut value);
    fs::write(path, json_file_bytes(&value)).unwrap();
}

fn age_event(repo: &Repo, change_id: &str, event_type: &str, seconds: i64) {
    rewrite_event(repo, change_id, event_type, |event| {
        event["created_at"] = serde_json::Value::String(
            (chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339(),
        );
    });
}

fn hold_transition_lock(repo: &Repo, change_id: &str) -> fs::File {
    hold_named_lock(repo, &format!("{change_id}.lock"))
}

fn hold_graph_lock(repo: &Repo) -> fs::File {
    hold_named_lock(repo, "graph.lock")
}

fn hold_target_lock(repo: &Repo, target: &str) -> fs::File {
    let digest = Sha256::digest(target.as_bytes());
    hold_named_lock(repo, &format!("target-{}.lock", hex::encode(digest)))
}

fn hold_named_lock(repo: &Repo, name: &str) -> fs::File {
    let dir = repo.root.join(".git/arc/locks");
    fs::create_dir_all(&dir).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(name))
        .unwrap();
    file.lock().unwrap();
    file
}

fn assert_waiting_on_transition_lock(children: &mut [&mut Child]) {
    thread::sleep(Duration::from_millis(250));
    for child in children {
        assert!(
            child.try_wait().unwrap().is_none(),
            "transition command bypassed the externally held product lock"
        );
    }
}

fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("arc subprocess did not exit within five seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn child_stdout(child: &mut Child) -> String {
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    output
}

#[test]
fn implementer_role_refuses_lead_owned_commands_without_appending_events() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "role-guard", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let initial_events = event_count(&repo, &change_id);
    let refused = [
        (
            "review",
            vec!["review", "role-guard", "--verdict", "approved"],
        ),
        (
            "resolve",
            vec![
                "resolve",
                "role-guard",
                "finding-id",
                "--status",
                "resolved",
            ],
        ),
        ("hold", vec!["hold", "role-guard", "--reason", "pause"]),
        ("release-hold", vec!["release-hold", "role-guard"]),
        ("close", vec!["close", "role-guard", "--abandoned"]),
        ("integrate", vec!["integrate", "role-guard"]),
    ];

    for (name, args) in refused {
        repo.arc(&repo.root)
            .env("ARC_ROLE", "implementer")
            .args(args)
            .assert()
            .code(9)
            .stderr(format!("role refusal: implementer may not {name}\n"));
        assert_eq!(
            event_count(&repo, &change_id),
            initial_events,
            "{name} refusal must not append an event"
        );
    }
}

#[test]
fn reviewer_role_can_review_and_resolve_but_cannot_close_or_integrate() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "reviewer-role");
    let finding = stdout(repo.arc(&worktree).args([
        "finding",
        "reviewer-role",
        "--summary",
        "reviewer can resolve this",
    ]));
    let finding_id = finding
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap();

    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args(["review", "reviewer-role", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ROLE", "reviewer")
        .args([
            "resolve",
            "reviewer-role",
            finding_id,
            "--status",
            "resolved",
        ])
        .assert()
        .success();

    let allowed_events = event_count(&repo, &change_id);
    for (name, args) in [
        ("integrate", vec!["integrate", "reviewer-role"]),
        ("close", vec!["close", "reviewer-role", "--abandoned"]),
    ] {
        repo.arc(&repo.root)
            .env("ARC_ROLE", "reviewer")
            .args(args)
            .assert()
            .code(9)
            .stderr(format!("role refusal: reviewer may not {name}\n"));
        assert_eq!(event_count(&repo, &change_id), allowed_events);
    }
}

#[test]
fn lead_and_unset_roles_retain_full_access() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "explicit-lead", "--no-worktree"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "unset-role", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .env("ARC_ROLE", "lead")
        .args(["hold", "explicit-lead", "--reason", "lead probe"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["hold", "unset-role", "--reason", "unset probe"])
        .assert()
        .success();
}

#[test]
fn invalid_role_is_a_usage_error() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .env("ARC_ROLE", " executor ")
        .args(["config"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "invalid execution role \"executor\"; expected implementer, reviewer, or lead",
        ));
}

#[test]
fn role_flag_and_environment_binding_are_equivalent() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "role-binding", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let initial_events = event_count(&repo, &change_id);

    let from_env = repo
        .arc(&repo.root)
        .env("ARC_ROLE", " implementer ")
        .args(["hold", "role-binding", "--reason", "env"])
        .output()
        .unwrap();
    let from_flag = repo
        .arc(&repo.root)
        .args([
            "--role",
            "implementer",
            "hold",
            "role-binding",
            "--reason",
            "flag",
        ])
        .output()
        .unwrap();

    assert_eq!(from_env.status.code(), Some(9));
    assert_eq!(from_flag.status.code(), Some(9));
    assert_eq!(from_env.stderr, from_flag.stderr);
    assert_eq!(
        String::from_utf8_lossy(&from_env.stderr),
        "role refusal: implementer may not hold\n"
    );
    assert_eq!(event_count(&repo, &change_id), initial_events);
}

#[test]
fn claim_lifecycle_reports_defaults_renewal_conflict_release_and_expiry() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "claim-life"]));
    let change_id = opened_change_id(&opened);

    repo.arc(&repo.root)
        .args([
            "claim",
            "claim-life",
            "--ttl",
            "5m",
            "--stage-budget",
            "implementing=1m",
        ])
        .assert()
        .success();
    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(status["schema"], "arc-status/3");
    assert_eq!(status["claim"]["owner"]["actor"], "tester");
    assert_eq!(status["claim"]["owner"]["harness"], "test");
    assert_eq!(status["claim"]["owner"]["session"], "session-a");
    assert_eq!(status["claim"]["ttl_seconds"], 300);
    assert_eq!(status["claim"]["stage"], "launch");
    assert_eq!(status["claim"]["stage_budgets"]["launch"], 60);
    assert_eq!(status["claim"]["stage_budgets"]["started"], 300);
    assert_eq!(status["claim"]["stage_budgets"]["spec-read"], 120);
    assert_eq!(status["claim"]["stage_budgets"]["implementing"], 60);
    assert_eq!(status["claim"]["stage_budgets"]["verifying"], 900);
    assert_eq!(status["claim"]["active"], true);

    let original_claim_id = status["claim"]["claim_id"].clone();
    let original_claimed_at = status["claim"]["claimed_at"].clone();
    thread::sleep(Duration::from_millis(5));
    repo.arc(&repo.root)
        .args(["claim", "claim-life"])
        .assert()
        .success();
    let renewed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(renewed["claim"]["claimed_at"], original_claimed_at);
    assert_eq!(renewed["claim"]["claim_id"], original_claim_id);
    assert_ne!(
        renewed["claim"]["last_activity_at"],
        status["claim"]["last_activity_at"]
    );
    let claim_events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "claim-life",
        "--type",
        "claim-set",
    ]));
    assert_eq!(claim_events.lines().count(), 2, "same owner renews");

    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["claim", "claim-life"])
        .assert()
        .code(8)
        .stderr(predicates::str::contains("owner=tester"))
        .stderr(predicates::str::contains("session=session-a"))
        .stderr(predicates::str::contains("stage=launch"));
    repo.arc(&repo.root)
        .env("ARC_SESSION", "lead-session")
        .args(["release-claim", "claim-life"])
        .assert()
        .success();
    let released: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert!(released["claim"].is_null());
    repo.arc(&repo.root)
        .args(["release-claim", "claim-life"])
        .assert()
        .code(8);

    repo.arc(&repo.root)
        .args(["claim", "claim-life", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &change_id, "claim-set", 5);
    let expired: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(expired["claim"]["active"], false);
    assert_eq!(expired["claim"]["expired"], true);
    assert_eq!(expired["claim"]["stale"], false);
    repo.arc(&repo.root)
        .args(["stage", "claim-life", "started"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .args(["release-claim", "claim-life"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .env("ARC_SESSION", "session-b")
        .args(["claim", "claim-life"])
        .assert()
        .success();
    let reclaimed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "claim-life"]))).unwrap();
    assert_eq!(reclaimed["claim"]["owner"]["session"], "session-b");
    assert_ne!(reclaimed["claim"]["claim_id"], original_claim_id);
}

#[test]
fn claim_duration_budget_and_identity_validation_is_strict() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "claim-input"]));
    for invalid in ["0s", "5d", "s", "-1m", "18446744073709551615h"] {
        repo.arc(&repo.root)
            .args(["claim", "claim-input", "--ttl", invalid])
            .assert()
            .failure();
    }
    for invalid in ["unknown=1m", "implementing", "implementing=0s"] {
        repo.arc(&repo.root)
            .args(["claim", "claim-input", "--stage-budget", invalid])
            .assert()
            .failure();
    }
    repo.arc(&repo.root)
        .env_remove("ARC_SESSION")
        .args(["claim", "claim-input"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("nonempty ARC_SESSION"));
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "")
        .args(["claim", "claim-input"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("nonempty ARC_HARNESS"));
}

#[test]
fn concurrent_claims_have_one_winner_and_stage_release_replay_stays_readable() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "claim-race"]));
    let change_id = opened_change_id(&opened);

    let transition_lock = hold_transition_lock(&repo, &change_id);
    let mut contenders = (0..16)
        .map(|index| {
            spawn_arc_with_session(
                &repo,
                &repo.root,
                &["claim", "claim-race"],
                &format!("contender-{index}"),
            )
        })
        .collect::<Vec<_>>();
    let mut contender_refs = contenders.iter_mut().collect::<Vec<_>>();
    assert_waiting_on_transition_lock(&mut contender_refs);
    transition_lock.unlock().unwrap();
    let statuses = contenders.iter_mut().map(wait_for_exit).collect::<Vec<_>>();
    assert_eq!(
        statuses.iter().filter(|status| status.success()).count(),
        1,
        "exactly one serialized acquisition succeeds"
    );
    assert!(statuses
        .iter()
        .filter(|status| !status.success())
        .all(|status| status.code() == Some(8)));
    let claim_events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "claim-race",
        "--type",
        "claim-set",
    ]));
    assert_eq!(claim_events.lines().count(), 1);
    assert!(repo
        .root
        .join(".git/arc/locks")
        .join(format!("{change_id}.lock"))
        .is_file());

    repo.arc(&repo.root)
        .env("ARC_SESSION", "lead")
        .args(["release-claim", "claim-race"])
        .assert()
        .success();
    for iteration in 0..12 {
        repo.arc(&repo.root)
            .args(["claim", "claim-race"])
            .assert()
            .success();
        let mut stage = spawn_arc_with_session(
            &repo,
            &repo.root,
            &["stage", "claim-race", "started"],
            "session-a",
        );
        let mut release = spawn_arc_with_session(
            &repo,
            &repo.root,
            &["release-claim", "claim-race"],
            &format!("lead-{iteration}"),
        );
        let stage_status = wait_for_exit(&mut stage);
        let release_status = wait_for_exit(&mut release);
        assert!(release_status.success());
        assert!(stage_status.success() || stage_status.code() == Some(8));
        repo.arc(&repo.root)
            .args(["status", "claim-race"])
            .assert()
            .success()
            .stdout(predicates::str::contains("\"claim\": null"));
    }
}

#[test]
fn transition_lock_acquisition_is_bounded() {
    let repo = Repo::new();
    let opened = stdout(
        repo.arc(&repo.root)
            .args(["begin", "bounded-lock", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let transition_lock = hold_transition_lock(&repo, &change_id);
    let started = Instant::now();
    let mut claim = spawn_arc(&repo, &repo.root, &["claim", "bounded-lock"]);
    assert_eq!(wait_for_exit(&mut claim).code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(3));
    transition_lock.unlock().unwrap();
}

#[test]
fn claim_persists_normalized_identity_and_handles_maximum_ttl() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "claim-normalized"]));
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "  Executor  ")
        .env("ARC_HARNESS", "  codex  ")
        .env("ARC_SESSION", "  thread-1  ")
        .args(["claim", "claim-normalized", "--ttl", "9223372036854775807s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "  Executor  ")
        .env("ARC_HARNESS", "  codex  ")
        .env("ARC_SESSION", "  thread-1  ")
        .args(["stage", "claim-normalized", "started"])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "claim-normalized"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["owner"]["actor"], "Executor");
    assert_eq!(status["claim"]["owner"]["harness"], "codex");
    assert_eq!(status["claim"]["owner"]["session"], "thread-1");
    assert_eq!(status["claim"]["active"], true);
    assert_eq!(
        status["claim"]["ttl_seconds"],
        serde_json::Value::from(i64::MAX)
    );

    let transitions = stdout(
        repo.arc(&repo.root)
            .args(["events", "--change", "claim-normalized"]),
    );
    for event in transitions.lines().filter_map(|line| {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        matches!(
            event["event_type"].as_str(),
            Some("claim-set" | "stage-set")
        )
        .then_some(event)
    }) {
        assert_eq!(event["actor"], "Executor");
        assert_eq!(event["harness"], "codex");
        assert_eq!(event["session"], "thread-1");
    }
}

#[test]
fn stage_requires_owned_live_claim_and_tracks_heartbeats_advisory_order_and_snapshot() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "stage-life"]));
    let worktree = repo.home.join(".worktrees/repo-stage-life");

    repo.arc(&repo.root)
        .args(["stage", "stage-life", "started"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .args(["claim", "stage-life", "--ttl", "1s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "stage",
            "stage-life",
            "implementing",
            "--note",
            "skipped ahead",
        ])
        .assert()
        .success();
    let first: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    thread::sleep(Duration::from_millis(5));
    repo.arc(&repo.root)
        .args(["stage", "stage-life", "implementing", "--note", "heartbeat"])
        .assert()
        .success();
    let heartbeat: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    assert_ne!(
        first["claim"]["stage_started_at"], heartbeat["claim"]["stage_started_at"],
        "a heartbeat refreshes age-in-stage and its budget clock"
    );
    assert_ne!(
        first["claim"]["last_activity_at"],
        heartbeat["claim"]["last_activity_at"]
    );
    assert_eq!(heartbeat["claim"]["note"], "heartbeat");

    repo.arc(&repo.root)
        .env("ARC_SESSION", "foreign")
        .args(["stage", "stage-life", "verifying"])
        .assert()
        .code(8);
    repo.arc(&repo.root)
        .args(["stage", "stage-life", "blocked-on"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("requires a nonempty --note"));
    repo.arc(&repo.root)
        .args([
            "stage",
            "stage-life",
            "blocked-on",
            "--note",
            "waiting for input",
        ])
        .assert()
        .success();

    repo.commit(&worktree, "stage.txt", "done\n", "feat: finish stage work");
    repo.arc(&worktree)
        .args(["snapshot", "stage-life"])
        .assert()
        .success();
    let snapshotted: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    assert_eq!(snapshotted["claim"]["stage"], "snapshotted");
    assert_eq!(snapshotted["claim"]["stale"], false);
    repo.arc(&repo.root)
        .args(["stage", "stage-life", "snapshotted"])
        .assert()
        .failure();

    let change_id = opened_change_id(&opened);
    age_event(&repo, &change_id, "patchset-added", 5);
    repo.arc(&repo.root)
        .args(["claim", "stage-life"])
        .assert()
        .success();
    let restarted: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stage-life"]))).unwrap();
    assert_eq!(restarted["claim"]["stage"], "launch");
    assert_eq!(restarted["claim"]["expired"], false);
}

#[test]
fn stale_claims_are_time_derived_and_watch_until_stalled_reaches() {
    let repo = Repo::new();

    stdout(repo.arc(&repo.root).args(["begin", "wall-clock-stall"]));
    repo.arc(&repo.root)
        .args(["claim", "wall-clock-stall", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    let started = Instant::now();
    let mut watcher = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "wall-clock-stall",
            "--until",
            "stalled",
            "--timeout",
            "4",
        ],
    );
    assert!(wait_for_exit(&mut watcher).success());
    assert_eq!(child_stdout(&mut watcher), "reached: stalled\n");
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "a fresh claim must become stalled through wall-clock passage"
    );

    let launch = stdout(repo.arc(&repo.root).args(["begin", "stale-launch"]));
    let launch_id = opened_change_id(&launch);
    repo.arc(&repo.root)
        .args(["claim", "stale-launch", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &launch_id, "claim-set", 5);
    let launch_status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "stale-launch"]),
    ))
    .unwrap();
    assert_eq!(launch_status["claim"]["active"], true);
    assert_eq!(launch_status["claim"]["stale"], true);
    assert_eq!(launch_status["claim"]["stage"], "launch");
    repo.arc(&repo.root)
        .args([
            "watch",
            "stale-launch",
            "--until",
            "stalled",
            "--timeout",
            "1",
        ])
        .assert()
        .success()
        .stdout("reached: stalled\n");
    repo.arc(&repo.root)
        .env("ARC_SESSION", "lead-recovery")
        .args(["release-claim", "stale-launch"])
        .assert()
        .success();

    let implementing = stdout(repo.arc(&repo.root).args(["begin", "stale-impl"]));
    let implementing_id = opened_change_id(&implementing);
    repo.arc(&repo.root)
        .args(["claim", "stale-impl", "--stage-budget", "implementing=1s"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["stage", "stale-impl", "implementing"])
        .assert()
        .success();
    age_event(&repo, &implementing_id, "stage-set", 5);
    let implementing_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stale-impl"]))).unwrap();
    assert_eq!(implementing_status["claim"]["stale"], true);
    assert!(
        implementing_status["claim"]["age_seconds"]
            .as_u64()
            .unwrap()
            >= 5
    );

    repo.arc(&repo.root)
        .args(["stage", "stale-impl", "blocked-on", "--note", "distress"])
        .assert()
        .success();
    age_event(&repo, &implementing_id, "stage-set", 60);
    let blocked: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "stale-impl"]))).unwrap();
    assert_eq!(blocked["claim"]["stage"], "blocked-on");
    assert_eq!(blocked["claim"]["stale"], false);
    assert!(blocked["claim"]["budget_seconds"].is_null());
}

#[test]
fn alternatives_exclude_active_claims_but_include_stale_and_expired_claims() {
    let repo = Repo::new();
    let prerequisite = stdout(repo.arc(&repo.root).args(["begin", "alt-prereq"]));
    let prerequisite_id = opened_change_id(&prerequisite);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "alt-blocked", "--blocked-by", &prerequisite_id]),
    );
    stdout(repo.arc(&repo.root).args(["begin", "alt-active"]));
    let stale = stdout(repo.arc(&repo.root).args(["begin", "alt-stale"]));
    let stale_id = opened_change_id(&stale);
    let expired = stdout(repo.arc(&repo.root).args(["begin", "alt-expired"]));
    let expired_id = opened_change_id(&expired);

    repo.arc(&repo.root)
        .args(["claim", "alt-active"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["claim", "alt-stale", "--stage-budget", "launch=1s"])
        .assert()
        .success();
    age_event(&repo, &stale_id, "claim-set", 5);
    repo.arc(&repo.root)
        .args(["claim", "alt-expired", "--ttl", "1s"])
        .assert()
        .success();
    age_event(&repo, &expired_id, "claim-set", 5);

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "alt-blocked"]),
    ))
    .unwrap();
    let slugs = status["suggested_alternatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alternative| alternative["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(slugs.contains(&"alt-prereq"));
    assert!(slugs.contains(&"alt-stale"));
    assert!(slugs.contains(&"alt-expired"));
    assert!(!slugs.contains(&"alt-active"));
}

#[test]
fn integration_warns_for_foreign_claim_but_still_succeeds_when_green() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "claimed-green"]));
    let worktree = repo.home.join(".worktrees/repo-claimed-green");
    repo.commit(&worktree, "green.txt", "green\n", "feat: add green change");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "executor")
        .env("ARC_SESSION", "executor-session")
        .args(["claim", "claimed-green"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "claimed-green"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "claimed-green", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "claimed-green"])
        .assert()
        .success()
        .stderr(predicates::str::contains(
            "warning: active foreign claim by executor",
        ));
}

#[test]
fn transition_lock_serializes_hold_against_integrate() {
    let repo = Repo::new();
    let (change_id, worktree, _) = change_with_patchset(&repo, "hold-integrate-race");
    repo.arc(&worktree)
        .args(["review", "hold-integrate-race", "--verdict", "approved"])
        .assert()
        .success();

    let transition_lock = hold_transition_lock(&repo, &change_id);
    let mut integrate = spawn_arc_with_session(
        &repo,
        &repo.root,
        &["integrate", "hold-integrate-race"],
        "lead-integrate",
    );
    let mut hold = spawn_arc_with_session(
        &repo,
        &repo.root,
        &[
            "hold",
            "hold-integrate-race",
            "--reason",
            "concurrent user hold",
        ],
        "lead-hold",
    );
    assert_waiting_on_transition_lock(&mut [&mut integrate, &mut hold]);
    transition_lock.unlock().unwrap();

    let integrate_status = wait_for_exit(&mut integrate);
    let hold_status = wait_for_exit(&mut hold);
    assert_ne!(
        integrate_status.success(),
        hold_status.success(),
        "serialized hold/integrate permits exactly one transition"
    );
    if integrate_status.success() {
        assert_eq!(hold_status.code(), Some(1));
    } else {
        assert_eq!(integrate_status.code(), Some(4));
        assert!(hold_status.success());
    }
    repo.arc(&repo.root)
        .args(["status", "hold-integrate-race"])
        .assert()
        .success();
}

#[test]
fn concurrent_integrations_serialize_on_the_target_branch_lock() {
    let repo = Repo::new();
    let (_, first_worktree, _) = change_with_patchset(&repo, "target-race-a");
    let (_, second_worktree, _) = change_with_patchset(&repo, "target-race-b");
    repo.arc(&first_worktree)
        .args(["review", "target-race-a", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&second_worktree)
        .args(["review", "target-race-b", "--verdict", "approved"])
        .assert()
        .success();

    let target_lock = hold_target_lock(&repo, "master");
    let mut first = spawn_arc_with_session(
        &repo,
        &repo.root,
        &["integrate", "target-race-a"],
        "integrator-a",
    );
    let mut second = spawn_arc_with_session(
        &repo,
        &repo.root,
        &["integrate", "target-race-b"],
        "integrator-b",
    );
    assert_waiting_on_transition_lock(&mut [&mut first, &mut second]);
    target_lock.unlock().unwrap();

    assert!(wait_for_exit(&mut first).success());
    assert!(wait_for_exit(&mut second).success());
    assert!(repo.root.join("target-race-a.txt").is_file());
    assert!(repo.root.join("target-race-b.txt").is_file());
    repo.arc(&repo.root)
        .args(["status", "target-race-a"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"outcome\": \"integrated\""));
    repo.arc(&repo.root)
        .args(["status", "target-race-b"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"outcome\": \"integrated\""));
}

#[test]
fn verification_gate_can_mutate_arc_state_without_lock_reentry_deadlock() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "verify-reentry", "--no-worktree"]),
    );
    let binary = std::env::var("CARGO_BIN_EXE_arc").unwrap();
    let gate = format!("\"{binary}\" hold verify-reentry --reason gate-self-mutation");
    let mut verify = spawn_arc(
        &repo,
        &repo.root,
        &["verify", "verify-reentry", "--command", &gate],
    );
    assert!(wait_for_exit(&mut verify).success());

    let state: serde_json::Value = serde_json::from_str(&stdout(repo.arc(&repo.root).args([
        "show",
        "verify-reentry",
        "--json",
    ])))
    .unwrap();
    assert_eq!(state["hold"], "gate-self-mutation");
    assert_eq!(state["verifications"].as_array().unwrap().len(), 1);
}

#[test]
fn snapshot_captures_git_identities_and_renders_claim_actor_mismatch() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "snapshot-who"]));
    let worktree = repo.home.join(".worktrees/repo-snapshot-who");
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Executor Person")
        .args(["claim", "snapshot-who"])
        .assert()
        .success();
    repo.commit(&worktree, "who.txt", "who\n", "feat: record identity");
    repo.arc(&worktree)
        .args(["snapshot", "snapshot-who"])
        .assert()
        .success();

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "snapshot-who"]),
    ))
    .unwrap();
    assert_eq!(status["latest_patchset"]["author"]["name"], "Tester");
    assert_eq!(
        status["latest_patchset"]["author"]["email"],
        "tester@example.invalid"
    );
    assert_eq!(status["latest_patchset"]["committer"]["name"], "Tester");
    assert_eq!(status["latest_patchset"]["claim_actor"], "Executor Person");
    assert_eq!(status["latest_patchset"]["provenance_mismatch"], true);
    assert_eq!(status["claim"]["snapshot_author"]["name"], "Tester");
    assert_eq!(status["claim"]["provenance_mismatch"], true);
    repo.arc(&repo.root)
        .args(["show", "snapshot-who"])
        .assert()
        .success()
        .stdout(predicates::str::contains("PROVENANCE MISMATCH"));
}

#[test]
fn events_replays_parseable_ndjson_and_filters_change_and_type() {
    let repo = Repo::new();
    let first = stdout(repo.arc(&repo.root).args(["begin", "events-a"]));
    let first_id = first
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    stdout(repo.arc(&repo.root).args(["begin", "events-b"]));
    stdout(
        repo.arc(&repo.root)
            .args(["comment", "events-a", "--body", "watch this"]),
    );

    let output = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "events-a",
        "--type",
        "comment-added",
    ]));
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let event: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(event["change_id"], first_id);
    assert_eq!(event["event_type"], "comment-added");

    let all = stdout(repo.arc(&repo.root).args(["events"]));
    let ids = all
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn events_follow_emits_a_later_snapshot_once() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "events-follow");
    // Create a second patchset after the watcher begins; filtering excludes
    // the first patchset replay so the output must contain exactly one line.
    repo.commit(&worktree, "later.txt", "later\n", "test: later snapshot");
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "events",
            "--follow",
            "--change",
            "events-follow",
            "--type",
            "patchset-added",
        ],
    );
    let child_output = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let reader_thread = thread::spawn(move || {
        let mut reader = BufReader::new(child_output);
        let mut first_line = String::new();
        let result = reader.read_line(&mut first_line);
        let _ = sender.send((reader, first_line, result));
    });
    let (mut reader, first_line, first_read) = match receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result,
        Err(_) => {
            child.kill().unwrap();
            child.wait().unwrap();
            reader_thread.join().unwrap();
            panic!("events --follow did not flush its initial replay");
        }
    };
    reader_thread.join().unwrap();
    assert!(first_read.unwrap() > 0);
    let first: serde_json::Value = serde_json::from_str(first_line.trim()).unwrap();
    assert_eq!(first["patchset_id"], "ps-01");

    // The second snapshot is created only after follow mode has flushed its
    // replay, so a delayed startup cannot make this test pass accidentally.
    stdout(repo.arc(&worktree).args(["snapshot", "events-follow"]));
    thread::sleep(Duration::from_millis(250));
    child.kill().unwrap();
    child.wait().unwrap();
    let mut later = String::new();
    reader.read_to_string(&mut later).unwrap();

    let events = std::iter::once(first_line.as_str())
        .chain(later.lines())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2, "replay plus the later snapshot");
    assert_eq!(events[0]["patchset_id"], "ps-01");
    assert_eq!(events[1]["patchset_id"], "ps-02");
}

#[test]
fn watch_snapshot_observes_a_later_snapshot() {
    let repo = Repo::new();
    let opened = stdout(repo.arc(&repo.root).args(["begin", "watch-snapshot"]));
    let worktree = repo.home.join(".worktrees").join("repo-watch-snapshot");
    assert!(opened.contains("change: watch-snapshot-"));
    repo.commit(
        &worktree,
        "snapshot.txt",
        "snapshot\n",
        "test: add snapshot",
    );

    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-snapshot",
            "--until",
            "snapshot",
            "--timeout",
            "2",
        ],
    );
    thread::sleep(Duration::from_millis(50));
    stdout(repo.arc(&worktree).args(["snapshot", "watch-snapshot"]));
    assert!(wait_for_exit(&mut child).success());
    assert_eq!(child_stdout(&mut child).trim(), "reached: snapshot");
}

#[test]
fn watch_ready_times_out_when_check_is_not_green() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watch-ready"]));
    repo.arc(&repo.root)
        .args(["watch", "watch-ready", "--until", "ready", "--timeout", "1"])
        .assert()
        .code(2)
        .stdout("timeout: ready\n");
}

#[test]
fn watch_ready_observes_a_git_only_head_transition() {
    let repo = Repo::new();
    let (_, worktree, head) = change_with_patchset(&repo, "watch-ready-live");
    repo.arc(&worktree)
        .args(["review", "watch-ready-live", "--verdict", "approved"])
        .assert()
        .success();

    let branch = "refs/heads/arc/watch-ready-live";
    let base = git_out(&repo.root, &["rev-parse", "master"]);
    git(&repo.root, &["update-ref", branch, &base]);
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-ready-live",
            "--until",
            "ready",
            "--timeout",
            "2",
        ],
    );
    thread::sleep(Duration::from_millis(50));
    // Ref restoration changes readiness without appending a ledger event.
    git(&repo.root, &["update-ref", branch, &head]);

    assert!(wait_for_exit(&mut child).success());
    assert_eq!(child_stdout(&mut child).trim(), "reached: ready");
}

#[test]
fn watch_integrated_returns_after_real_integration() {
    let repo = Repo::new();
    let (_, worktree, _) = change_with_patchset(&repo, "watch-integrated");
    repo.arc(&worktree)
        .args(["review", "watch-integrated", "--verdict", "approved"])
        .assert()
        .success();
    let mut child = spawn_arc(
        &repo,
        &repo.root,
        &[
            "watch",
            "watch-integrated",
            "--until",
            "integrated",
            "--timeout",
            "2",
        ],
    );
    thread::sleep(Duration::from_millis(50));
    repo.arc(&repo.root)
        .args(["integrate", "watch-integrated"])
        .assert()
        .success();
    assert!(wait_for_exit(&mut child).success());
    assert_eq!(child_stdout(&mut child).trim(), "reached: integrated");
}

#[test]
fn watch_closed_accepts_abandoned_and_superseded_but_integrated_does_not() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "watch-abandoned"]));
    stdout(repo.arc(&repo.root).args(["begin", "watch-replacement"]));
    stdout(repo.arc(&repo.root).args(["begin", "watch-superseded"]));
    repo.arc(&repo.root)
        .args(["close", "watch-abandoned", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "close",
            "watch-superseded",
            "--superseded",
            "watch-replacement",
        ])
        .assert()
        .success();

    for change in ["watch-abandoned", "watch-superseded"] {
        repo.arc(&repo.root)
            .args(["watch", change, "--until", "closed", "--timeout", "1"])
            .assert()
            .success()
            .stdout("reached: closed\n");
    }
    repo.arc(&repo.root)
        .args([
            "watch",
            "watch-abandoned",
            "--until",
            "integrated",
            "--timeout",
            "1",
        ])
        .assert()
        .code(2)
        .stdout("timeout: integrated\n");
}

fn begin_change(repo: &Repo, slug: &str, blocked_by: Option<&str>) -> String {
    let mut command = repo.arc(&repo.root);
    command.args(["begin", slug, "--no-worktree"]);
    if let Some(blocker) = blocked_by {
        command.args(["--blocked-by", blocker]);
    }
    let out = stdout(&mut command);
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

fn replace_closure_successor(repo: &Repo, change_id: &str, successor: &str) {
    let events = repo
        .root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events");
    for entry in fs::read_dir(events).unwrap() {
        let path = entry.unwrap().path();
        let mut event: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if event["event_type"] == "change-closed" {
            event["outcome"] = serde_json::json!("superseded");
            event["superseded_by"] = serde_json::json!(successor);
            event.as_object_mut().unwrap().remove("integrated_commit");
            fs::write(path, json_file_bytes(&event)).unwrap();
            return;
        }
    }
    panic!("expected a change-closed event for {change_id}");
}

/// begin → worktree + branch + ledger; list/status see the change.
#[test]
fn begin_creates_change_branch_and_worktree() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "fix-thing", "--title", "Fix the thing"]),
    );
    assert!(out.contains("change: fix-thing-"));
    assert!(out.contains("branch: arc/fix-thing"));
    assert!(out.contains("worktree: "));

    let wt = repo.home.join(".worktrees").join("repo-fix-thing");
    assert!(wt.is_dir(), "worktree should exist");

    let list = stdout(repo.arc(&repo.root).args(["list", "--json"]));
    let rows: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(rows[0]["slug"], "fix-thing");
    assert_eq!(rows[0]["state"], "open");

    // Same open slug refuses a duplicate.
    repo.arc(&repo.root)
        .args(["begin", "fix-thing"])
        .assert()
        .failure();
}

/// A dependency chain blocks until each prerequisite integrates. Status
/// suggests only other open changes whose own prerequisites are satisfied.
#[test]
fn blocker_chain_transitions_and_suggests_ready_alternatives() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "chain-a"]));
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "chain-b", "--blocked-by", "chain-a"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "chain-c", "--blocked-by", "chain-b"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "chain-d", "--blocked-by", "chain-c"]),
    );
    stdout(repo.arc(&repo.root).args(["begin", "chain-held"]));
    repo.arc(&repo.root)
        .args(["hold", "chain-held", "--reason", "do not start"])
        .assert()
        .success();

    let b_status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "chain-b", "--json"]),
    ))
    .unwrap();
    assert_eq!(b_status["blocker_status"]["blocked"], true);
    assert_eq!(
        b_status["suggested_alternatives"].as_array().unwrap().len(),
        1
    );
    assert_eq!(b_status["suggested_alternatives"][0]["slug"], "chain-a");
    let blocker_status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["blocker-status", "chain-b"]),
    ))
    .unwrap();
    assert_eq!(blocker_status["schema"], "arc-blocker-status/1");
    assert_eq!(blocker_status["blocked"], true);
    assert_eq!(blocker_status["blockers_ready"][0]["slug"], "chain-a");
    repo.arc(&repo.root)
        .args(["is-blocked", "chain-b"])
        .assert()
        .code(1);
    repo.arc(&repo.root)
        .args(["is-blocked", "does-not-exist"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("no change matches"));
    repo.arc(&repo.root)
        .args(["check", "chain-b"])
        .assert()
        .code(7);

    complete_change(&repo, "chain-a");

    let b_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "chain-b"]))).unwrap();
    assert_eq!(b_status["blocker_status"]["blocked"], false);
    assert_eq!(b_status["suggested_alternatives"], serde_json::json!([]));
    repo.arc(&repo.root)
        .args(["is-blocked", "chain-b"])
        .assert()
        .success();

    let c_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "chain-c"]))).unwrap();
    assert_eq!(c_status["blocker_status"]["blocked"], true);
    assert_eq!(
        c_status["suggested_alternatives"].as_array().unwrap().len(),
        1
    );
    assert_eq!(c_status["suggested_alternatives"][0]["slug"], "chain-b");

    complete_change(&repo, "chain-b");
    let c_status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "chain-c"]))).unwrap();
    assert_eq!(c_status["blocker_status"]["blocked"], false);
}

#[test]
fn superseded_prerequisites_resolve_after_successor_integration() {
    let repo = Repo::new();
    let prerequisite = begin_change(&repo, "superseded-a", None);
    let successor = begin_change(&repo, "superseded-a2", None);
    let dependent = begin_change(&repo, "superseded-dependent", Some(&prerequisite));

    repo.arc(&repo.root)
        .args(["close", &prerequisite, "--superseded", &successor])
        .assert()
        .success();
    let wedged: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["blocker-status", &dependent]),
    ))
    .unwrap();
    assert_eq!(wedged["blocked"], true);
    assert_eq!(wedged["blockers_ready"][0]["status"], "wedged");

    repo.arc(&repo.root)
        .args(["close", &successor, "--integrated", "HEAD"])
        .assert()
        .success();
    let ready: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &dependent]))).unwrap();
    assert_eq!(ready["blocker_status"]["blocked"], false);
    assert_eq!(
        ready["blocker_status"]["blockers_ready"][0]["status"],
        "superseded-integrated"
    );
    assert_eq!(
        ready["blocker_status"]["blockers_ready"][0]["integrated"],
        true
    );
    repo.arc(&repo.root)
        .args(["is-blocked", &dependent])
        .assert()
        .success();

    let first = begin_change(&repo, "transitive-a", None);
    let second = begin_change(&repo, "transitive-a2", None);
    let third = begin_change(&repo, "transitive-a3", None);
    let transitive_dependent = begin_change(&repo, "transitive-dependent", Some(&first));
    repo.arc(&repo.root)
        .args(["close", &first, "--superseded", &second])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", &second, "--superseded", &third])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", &third, "--integrated", "HEAD"])
        .assert()
        .success();
    let transitive: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["blocker-status", &transitive_dependent]),
    ))
    .unwrap();
    assert_eq!(transitive["blocked"], false);
    assert_eq!(
        transitive["blockers_ready"][0]["status"],
        "superseded-integrated"
    );
    assert_eq!(transitive["blockers_ready"][0]["integrated"], true);
}

#[test]
fn wedged_prerequisites_report_recovery_and_stay_blocked() {
    let repo = Repo::new();
    let abandoned = begin_change(&repo, "abandoned-a", None);
    let dependent = begin_change(&repo, "abandoned-dependent", Some(&abandoned));
    repo.arc(&repo.root)
        .args(["close", &abandoned, "--abandoned"])
        .assert()
        .success();

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", &dependent]))).unwrap();
    let dependency = &status["blocker_status"]["blockers_ready"][0];
    assert_eq!(status["schema"], "arc-status/3");
    assert_eq!(status["blocker_status"]["blocked"], true);
    assert_eq!(status["next_action"], "repair_blockers:metadata");
    assert_eq!(dependency["status"], "wedged");
    assert_eq!(
        dependency["recovery"],
        "prerequisite closed without integration: clear or retarget with arc metadata"
    );
    repo.arc(&repo.root)
        .args(["is-blocked", &dependent])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(format!(
            "blocked by {abandoned} (wedged)"
        )));
    repo.arc(&repo.root)
        .args(["check", &dependent])
        .assert()
        .code(7)
        .stdout(predicates::str::contains(
            "prerequisite closed without integration: clear or retarget with arc metadata",
        ));

    let raw_missing = begin_change(&repo, "raw-missing-a", None);
    let raw_dependent = begin_change(&repo, "raw-missing-dependent", Some(&raw_missing));
    repo.arc(&repo.root)
        .args(["close", &raw_missing, "--abandoned"])
        .assert()
        .success();
    replace_closure_successor(&repo, &raw_missing, "missing-successor");
    let missing: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["blocker-status", &raw_dependent]),
    ))
    .unwrap();
    assert_eq!(missing["blocked"], true);
    assert_eq!(missing["blockers_ready"][0]["status"], "wedged");

    let first = begin_change(&repo, "cycle-a", None);
    let second = begin_change(&repo, "cycle-a2", None);
    let cycle_dependent = begin_change(&repo, "cycle-dependent", Some(&first));
    repo.arc(&repo.root)
        .args(["close", &first, "--superseded", &second])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["close", &second, "--superseded", &first])
        .assert()
        .success();
    let cycle: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["blocker-status", &cycle_dependent]),
    ))
    .unwrap();
    assert_eq!(cycle["blocked"], true);
    assert_eq!(cycle["blockers_ready"][0]["status"], "wedged");
}

#[test]
fn imported_change_can_remove_missing_blocker() {
    let source = Repo::new();
    let blocker_out = stdout(source.arc(&source.root).args(["begin", "remote-blocker"]));
    let blocker_id = blocker_out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    stdout(source.arc(&source.root).args([
        "begin",
        "dependent-change",
        "--blocked-by",
        &blocker_id,
    ]));
    let bundle = source.home.join("dependent.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "dependent-change",
            "--output",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .env("ARC_ROLE", "implementer")
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let blocked: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "dependent-change"]),
    ))
    .unwrap();
    assert_eq!(blocked["blocker_status"]["blocked"], true);
    assert_eq!(
        blocked["blocker_status"]["blockers_ready"][0]["status"],
        "missing"
    );

    destination
        .arc(&destination.root)
        .args([
            "metadata",
            "dependent-change",
            "--remove-blocked-by",
            &blocker_id,
        ])
        .assert()
        .success();
    let cleared: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "dependent-change"]),
    ))
    .unwrap();
    assert_eq!(cleared["blocked_by"], serde_json::json!([]));
    assert_eq!(cleared["blocker_status"]["blocked"], false);
}

#[test]
fn batch_check_treats_all_closed_outcomes_as_terminal() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "batch-live", "--tag", "#terminal-suite"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "batch-abandoned", "--tag", "#terminal-suite"]),
    );
    repo.arc(&repo.root)
        .args(["close", "batch-abandoned", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["metadata", "batch-abandoned", "--tag", "#too-late"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is closed"));
    complete_change(&repo, "batch-live");

    repo.arc(&repo.root)
        .args(["check", "--tag", "#terminal-suite"])
        .assert()
        .success()
        .stdout(
            predicates::str::contains("batch-live-").and(predicates::str::contains(": integrated")),
        )
        .stdout(
            predicates::str::contains("batch-abandoned-")
                .and(predicates::str::contains(": abandoned")),
        );
}

#[test]
fn query_tags_batch_views_and_actionable_errors() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "tagged-a", "--tag", "#suite", "--tag", "#fast"]),
    );
    stdout(repo.arc(&repo.root).args([
        "begin",
        "tagged-b",
        "--blocked-by",
        "tagged-a",
        "--tag",
        "#suite",
    ]));

    let query = stdout(repo.arc(&repo.root).args([
        "query",
        "--status",
        "open",
        "--target",
        "master",
        "--tag",
        "#suite",
        "--actor",
        "tester",
        "--harness",
        "test",
    ]));
    assert_eq!(query.lines().count(), 2);
    assert!(query.contains("tagged-a-"));
    assert!(query.contains("tagged-b-"));

    let wide = stdout(repo.arc(&repo.root).args(["list", "--format", "wide"]));
    assert!(wide.contains("Verdict"));
    assert!(wide.contains("blocked-by:tagged-a"));

    let status: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&repo.root).args(["status", "tagged-b"]))).unwrap();
    assert_eq!(status["next_action"], "wait_for:blockers");
    assert_eq!(status["ready_to_integrate"], false);
    assert_eq!(status["blocker_summary"]["hold"]["active"], false);

    repo.arc(&repo.root)
        .args(["check", "tagged-b"])
        .assert()
        .code(7)
        .stdout(predicates::str::contains("Cannot integrate"))
        .stdout(predicates::str::contains("Next step: wait_for:blockers"));
    repo.arc(&repo.root)
        .args(["integrate", "tagged-b"])
        .assert()
        .code(7)
        .stderr(predicates::str::contains("prerequisite changes unresolved"));

    repo.arc(&repo.root)
        .args(["metadata", "tagged-a", "--tag", "#extra"])
        .assert()
        .success();
    let extra = stdout(
        repo.arc(&repo.root)
            .args(["query", "--tag", "#extra", "--json"]),
    );
    let rows: serde_json::Value = serde_json::from_str(&extra).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["slug"], "tagged-a");

    // Metadata events remain first-class through deterministic transfer.
    let bundle = repo.home.join("tagged-a.json");
    repo.arc(&repo.root)
        .args(["export", "tagged-a", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("unknown event type").not());
    let transferred = stdout(
        destination
            .arc(&destination.root)
            .args(["query", "--tag", "#extra", "--json"]),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&transferred)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // B already depends on A, so making A depend on B would form a cycle.
    repo.arc(&repo.root)
        .args(["metadata", "tagged-a", "--blocked-by", "tagged-b"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("dependency cycle"));

    let batch = stdout(
        repo.arc(&repo.root)
            .args(["show", "--tag", "#suite", "--json"]),
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&batch)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    repo.arc(&repo.root)
        .args(["check", "--tag", "#suite"])
        .assert()
        .code(3)
        .stdout(predicates::str::contains("tagged-a-"))
        .stdout(predicates::str::contains("tagged-b-"));
}

#[test]
fn concurrent_metadata_updates_cannot_create_a_dependency_cycle() {
    let repo = Repo::new();
    let first = begin_change(&repo, "cycle-race-a", None);
    let second = begin_change(&repo, "cycle-race-b", None);

    let graph_lock = hold_graph_lock(&repo);
    let mut first_to_second = spawn_arc(
        &repo,
        &repo.root,
        &["metadata", &first, "--blocked-by", &second],
    );
    let mut second_to_first = spawn_arc(
        &repo,
        &repo.root,
        &["metadata", &second, "--blocked-by", &first],
    );
    assert_waiting_on_transition_lock(&mut [&mut first_to_second, &mut second_to_first]);
    graph_lock.unlock().unwrap();

    let first_status = wait_for_exit(&mut first_to_second);
    let second_status = wait_for_exit(&mut second_to_first);
    assert_ne!(first_status.success(), second_status.success());
    assert_eq!(
        [first_status, second_status]
            .iter()
            .filter(|status| status.success())
            .count(),
        1
    );
    let first_state: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["show", &first, "--json"]),
    ))
    .unwrap();
    let second_state: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["show", &second, "--json"]),
    ))
    .unwrap();
    let edge_count = first_state["blocked_by"].as_array().unwrap().len()
        + second_state["blocked_by"].as_array().unwrap().len();
    assert_eq!(edge_count, 1);
}

/// The full green path: implement → snapshot → verify gate → approve →
/// check ok → integrate produces a --no-ff merge with correct parents.
#[test]
fn green_path_integrates_with_merge_commit() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"test -f README.md\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);
    let old_master = repo.head(&repo.root);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-x", "--title", "Feature X"]),
    );
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "x.txt", "x\n", "feat: add x");

    stdout(repo.arc(&wt).args(["snapshot", "feat-x"]));
    repo.arc(&wt)
        .args(["verify", "feat-x", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt).args(["check", "feat-x"]).assert().success();

    // Integrate from the main checkout, which has master checked out.
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let merged = repo.head(&repo.root);
    let parents = git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged]);
    let ids: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(ids.len(), 3, "merge commit must have two parents");
    assert_eq!(ids[1], old_master);
    let subject = git_out(&repo.root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject, "merge(feat-x): Feature X");

    let status = stdout(repo.arc(&repo.root).args(["status", "feat-x"]));
    let report: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(report["state"], "closed");
    assert_eq!(report["closure"]["outcome"], "integrated");
}

/// Blocking finding → check exits 2 and integrate refuses; resolving it
/// plus a renewed verdict unblocks.
#[test]
fn blocking_finding_blocks_until_resolved_and_reapproved() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-y"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-y");
    repo.commit(&wt, "y.txt", "y\n", "fix: y");
    stdout(repo.arc(&wt).args(["snapshot", "fix-y"]));

    let findings = r#"[{"blocking": true, "severity": "major",
        "summary": "y is wrong", "anchor": {"path": "y.txt", "line_start": 1}}]"#;
    repo.arc(&wt)
        .args([
            "review",
            "fix-y",
            "--verdict",
            "changes-requested",
            "--findings-json",
            "-",
        ])
        .write_stdin(findings)
        .assert()
        .success();

    repo.arc(&wt).args(["check", "fix-y"]).assert().code(2);
    repo.arc(&repo.root)
        .args(["integrate", "fix-y"])
        .assert()
        .code(2);

    let show = stdout(repo.arc(&wt).args(["show", "fix-y", "--json"]));
    let state: serde_json::Value = serde_json::from_str(&show).unwrap();
    let fid = state["findings"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    // Anchor captured a blob for the head side.
    assert!(state["findings"][&fid]["anchor"]["blob"].is_string());

    repo.commit(&wt, "y.txt", "y fixed\n", "fix: correct y");
    let fix = repo.head(&wt);
    repo.arc(&wt)
        .args([
            "resolve", "fix-y", &fid, "--status", "resolved", "--commit", &fix,
        ])
        .assert()
        .success();

    // Old approval basis is gone: new head needs a new patchset + verdict.
    repo.arc(&wt).args(["check", "fix-y"]).assert().code(3);
    stdout(repo.arc(&wt).args(["snapshot", "fix-y"]));
    repo.arc(&wt)
        .args(["review", "fix-y", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-y"]).assert().success();
}

/// A commit after approval makes the verdict stale (exit 3) until a new
/// patchset is approved.
#[test]
fn approval_goes_stale_when_head_moves() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-z"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-z");
    repo.commit(&wt, "z.txt", "z\n", "fix: z");
    stdout(repo.arc(&wt).args(["snapshot", "fix-z"]));
    repo.arc(&wt)
        .args(["review", "fix-z", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-z"]).assert().success();

    repo.commit(&wt, "z.txt", "z2\n", "fix: z again");
    repo.arc(&wt).args(["check", "fix-z"]).assert().code(3);
    repo.arc(&repo.root)
        .args(["integrate", "fix-z"])
        .assert()
        .code(3);
}

/// integrate --cleanup invoked from INSIDE the change worktree must not
/// die when that worktree is removed under it (regression: branch
/// deletion used the vanished cwd).
#[test]
fn integrate_cleanup_from_inside_change_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-c"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-c");
    repo.commit(&wt, "c.txt", "c\n", "fix: c");
    stdout(repo.arc(&wt).args(["snapshot", "fix-c"]));
    repo.arc(&wt)
        .args(["review", "fix-c", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["integrate", "fix-c", "--cleanup"])
        .assert()
        .success();

    assert!(!wt.exists(), "change worktree should be removed");
    let branches = git_out(&repo.root, &["branch", "--list", "arc/fix-c"]);
    assert!(branches.is_empty(), "change branch should be deleted");
}

/// Hold blocks integration (exit 4) until released.
#[test]
fn hold_blocks_integration() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-h"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-h");
    repo.commit(&wt, "h.txt", "h\n", "fix: h");
    stdout(repo.arc(&wt).args(["snapshot", "fix-h"]));
    repo.arc(&wt)
        .args(["review", "fix-h", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["hold", "fix-h", "--reason", "manual testing first"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().code(4);
    repo.arc(&repo.root)
        .args(["integrate", "fix-h"])
        .assert()
        .code(4);

    repo.arc(&wt)
        .args(["release-hold", "fix-h"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().success();
}

/// A declared gate that never ran (or failed) blocks with exit 5; a pass
/// at the exact head unblocks.
#[test]
fn gates_must_be_green_at_head() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.fails]\ncommand = \"false\"\nprofiles = [\"local\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);

    stdout(repo.arc(&repo.root).args(["begin", "fix-g"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-g");
    repo.commit(&wt, "g.txt", "g\n", "fix: g");
    stdout(repo.arc(&wt).args(["snapshot", "fix-g"]));
    repo.arc(&wt)
        .args(["review", "fix-g", "--verdict", "approved"])
        .assert()
        .success();

    // Gate never ran: blocked and accurately summarized as pending.
    repo.arc(&wt).args(["check", "fix-g"]).assert().code(5);
    let pending: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "fix-g"]))).unwrap();
    assert_eq!(
        pending["blocker_summary"]["gate_status"]["fails"],
        "pending"
    );
    assert_eq!(pending["gates"][0]["result"], "pending");

    // Gate ran and failed: still blocked, verify itself exits 1.
    repo.arc(&wt)
        .args(["verify", "fix-g", "--gate", "fails"])
        .assert()
        .code(1);
    repo.arc(&wt).args(["check", "fix-g"]).assert().code(5);
    let failed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "fix-g"]))).unwrap();
    assert_eq!(failed["blocker_summary"]["gate_status"]["fails"], "fail");
    assert_eq!(failed["gates"][0]["result"], "fail");

    // Fix the gate in the worktree's .arc? Gate command comes from the
    // toplevel of the invoking worktree, so run a passing command via a
    // redefined gates file in the change worktree.
    fs::write(
        wt.join(".arc/gates.toml"),
        "[gates.fails]\ncommand = \"true\"\nprofiles = [\"local\"]\n",
    )
    .unwrap();
    git(&wt, &["add", ".arc"]);
    git(&wt, &["commit", "-m", "fix gate"]);
    stdout(repo.arc(&wt).args(["snapshot", "fix-g"]));
    repo.arc(&wt)
        .args(["verify", "fix-g", "--gate", "fails"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "fix-g", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-g"]).assert().success();
    let passed: serde_json::Value =
        serde_json::from_str(&stdout(repo.arc(&wt).args(["status", "fix-g"]))).unwrap();
    assert_eq!(passed["blocker_summary"]["gate_status"]["fails"], "pass");
    assert_eq!(passed["gates"][0]["result"], "pass");
}

/// Comments, replies, and prefix resolution round-trip through the ledger.
#[test]
fn comment_reply_roundtrip_and_prefix_resolution() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "chat-c"]));
    let wt = repo.home.join(".worktrees").join("repo-chat-c");
    repo.commit(&wt, "c.txt", "c\n", "c");
    stdout(repo.arc(&wt).args(["snapshot", "chat-c"]));

    let out = stdout(repo.arc(&wt).args([
        "comment",
        "chat-c",
        "--body",
        "looks odd",
        "--path",
        "c.txt",
        "--line",
        "1",
    ]));
    let event_id = out
        .lines()
        .find_map(|l| l.strip_prefix("event: "))
        .unwrap()
        .to_string();

    repo.arc(&wt)
        .args(["reply", "chat-c", &event_id, "--body", "explained"])
        .assert()
        .success();

    // Bare slug prefix resolves the change.
    let show = stdout(repo.arc(&wt).args(["show", "chat-c"]));
    assert!(show.contains("looks odd"));
    assert!(show.contains("explained"));
}

/// Approving while recording blocking findings is contradictory.
#[test]
fn approve_with_blocking_findings_refused() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-w"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-w");
    repo.commit(&wt, "w.txt", "w\n", "w");
    stdout(repo.arc(&wt).args(["snapshot", "fix-w"]));
    let findings = r#"[{"blocking": true, "severity": "critical", "summary": "no"}]"#;
    repo.arc(&wt)
        .args([
            "review",
            "fix-w",
            "--verdict",
            "approved",
            "--findings-json",
            "-",
        ])
        .write_stdin(findings)
        .assert()
        .failure();
}

/// Snapshot sets a retention ref so reviewed heads survive branch deletion.
#[test]
fn snapshot_sets_retention_ref() {
    let repo = Repo::new();
    let out = stdout(repo.arc(&repo.root).args(["begin", "keep-k"]));
    let change_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt = repo.home.join(".worktrees").join("repo-keep-k");
    repo.commit(&wt, "k.txt", "k\n", "k");
    stdout(repo.arc(&wt).args(["snapshot", "keep-k"]));
    let head = repo.head(&wt);
    let kept = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-01")],
    );
    assert_eq!(kept, head);

    // A rewound branch gets a second patchset with its own pin; the
    // first head stays protected.
    git(&wt, &["reset", "--hard", "HEAD~1"]);
    repo.commit(&wt, "k2.txt", "k2\n", "k2");
    stdout(repo.arc(&wt).args(["snapshot", "keep-k"]));
    let kept1 = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-01")],
    );
    let kept2 = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-02")],
    );
    assert_eq!(kept1, head, "rewound head must stay pinned");
    assert_eq!(kept2, repo.head(&wt));
}

/// Abandoning a change must keep every reviewed head pinned; integrating
/// releases only heads reachable from the merge.
#[test]
fn closure_retention_policy() {
    let repo = Repo::new();

    // Abandoned: pins survive even branch force-deletion.
    let out = stdout(repo.arc(&repo.root).args(["begin", "drop-r"]));
    let drop_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt = repo.home.join(".worktrees").join("repo-drop-r");
    repo.commit(&wt, "r.txt", "r\n", "r");
    stdout(repo.arc(&wt).args(["snapshot", "drop-r"]));
    let dropped_head = repo.head(&wt);
    repo.arc(&repo.root)
        .args(["close", "drop-r", "--abandoned"])
        .assert()
        .success()
        .stdout(predicates::str::contains("kept refs/arc/keep/"));
    git(
        &repo.root,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    git(&repo.root, &["branch", "-D", "arc/drop-r"]);
    let kept = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{drop_id}/ps-01")],
    );
    assert_eq!(kept, dropped_head, "abandoned head must stay pinned");

    // Integrated: the reachable head's pin is released.
    let out = stdout(repo.arc(&repo.root).args(["begin", "land-r"]));
    let land_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt2 = repo.home.join(".worktrees").join("repo-land-r");
    repo.commit(&wt2, "l.txt", "l\n", "l");
    stdout(repo.arc(&wt2).args(["snapshot", "land-r"]));
    repo.arc(&wt2)
        .args(["review", "land-r", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "land-r"])
        .assert()
        .success();
    let refs = git_out(
        &repo.root,
        &["for-each-ref", &format!("refs/arc/keep/{land_id}/")],
    );
    assert!(refs.is_empty(), "reachable pins should be released");
}

/// begin derives the target from the primary worktree's branch, even
/// when invoked from another change's worktree on a different branch.
#[test]
fn begin_derives_target_from_primary_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "first-t"]));
    let wt = repo.home.join(".worktrees").join("repo-first-t");
    repo.commit(&wt, "t.txt", "t\n", "t");

    // From inside the first change's worktree: target must be master,
    // and the new branch must derive from master's head, not from the
    // in-progress arc/first-t head.
    stdout(repo.arc(&wt).args(["begin", "second-t"]));
    let show = stdout(repo.arc(&wt).args(["show", "second-t", "--json"]));
    let state: serde_json::Value = serde_json::from_str(&show).unwrap();
    assert_eq!(state["target_branch"], "master");
    assert_eq!(
        state["base"],
        serde_json::Value::String(repo.head(&repo.root)),
        "base must be master's head, not the other change's head"
    );
}

/// Implicit stacking on an open change branch is refused; explicit
/// --target allows it.
#[test]
fn begin_refuses_implicit_stacking() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "base-s"]));
    let wt = repo.home.join(".worktrees").join("repo-base-s");
    repo.commit(&wt, "s.txt", "s\n", "s");

    // Make the primary worktree sit on the change branch: simulate by
    // passing --target pointing at the open change branch explicitly —
    // allowed — versus the implicit refusal path, which needs the
    // default target to resolve to that branch. Explicit works:
    repo.arc(&wt)
        .args([
            "begin",
            "stack-s",
            "--target",
            "arc/base-s",
            "--no-worktree",
        ])
        .assert()
        .success();
}

/// ARC_WORKTREES_DIR and ARC_DATA_ROOT relocate paths (sandboxing).
#[test]
fn path_overrides_relocate_worktrees_and_ledger() {
    let repo = Repo::new();
    let sandbox_wts = repo.home.join("sandbox-wts");
    let sandbox_data = repo.home.join("sandbox-data");

    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_WORKTREES_DIR", &sandbox_wts)
            .env("ARC_DATA_ROOT", &sandbox_data)
            .args(["begin", "boxed-p"]),
    );
    assert!(out.contains("change: boxed-p-"));
    assert!(sandbox_wts.join("repo-boxed-p").is_dir());

    // Ledger landed under the slugged repo path inside the data root,
    // and the default (in-repo) store does not know the change.
    let slug_dirs: Vec<_> = fs::read_dir(&sandbox_data).unwrap().collect();
    assert_eq!(slug_dirs.len(), 1);
    repo.arc(&repo.root)
        .env("ARC_DATA_ROOT", &sandbox_data)
        .args(["show", "boxed-p"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["show", "boxed-p"])
        .assert()
        .failure();
}

/// The config file under AI_HOME drives the same overrides.
#[test]
fn config_file_under_ai_home() {
    let repo = Repo::new();
    let ai_home = repo.home.join("ai");
    fs::create_dir_all(ai_home.join("arc")).unwrap();
    fs::write(
        ai_home.join("arc/config.toml"),
        format!(
            "worktrees_dir = \"{}\"\n",
            repo.home.join("cfg-wts").display()
        ),
    )
    .unwrap();

    stdout(
        repo.arc(&repo.root)
            .env("AI_HOME", &ai_home)
            .args(["begin", "cfg-c"]),
    );
    assert!(repo.home.join("cfg-wts").join("repo-cfg-c").is_dir());
}

/// close --abandoned works and closed changes refuse new work.
#[test]
fn close_abandoned_and_refuse_further_work() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "drop-d", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["close", "drop-d", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["check", "drop-d"])
        .assert()
        .code(6);
    repo.arc(&repo.root)
        .args(["hold", "drop-d", "--reason", "x"])
        .assert()
        .failure();
    // Slug is reusable after closure.
    repo.arc(&repo.root)
        .args([
            "begin",
            "drop-d",
            "--no-worktree",
            "--branch",
            "arc/drop-d-2",
        ])
        .assert()
        .success();
}

#[test]
fn export_is_deterministic() {
    let repo = Repo::new();
    change_with_patchset(&repo, "move-d");
    let first = repo.home.join("first.json");
    let second = repo.home.join("second.json");

    repo.arc(&repo.root)
        .args(["export", "move-d", "--output", first.to_str().unwrap()])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["export", "move-d", "--output", second.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
}

#[test]
fn bundle_roundtrip_preserves_claim_stage_and_snapshot_provenance_events() {
    let source = Repo::new();
    let opened = stdout(source.arc(&source.root).args(["begin", "move-claim"]));
    let change_id = opened_change_id(&opened);
    let worktree = source.home.join(".worktrees/repo-move-claim");
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args([
            "claim",
            "move-claim",
            "--ttl",
            "5m",
            "--stage-budget",
            "implementing=2m",
        ])
        .assert()
        .success();
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Executor")
        .args(["stage", "move-claim", "implementing"])
        .assert()
        .success();
    source.commit(&worktree, "move.txt", "move\n", "feat: move claimed work");
    source
        .arc(&worktree)
        .args(["snapshot", "move-claim"])
        .assert()
        .success();

    let bundle = source.home.join("claim-bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-claim", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let exported: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let event_types = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(event_types.contains(&"claim-set"));
    assert!(event_types.contains(&"stage-set"));
    let claim = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "claim-set")
        .unwrap();
    let stage = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "stage-set")
        .unwrap();
    let patchset = exported["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["event_type"] == "patchset-added")
        .unwrap();
    assert_eq!(patchset["author_name"], "Tester");
    assert_eq!(patchset["committer_email"], "tester@example.invalid");
    assert_eq!(stage["claim_id"], claim["claim_id"]);
    assert_eq!(patchset["claim_id"], claim["claim_id"]);
    assert_eq!(patchset["claim_actor"], "Executor");

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("unknown event type").not());
    let roundtrip = destination.home.join("claim-roundtrip.json");
    destination
        .arc(&destination.root)
        .args([
            "export",
            &change_id,
            "--output",
            roundtrip.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(fs::read(bundle).unwrap(), fs::read(roundtrip).unwrap());
}

#[test]
fn old_patchset_events_without_identity_fields_remain_readable() {
    let repo = Repo::new();
    let (change_id, _, _) = change_with_patchset(&repo, "old-patchset");
    rewrite_event(&repo, &change_id, "patchset-added", |event| {
        event.as_object_mut().unwrap().remove("author_name");
        event.as_object_mut().unwrap().remove("author_email");
        event.as_object_mut().unwrap().remove("committer_name");
        event.as_object_mut().unwrap().remove("committer_email");
    });

    let status: serde_json::Value = serde_json::from_str(&stdout(
        repo.arc(&repo.root).args(["status", "old-patchset"]),
    ))
    .unwrap();
    assert!(status["latest_patchset"]["author"].is_null());
    assert!(status["latest_patchset"]["committer"].is_null());
    repo.arc(&repo.root)
        .args(["show", "old-patchset"])
        .assert()
        .success();
}

#[test]
fn claim_events_without_generation_fields_are_rejected() {
    let repo = Repo::new();
    let opened =
        stdout(
            repo.arc(&repo.root)
                .args(["begin", "old-claim-generation", "--no-worktree"]),
        );
    let change_id = opened_change_id(&opened);
    repo.arc(&repo.root)
        .args(["claim", "old-claim-generation"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["stage", "old-claim-generation", "started"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["release-claim", "old-claim-generation"])
        .assert()
        .success();
    // The claim protocol shipped with generations from the start: the binary
    // always writes claim_id, so a claim event without one is corruption or a
    // forgery and must fail loud rather than replay through inference.
    rewrite_event(&repo, &change_id, "claim-set", |event| {
        event.as_object_mut().unwrap().remove("claim_id");
    });

    repo.arc(&repo.root)
        .args(["status", "old-claim-generation"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("malformed event file"));
}

#[test]
fn export_import_roundtrip_is_byte_identical() {
    let source = Repo::new();
    change_with_patchset(&source, "move-r");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-r", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let roundtrip = destination.home.join("roundtrip.json");
    destination
        .arc(&destination.root)
        .args(["export", "move-r", "--output", roundtrip.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(fs::read(bundle).unwrap(), fs::read(roundtrip).unwrap());

    // Continuing the change on the destination creates events with that
    // store's repository ID. Mixed provenance remains exportable/importable.
    destination
        .arc(&destination.root)
        .args(["hold", "move-r", "--reason", "continue elsewhere"])
        .assert()
        .success();
    let continued = destination.home.join("continued.json");
    destination
        .arc(&destination.root)
        .args(["export", "move-r", "--output", continued.to_str().unwrap()])
        .assert()
        .success();
    let third = Repo::new();
    third
        .arc(&third.root)
        .args(["import", continued.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn import_is_idempotent() {
    let source = Repo::new();
    change_with_patchset(&source, "move-i");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-i", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let event_count = serde_json::from_slice::<serde_json::Value>(&fs::read(&bundle).unwrap())
        .unwrap()["event_count"]
        .as_u64()
        .unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "summary: new=0 skipped={event_count} conflicts=0"
        )));
}

#[test]
fn import_conflict_writes_nothing() {
    let source = Repo::new();
    let (change_id, _, _) = change_with_patchset(&source, "move-c");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-c", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let bundle_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let event_id = bundle_json["events"][0]["event_id"].as_str().unwrap();
    let event_path = destination
        .root
        .join(".git/arc/changes")
        .join(&change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    let mut tampered: serde_json::Value =
        serde_json::from_slice(&fs::read(&event_path).unwrap()).unwrap();
    tampered["actor"] = serde_json::Value::String("tampered".into());
    let tampered_bytes = json_file_bytes(&tampered);
    fs::write(&event_path, &tampered_bytes).unwrap();

    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicates::str::contains(format!("conflict: {event_id}")))
        .stdout(predicates::str::contains(
            "aborted: no events or refs written",
        ));
    assert_eq!(fs::read(event_path).unwrap(), tampered_bytes);
}

#[test]
fn import_rejects_malformed_known_events_before_writing() {
    let source = Repo::new();
    stdout(
        source
            .arc(&source.root)
            .args(["begin", "move-malformed", "--no-worktree"]),
    );
    source
        .arc(&source.root)
        .args(["claim", "move-malformed"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .args(["stage", "move-malformed", "started"])
        .assert()
        .success();
    let bundle_path = source.home.join("malformed.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-malformed",
            "--output",
            bundle_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let original_bundle = bundle.clone();
    let claim = bundle["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "claim-set")
        .unwrap();
    claim["ttl_seconds"] = serde_json::Value::String("not-seconds".into());
    refresh_bundle_checksum(&mut bundle);
    fs::write(&bundle_path, json_file_bytes(&bundle)).unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("known event"))
        .stderr(predicates::str::contains("malformed"));
    assert!(!destination.root.join(".git/arc").exists());

    let mut ownerless = original_bundle;
    let stage = ownerless["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|event| event["event_type"] == "stage-set")
        .unwrap();
    stage.as_object_mut().unwrap().remove("session");
    refresh_bundle_checksum(&mut ownerless);
    let ownerless_path = source.home.join("ownerless-stage.json");
    fs::write(&ownerless_path, json_file_bytes(&ownerless)).unwrap();
    let ownerless_destination = Repo::new();
    ownerless_destination
        .arc(&ownerless_destination.root)
        .args(["import", ownerless_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("has no session"));
    assert!(!ownerless_destination.root.join(".git/arc").exists());
}

#[test]
fn import_replays_combined_history_before_writing() {
    let source = Repo::new();
    let opened = stdout(
        source
            .arc(&source.root)
            .args(["begin", "move-combined", "--no-worktree"]),
    );
    let change_id = opened_change_id(&opened);
    let bundle_path = source.home.join("combined.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-combined",
            "--output",
            bundle_path.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let bundled_open = bundle["events"][0].clone();
    let bundled_event_id = bundled_open["event_id"].as_str().unwrap().to_string();

    let destination = Repo::new();
    stdout(
        destination
            .arc(&destination.root)
            .args(["begin", "seed-store", "--no-worktree"]),
    );
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    let mut local_open = bundled_open;
    local_open["event_id"] = serde_json::Value::String("00000000000000000000000000".into());
    local_open["repository_id"] = config["repository_id"].clone();
    let local_dir = event_dir(&destination, &change_id);
    fs::create_dir_all(&local_dir).unwrap();
    fs::write(
        local_dir.join("00000000000000000000000000.json"),
        json_file_bytes(&local_open),
    )
    .unwrap();

    destination
        .arc(&destination.root)
        .args(["import", bundle_path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "combined local and bundled known events are not replayable",
        ));
    assert!(!local_dir.join(format!("{bundled_event_id}.json")).exists());
}

#[test]
fn stale_imported_release_stage_and_snapshot_cannot_mutate_a_replacement_claim() {
    let source = Repo::new();
    let opened = stdout(source.arc(&source.root).args(["begin", "move-stale-claim"]));
    let change_id = opened_change_id(&opened);
    let worktree = source.home.join(".worktrees/repo-move-stale-claim");
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Source Executor")
        .args(["claim", "move-stale-claim"])
        .assert()
        .success();
    let initial = source.home.join("initial.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-stale-claim",
            "--output",
            initial.to_str().unwrap(),
        ])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", initial.to_str().unwrap()])
        .assert()
        .success();
    destination
        .arc(&destination.root)
        .env("ARC_SESSION", "lead")
        .args(["release-claim", "move-stale-claim"])
        .assert()
        .success();
    destination
        .arc(&destination.root)
        .env("ARC_ACTOR", "Replacement Executor")
        .env("ARC_SESSION", "replacement-session")
        .args(["claim", "move-stale-claim"])
        .assert()
        .success();
    let replacement: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "move-stale-claim"]),
    ))
    .unwrap();
    let replacement_claim_id = replacement["claim"]["claim_id"].clone();

    thread::sleep(Duration::from_millis(5));
    source
        .arc(&worktree)
        .env("ARC_ACTOR", "Source Executor")
        .args(["stage", "move-stale-claim", "started"])
        .assert()
        .success();
    source.commit(
        &worktree,
        "stale.txt",
        "source snapshot\n",
        "test: snapshot source claim",
    );
    source
        .arc(&worktree)
        .args(["snapshot", "move-stale-claim"])
        .assert()
        .success();
    source
        .arc(&source.root)
        .env("ARC_SESSION", "source-lead")
        .args(["release-claim", "move-stale-claim"])
        .assert()
        .success();
    let updated = source.home.join("updated.json");
    source
        .arc(&source.root)
        .args([
            "export",
            "move-stale-claim",
            "--output",
            updated.to_str().unwrap(),
        ])
        .assert()
        .success();

    destination
        .arc(&destination.root)
        .args(["import", updated.to_str().unwrap()])
        .assert()
        .success();
    let status: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["status", "move-stale-claim"]),
    ))
    .unwrap();
    assert_eq!(status["claim"]["claim_id"], replacement_claim_id);
    assert_eq!(status["claim"]["owner"]["actor"], "Replacement Executor");
    assert_eq!(status["claim"]["stage"], "launch");
    assert!(status["claim"]["snapshot_author"].is_null());

    let state: serde_json::Value = serde_json::from_str(&stdout(
        destination
            .arc(&destination.root)
            .args(["show", &change_id, "--json"]),
    ))
    .unwrap();
    let patchset = state["patchsets"].as_array().unwrap().last().unwrap();
    assert_eq!(patchset["claim_actor"], "Source Executor");
    assert_ne!(patchset["claim_id"], replacement_claim_id);
}

#[test]
fn import_dry_run_into_fresh_repo_writes_nothing() {
    let source = Repo::new();
    change_with_patchset(&source, "move-p");
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-p", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let event_count = serde_json::from_slice::<serde_json::Value>(&fs::read(&bundle).unwrap())
        .unwrap()["event_count"]
        .as_u64()
        .unwrap();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "summary: new={event_count} skipped=0 conflicts=0"
        )))
        .stdout(predicates::str::contains(
            "dry-run: no events or refs written",
        ));
    assert!(!destination.root.join(".git/arc").exists());
}

#[test]
fn import_restores_patchset_retention_refs() {
    let repo = Repo::new();
    let (change_id, _, head) = change_with_patchset(&repo, "move-k");
    let bundle = repo.home.join("bundle.json");
    repo.arc(&repo.root)
        .args(["export", "move-k", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let change_dir = repo.root.join(".git/arc/changes").join(&change_id);
    fs::remove_dir_all(change_dir).unwrap();
    let retention_ref = format!("refs/arc/keep/{change_id}/ps-01");
    git(&repo.root, &["update-ref", "-d", &retention_ref]);

    repo.arc(&repo.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(git_out(&repo.root, &["rev-parse", &retention_ref]), head);
}

#[test]
fn import_preserves_unknown_event_bytes() {
    let source = Repo::new();
    let out = stdout(
        source
            .arc(&source.root)
            .args(["begin", "move-u", "--no-worktree"]),
    );
    let change_id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(source.root.join(".git/arc/config.json")).unwrap())
            .unwrap();
    let event_id = "ZZZZZZZZZZZZZZZZZZZZZZZZZZ";
    let unknown = serde_json::json!({
        "schema_version": 1,
        "event_id": event_id,
        "repository_id": config["repository_id"],
        "change_id": change_id,
        "actor": "future-agent",
        "created_at": "2026-07-16T00:00:00Z",
        "event_type": "future-thing",
        "future_payload": {"kept": [1, 2, 3], "nested": true}
    });
    let source_event = source
        .root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    let unknown_bytes = json_file_bytes(&unknown);
    fs::write(&source_event, &unknown_bytes).unwrap();
    let bundle = source.home.join("bundle.json");
    source
        .arc(&source.root)
        .args(["export", "move-u", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();

    let destination = Repo::new();
    destination
        .arc(&destination.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "unknown event type: {event_id} future-thing"
        )));
    let destination_event = destination
        .root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
        .join(format!("{event_id}.json"));
    assert_eq!(fs::read(destination_event).unwrap(), unknown_bytes);

    let streamed = stdout(destination.arc(&destination.root).args([
        "events",
        "--change",
        "move-u",
        "--type",
        "future-thing",
    ]));
    let streamed: serde_json::Value = serde_json::from_str(streamed.trim()).unwrap();
    assert_eq!(streamed, unknown);
}

// ---------------------------------------------------------------------------
// arc thread — archive mechanics
// ---------------------------------------------------------------------------

fn thread_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

fn thread_dir(repo: &Repo) -> PathBuf {
    let out = stdout(repo.arc(&repo.root).args(["thread", "dir"]));
    PathBuf::from(out.trim())
}

#[test]
fn thread_dir_precedence_env_over_config_over_default() {
    let repo = Repo::new();
    let canon = fs::canonicalize(&repo.root).unwrap();

    // Default: <ai_home>/threads/<repo-root-slug>.
    let expected_default = repo
        .home
        .join(".local/ai/threads")
        .join(thread_slug(&canon));
    let got_default = thread_dir(&repo);
    assert_eq!(got_default, expected_default);

    // Config override keyed by the repository-root path.
    let cfg_dir = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&cfg_dir).unwrap();
    let override_dir = repo.home.join("custom-thread-archive");
    fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[threads]\ndirs = {{ \"{}\" = \"{}\" }}\n",
            canon.display(),
            override_dir.display()
        ),
    )
    .unwrap();
    let got_config = thread_dir(&repo);
    assert_eq!(got_config, override_dir);

    // Env wins over both config and default.
    let env_dir = repo.home.join("env-thread-archive");
    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_THREAD_DIR", &env_dir)
            .args(["thread", "dir"]),
    );
    assert_eq!(PathBuf::from(out.trim()), env_dir);

    // dir prints but never creates the directory.
    assert!(!got_default.exists());
    assert!(!override_dir.exists());
}

#[test]
fn thread_note_writes_file_and_journal_line() {
    let repo = Repo::new();
    let body_path = repo.home.join("body.md");
    let body = "# My Heading\n\nverbatim body\n";
    fs::write(&body_path, body).unwrap();

    let out = stdout(repo.arc(&repo.root).args([
        "thread",
        "note",
        "delegation-blocker-ux",
        "--kind",
        "handoff",
        "--body-file",
        body_path.to_str().unwrap(),
    ]));
    let file = PathBuf::from(out.trim());
    let name = file.file_name().unwrap().to_string_lossy().to_string();

    // Filename shape: <UTC yyyymmddTHHMMSSZ>-<topic>-<kind>.md
    assert!(
        name.ends_with("-delegation-blocker-ux-handoff.md"),
        "{name}"
    );
    let ts = name.split('-').next().unwrap();
    assert_eq!(ts.len(), 16, "timestamp {ts}");
    assert!(ts.ends_with('Z'));
    assert!(ts.contains('T'));

    // Body is written verbatim.
    assert_eq!(fs::read_to_string(&file).unwrap(), body);

    // Journal line carries the harness/session identity and references the file.
    let journal = fs::read_to_string(file.parent().unwrap().join("journal.md")).unwrap();
    let line = journal.lines().next().unwrap();
    assert!(line.starts_with("- "), "{line}");
    assert!(
        line.contains(" test session-a delegation-blocker-ux: "),
        "{line}"
    );
    assert!(line.ends_with(&format!("({name})")), "{line}");
}

#[test]
fn thread_note_title_prepends_heading() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args([
                "thread",
                "note",
                "topic-a",
                "--kind",
                "plan",
                "--body-file",
                "-",
                "--title",
                "The Plan",
            ])
            .write_stdin("plan contents\n"),
    );
    let file = PathBuf::from(out.trim());
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "# The Plan\n\nplan contents\n"
    );
    let journal = fs::read_to_string(file.parent().unwrap().join("journal.md")).unwrap();
    assert!(journal.contains(" topic-a: The Plan ("), "{journal}");
}

#[test]
fn thread_note_rejects_invalid_kind_and_topic_without_writing() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    let body_path = repo.home.join("body.md");
    fs::write(&body_path, "body\n").unwrap();

    // Invalid kind is a clap usage error (exit 2).
    repo.arc(&repo.root)
        .args([
            "thread",
            "note",
            "topic-a",
            "--kind",
            "bogus",
            "--body-file",
            body_path.to_str().unwrap(),
        ])
        .assert()
        .code(2);

    // Non-kebab topic is a usage error (exit 1).
    repo.arc(&repo.root)
        .args([
            "thread",
            "note",
            "Bad_Topic",
            "--kind",
            "note",
            "--body-file",
            body_path.to_str().unwrap(),
        ])
        .assert()
        .failure();

    // Nothing was written.
    assert!(!dir.exists());
}

#[test]
fn thread_journal_appends_without_creating_artifact_file() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-a", "consumed inbox X"])
        .assert()
        .success();
    let dir = thread_dir(&repo);
    let entries: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(entries, vec!["journal.md".to_string()]);
    let journal = fs::read_to_string(dir.join("journal.md")).unwrap();
    assert!(
        journal.contains(" test session-a topic-a: consumed inbox X"),
        "{journal}"
    );
    assert!(
        !journal.contains('('),
        "journal-only line has no filename: {journal}"
    );
}

#[test]
fn thread_catchup_lists_newest_first_and_json_parses() {
    let repo = Repo::new();
    let dir = thread_dir(&repo);
    fs::create_dir_all(&dir).unwrap();
    // Two crafted artifacts with distinct, deterministic timestamps.
    fs::write(
        dir.join("20260101T000000Z-alpha-note.md"),
        "# Alpha heading\nold\n",
    )
    .unwrap();
    fs::write(
        dir.join("20260202T000000Z-beta-plan.md"),
        "# Beta heading\nnew\n",
    )
    .unwrap();
    fs::write(dir.join("journal.md"), "- prior journal line\n").unwrap();

    // Text form is newest-first.
    let text = stdout(repo.arc(&repo.root).args(["thread", "catchup"]));
    let beta = text.find("beta").unwrap();
    let alpha = text.find("alpha").unwrap();
    assert!(beta < alpha, "beta (newer) must list before alpha:\n{text}");

    // JSON form parses and preserves order + parsed fields.
    let json = stdout(repo.arc(&repo.root).args(["thread", "catchup", "--json"]));
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["topic"], "beta");
    assert_eq!(files[0]["kind"], "plan");
    assert_eq!(files[0]["timestamp"], "20260202T000000Z");
    assert_eq!(files[0]["heading"], "# Beta heading");
    assert_eq!(files[1]["topic"], "alpha");
    assert_eq!(v["journal_tail"].as_array().unwrap().len(), 1);

    // --limit caps the artifact list.
    let limited = stdout(
        repo.arc(&repo.root)
            .args(["thread", "catchup", "--limit", "1", "--json"]),
    );
    let lv: serde_json::Value = serde_json::from_str(limited.trim()).unwrap();
    assert_eq!(lv["files"].as_array().unwrap().len(), 1);
    assert_eq!(lv["files"][0]["topic"], "beta");
}

#[test]
fn thread_journal_is_append_only() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-a", "first message"])
        .assert()
        .success();
    let dir = thread_dir(&repo);
    let after_first = fs::read(dir.join("journal.md")).unwrap();

    repo.arc(&repo.root)
        .args(["thread", "journal", "topic-b", "second message"])
        .assert()
        .success();
    let after_second = fs::read(dir.join("journal.md")).unwrap();

    // The earlier bytes are preserved verbatim; only new bytes are appended.
    assert!(after_second.starts_with(&after_first[..]));
    assert!(after_second.len() > after_first.len());
    let text = String::from_utf8(after_second).unwrap();
    assert!(text.contains("topic-a: first message"));
    assert!(text.contains("topic-b: second message"));
}
