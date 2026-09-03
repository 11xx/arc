pub(crate) use assert_cmd::Command as AssertCommand;
pub(crate) use predicates::prelude::PredicateBooleanExt;
pub(crate) use sha2::{Digest, Sha256};
pub(crate) use std::fs;
pub(crate) use std::io::{BufRead, BufReader, Read};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::{Child, Command, ExitStatus, Stdio};
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, Instant};
pub(crate) use tempfile::TempDir;

pub(crate) struct Repo {
    _tmp: TempDir,
    pub(crate) root: PathBuf,
    pub(crate) home: PathBuf,
}

impl Repo {
    pub(crate) fn new() -> Repo {
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

    pub(crate) fn arc(&self, cwd: &Path) -> AssertCommand {
        let mut cmd = AssertCommand::cargo_bin("arc").unwrap();
        cmd.current_dir(cwd)
            .env("HOME", &self.home)
            .env("ARC_ACTOR", "tester")
            .env("ARC_HARNESS", "test")
            .env("ARC_SESSION", "session-a")
            .env_remove("ARC_ROLE")
            .env_remove("ARC_MODEL")
            .env_remove("ARC_ON_BEHALF_OF")
            .env_remove("ARC_DATA_DIR")
            .env_remove("ARC_DATA_ROOT")
            .env_remove("ARC_WORKTREES_DIR")
            .env_remove("AI_HOME")
            // The suite may itself run inside a harness. Whatever session
            // variable that harness exports must not reach the binary under
            // test, or `env` detects the runner instead of the fixture.
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("OPENCODE_SESSION")
            .env_remove("OPENCODE_TERMINAL")
            .env_remove("PI_SESSION_ID");
        cmd
    }

    pub(crate) fn commit(&self, cwd: &Path, file: &str, content: &str, msg: &str) {
        fs::write(cwd.join(file), content).unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", msg]);
    }

    pub(crate) fn head(&self, cwd: &Path) -> String {
        git_out(cwd, &["rev-parse", "HEAD"])
    }
}

pub(crate) fn git(cwd: &Path, args: &[&str]) {
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

pub(crate) fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub(crate) fn stdout(cmd: &mut AssertCommand) -> String {
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub(crate) fn change_with_patchset(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
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

pub(crate) fn complete_change(repo: &Repo, slug: &str) {
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

pub(crate) fn json_file_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

pub(crate) fn spawn_arc(repo: &Repo, cwd: &Path, args: &[&str]) -> Child {
    spawn_arc_with_session(repo, cwd, args, "session-a")
}

pub(crate) fn spawn_arc_with_session(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    session: &str,
) -> Child {
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

pub(crate) fn refresh_bundle_checksum(bundle: &mut serde_json::Value) {
    let mut digest = Sha256::new();
    for event in bundle["events"].as_array().unwrap() {
        digest.update(serde_json::to_vec(event).unwrap());
        digest.update(b"\n");
    }
    bundle["events_sha256"] = serde_json::Value::String(hex::encode(digest.finalize()));
}

pub(crate) fn opened_change_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("begin output should contain a change id")
        .to_string()
}

pub(crate) fn event_dir(repo: &Repo, change_id: &str) -> PathBuf {
    repo.root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
}

pub(crate) fn event_count(repo: &Repo, change_id: &str) -> usize {
    fs::read_dir(event_dir(repo, change_id)).unwrap().count()
}

pub(crate) fn rewrite_event(
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

pub(crate) fn age_event(repo: &Repo, change_id: &str, event_type: &str, seconds: i64) {
    rewrite_event(repo, change_id, event_type, |event| {
        event["created_at"] = serde_json::Value::String(
            (chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339(),
        );
    });
}

pub(crate) fn hold_transition_lock(repo: &Repo, change_id: &str) -> fs::File {
    hold_named_lock(repo, &format!("{change_id}.lock"))
}

pub(crate) fn hold_graph_lock(repo: &Repo) -> fs::File {
    hold_named_lock(repo, "graph.lock")
}

pub(crate) fn hold_target_lock(repo: &Repo, target: &str) -> fs::File {
    let digest = Sha256::digest(target.as_bytes());
    hold_named_lock(repo, &format!("target-{}.lock", hex::encode(digest)))
}

pub(crate) fn hold_named_lock(repo: &Repo, name: &str) -> fs::File {
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

pub(crate) fn assert_waiting_on_transition_lock(children: &mut [&mut Child]) {
    thread::sleep(Duration::from_millis(250));
    for child in children {
        assert!(
            child.try_wait().unwrap().is_none(),
            "transition command bypassed the externally held product lock"
        );
    }
}

pub(crate) fn wait_for_exit(child: &mut Child) -> ExitStatus {
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

pub(crate) fn child_stdout(child: &mut Child) -> String {
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    output
}

/// Open a change with `--no-worktree` and no checkout of its own.
///
/// `--no-worktree` takes over a clean checkout standing on the target, which
/// is what it is for and what the lifecycle tests cover directly. A test that
/// opens several changes in one repository wants none of them checked out, so
/// it holds the tree dirty for the duration — the same decline a caller with
/// unrelated work in progress gets.
pub(crate) fn with_uncommitted_worktree<T>(repo: &Repo, f: impl FnOnce() -> T) -> T {
    let path = repo.root.join(".arc-test-uncommitted");
    assert!(!path.exists(), "test fixture path already exists: {path:?}");
    fs::write(&path, b"temporary uncommitted fixture\n").unwrap();
    let result = f();
    fs::remove_file(path).unwrap();
    result
}

pub(crate) fn begin_no_worktree(repo: &Repo, slug: &str, extra: &[&str]) -> String {
    let out = with_uncommitted_worktree(repo, || {
        let mut command = repo.arc(&repo.root);
        command.args(["begin", slug, "--no-worktree"]);
        command.args(extra);
        stdout(&mut command)
    });
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

pub(crate) fn begin_change(repo: &Repo, slug: &str, blocked_by: Option<&str>) -> String {
    let extra = blocked_by
        .map(|blocker| vec!["--blocked-by", blocker])
        .unwrap_or_default();
    begin_no_worktree(repo, slug, &extra)
}

pub(crate) fn json_stdout(cmd: &mut AssertCommand) -> serde_json::Value {
    serde_json::from_str(&stdout(cmd)).unwrap()
}

/// Write one journal artifact and return the directory holding it and its
/// filename, which is how every artifact-subject command addresses it.
pub(crate) fn journal_artifact(
    repo: &Repo,
    topic: &str,
    kind: &str,
    body: &str,
) -> (PathBuf, String) {
    let source = repo.home.join(format!("{topic}-body.md"));
    fs::write(&source, body).unwrap();
    let printed = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        topic,
        "--kind",
        kind,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    let path = PathBuf::from(printed.trim());
    let file = path.file_name().unwrap().to_string_lossy().to_string();
    (path.parent().unwrap().to_path_buf(), file)
}

/// Every typed journal event, in the order they were appended.
pub(crate) fn journal_event_log(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The claim IDs `claim-set` recorded on one artifact, oldest first.
pub(crate) fn artifact_claim_ids(dir: &Path, file: &str) -> Vec<String> {
    journal_event_log(dir)
        .into_iter()
        .filter(|event| event["event"] == "claim-set" && event["file"] == file)
        .map(|event| event["claim_id"].as_str().unwrap().to_string())
        .collect()
}

/// Run a command that executes a fixture script this suite wrote itself,
/// tolerating the kernel's text-file-busy refusal.
///
/// A fixture is executable the moment its bytes are written, but a sibling
/// test thread that forks while the writing descriptor is still open hands the
/// child an inherited copy of it. Until that child reaches its own exec, the
/// fixture's inode has an open writer, and any exec of it is refused. The
/// window is microseconds wide and closes on its own, so retry across it.
pub(crate) fn output_past_busy_text(command: &mut Command) -> std::process::Output {
    for _ in 0..40 {
        match command.output() {
            Err(err) if err.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(Duration::from_millis(25));
            }
            other => return other.unwrap(),
        }
    }
    panic!("fixture executable stayed busy");
}
