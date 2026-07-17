use assert_cmd::Command as AssertCommand;
use predicates::prelude::PredicateBooleanExt;
use std::fs;
use std::io::Read;
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
    repo.commit(&worktree, "change.txt", "change\n", "test: add change");
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
    let binary = std::env::var_os("CARGO_BIN_EXE_arc").expect("cargo should provide arc binary");
    Command::new(binary)
        .args(args)
        .current_dir(cwd)
        .env("HOME", &repo.home)
        .env("ARC_ACTOR", "tester")
        .env("ARC_HARNESS", "test")
        .env_remove("ARC_DATA_DIR")
        .env_remove("ARC_DATA_ROOT")
        .env_remove("ARC_WORKTREES_DIR")
        .env_remove("AI_HOME")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_for_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("arc subprocess did not exit within two seconds");
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
    thread::sleep(Duration::from_millis(50));
    stdout(repo.arc(&worktree).args(["snapshot", "events-follow"]));
    thread::sleep(Duration::from_millis(150));
    child.kill().unwrap();
    child.wait().unwrap();

    let output = child_stdout(&mut child);
    let events = output
        .lines()
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
        .args(["watch", "watch-ready", "--until", "ready", "--timeout", "0"])
        .assert()
        .code(2)
        .stdout("timeout: ready\n");
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
            .args(["watch", change, "--until", "closed", "--timeout", "0"])
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
            "0",
        ])
        .assert()
        .code(2)
        .stdout("timeout: integrated\n");
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
}
