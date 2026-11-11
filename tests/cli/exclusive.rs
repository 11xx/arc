use crate::common::*;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};

struct Signal(fs::File);

impl Signal {
    fn new(path: &Path) -> Self {
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: mkfifo borrows a NUL-terminated fixture path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        Self(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap(),
        )
    }

    fn wait(&mut self) {
        let mut ready = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows one initialized descriptor with a bounded wait.
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, HANG_BOUND.as_millis() as i32) },
            1,
            "fixture signal never arrived"
        );
        self.0.read_exact(&mut [0]).unwrap();
    }

    fn send(&mut self) {
        self.0.write_all(b"x").unwrap();
    }
}

fn lock_name() -> String {
    format!("s7-{}", ulid::Ulid::new())
}

fn lock_path(repo: &Repo, name: &str) -> PathBuf {
    let root = repo.home.join(".local/ai/arc/exclusive");
    fs::create_dir_all(&root).unwrap();
    root.join(format!(
        "{}.lock",
        hex::encode(Sha256::digest(name.as_bytes()))
    ))
}

fn elapsed_event(duration: Duration) {
    // SAFETY: timerfd_create returns an owned descriptor; settime borrows an
    // initialized one-shot timer and does not retain the structure.
    let timer = unsafe {
        let fd = libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC);
        assert!(fd >= 0);
        let settings = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: duration.as_secs() as i64,
                tv_nsec: duration.subsec_nanos() as i64,
            },
        };
        assert_eq!(
            libc::timerfd_settime(fd, 0, &settings, std::ptr::null_mut()),
            0
        );
        fs::File::from_raw_fd(fd)
    };
    (&timer).read_exact(&mut [0; 8]).unwrap();
}

fn wait_until_released(repo: &Repo, name: &str) {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(repo, name))
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        file.lock().unwrap();
        drop(file);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(HANG_BOUND)
        .expect("holder retained the lock after its descendants exited");
    waiter.join().unwrap();
}

fn declare(repo: &Repo, command: &str, extra: &str) {
    repo.redeclare_gates(&format!("[gates.unit]\ncommand = {command:?}\n{extra}"));
}

fn begin(repo: &Repo, name: &str) {
    stdout(repo.arc(&repo.root).args(["begin", name, "--no-worktree"]));
}

fn evidence(repo: &Repo, name: &str) -> serde_json::Value {
    let view = json_stdout(repo.arc(&repo.root).args(["show", name, "--json"]));
    view["verifications"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone()
}

fn waiting(child: &mut Child) -> thread::JoinHandle<String> {
    let stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut text = String::new();
        BufReader::new(stderr).read_to_string(&mut text).unwrap();
        text
    })
}

fn shared_spawn(repo: &Repo, home: &Path, args: &[&str]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arc"));
    let fixture = repo.fixture_env(AssertCommand::new("true"), &repo.root);
    command
        .current_dir(&repo.root)
        .args(args)
        .envs(
            fixture
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .env("ARC_SANDBOX", home)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in fixture.get_envs() {
        if value.is_none() {
            command.env_remove(key);
        }
    }
    command.spawn().unwrap()
}

/// Claim: equal exclusive names serialize commands across projects and checkouts,
/// and the second receipt charges only command execution to run_ms. Rejects a
/// repository-scoped lock, overlapping commands, missing owner details, or charging wait.
#[test]
fn exclusive_serializes_projects_and_separates_wait_from_run() {
    let name = lock_name();
    let first = Repo::new();
    let second = Repo::new();
    let ready_path = first.home.join("ready");
    let release_path = first.home.join("release");
    let mut ready = Signal::new(&ready_path);
    let mut release = Signal::new(&release_path);
    let marker = first.home.join("active");
    let script = first.home.join("gate.py");
    fs::write(
        &script,
        format!(
            r#"import os,sys
marker={marker:?}
fd=os.open(marker,os.O_CREAT|os.O_EXCL|os.O_WRONLY)
os.close(fd)
if sys.argv[1]=='first':
    with open({ready_path:?},'wb',buffering=0) as f: f.write(b'x')
    with open({release_path:?},'rb',buffering=0) as f: f.read(1)
os.unlink(marker)
"#
        ),
    )
    .unwrap();
    declare(
        &first,
        &format!("python3 {} first", script.display()),
        &format!("exclusive={name:?}\n"),
    );
    declare(
        &second,
        &format!("python3 {} second", script.display()),
        &format!("exclusive={name:?}\n"),
    );
    begin(&first, "first");
    begin(&second, "second");
    let one = first.spawn_arc_piped(&first.root, &["verify", "--gate", "unit"]);
    ready.wait();
    let mut two = shared_spawn(&second, &first.home, &["verify", "--all"]);
    let stderr = two.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        let mut text = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            if line.contains("waiting for exclusive") {
                let _ = tx.send(line.clone());
            }
            text.push_str(&line);
            text.push('\n');
        }
        text
    });
    let announced = rx.recv_timeout(HANG_BOUND);
    elapsed_event(Duration::from_millis(400));
    release.send();
    let one = one.wait_with_output().unwrap();
    let two = two.wait_with_output().unwrap();
    let stderr = reader.join().unwrap();
    assert!(
        one.status.success(),
        "{}",
        String::from_utf8_lossy(&one.stderr)
    );
    assert!(
        two.status.success(),
        "{stderr}\n{}",
        evidence(&second, "second")
    );
    let announced = announced.expect("waiter did not identify the exclusive holder");
    assert!(
        announced.contains(first.root.to_str().unwrap()),
        "{announced}"
    );
    assert!(
        announced.contains("change first-")
            && announced.contains("gate unit")
            && announced.contains("held for"),
        "{announced}"
    );
    let receipt = evidence(&second, "second");
    let wait = receipt["wait_ms"].as_u64().unwrap();
    let run = receipt["run_ms"].as_u64().unwrap();
    let duration = receipt["duration_ms"].as_u64().unwrap();
    assert!(wait >= 390, "exclusive wait missing: {receipt}");
    assert!(
        wait + run <= duration + 2,
        "exclusive wait charged to command: {receipt}"
    );
    eprintln!("overlap measurement: wait_ms={wait}, run_ms={run}");
}

/// Claim: admission expiry starts no command and records not-admitted with its
/// wait, including publication and candidates, not a failure or fabricated run time.
/// Rejects a failed run on expiry or another gate runner bypassing admission.
#[test]
fn exclusive_admission_expiry_is_unrun() {
    let name = lock_name();
    let repo = Repo::new();
    declare(
        &repo,
        "touch should-not-run",
        &format!("exclusive={name:?}\nadmission='150ms'\n"),
    );
    begin(&repo, "expiry");
    let lock_path = lock_path(&repo, &name);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    // SAFETY: the fixture owns this descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    repo.arc(&repo.root)
        .args(["verify", "--gate", "unit"])
        .assert()
        .code(1);
    let receipt = evidence(&repo, "expiry");
    assert_eq!(
        receipt["result"], "not-admitted",
        "admission was recorded as gate failure: {receipt}"
    );
    assert!(receipt["wait_ms"].as_u64().unwrap() >= 140, "{receipt}");
    assert!(
        receipt["run_ms"].is_null(),
        "unrun command was timed: {receipt}"
    );
    assert!(!repo.root.join("should-not-run").exists());
    let publication =
        stdout_any_status(
            repo.arc(&repo.root)
                .args(["publication", "--target", "master"]),
        );
    assert!(
        publication.contains("publication gate unit: not-admitted"),
        "publication bypassed exclusive admission: {publication}"
    );
    repo.arc(&repo.root)
        .args(["brief", "expiry", "--body-file", "-"])
        .write_stdin("Exercise a declared gate.")
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "candidate",
            "register",
            "--tree",
            "HEAD",
            "--brief",
            "expiry",
            "--producer",
            "fixture",
            "--id",
            "held",
        ])
        .assert()
        .success();
    let candidate = stdout_any_status(repo.arc(&repo.root).args(["candidate", "verify", "held"]));
    assert!(
        candidate.contains("gate unit: not-admitted"),
        "candidate bypassed exclusive admission: {candidate}"
    );
    drop(lock);
    // A timed-out helper must not acquire and retain a lock after its caller returns.
    repo.arc(&repo.root)
        .args(["verify", "--gate", "unit"])
        .assert()
        .success();
}

/// Claim: launcher death, command completion and timeout retain exclusivity while
/// a detached descendant lives, even if it closes all inherited descriptors. Rejects a
/// lock owned only by arc, the command leader, or a descendant's open descriptors.
#[test]
fn exclusive_survives_launcher_death_and_detached_children() {
    descendant_case("killed");
    descendant_case("normal");
    #[cfg(debug_assertions)]
    descendant_case("timeout");
}

fn descendant_case(mode: &str) {
    let name = lock_name();
    let first = Repo::new();
    let second = Repo::new();
    let mut ready = Signal::new(&first.home.join("ready"));
    let mut release = Signal::new(&first.home.join("release"));
    let done_path = first.home.join("done");
    let mut done = Signal::new(&done_path);
    let _leader_release = Signal::new(&first.home.join("leader-release"));
    let script = first.home.join("child.py");
    fs::write(
        &script,
        format!(
            r#"import os
if os.fork()==0:
    os.setsid()
    os.closerange(0,65536)
    with open({:?},'wb',buffering=0) as f: f.write(b'x')
    with open({:?},'rb',buffering=0) as f: f.read(1)
    os._exit(0)
with open({done_path:?},'wb',buffering=0) as f: f.write(b'x')
if {mode:?} != 'normal':
    with open({:?},'rb',buffering=0) as f: f.read(1)
"#,
            first.home.join("ready"),
            first.home.join("release"),
            first.home.join("leader-release")
        ),
    )
    .unwrap();
    declare(
        &first,
        &format!("python3 {}", script.display()),
        &format!(
            "exclusive={name:?}\n{}",
            if mode == "timeout" {
                "timeout='1s'\n"
            } else {
                ""
            }
        ),
    );
    declare(
        &second,
        "true",
        &format!("exclusive={name:?}\nadmission='200ms'\n"),
    );
    begin(&first, "parent");
    begin(&second, "child");
    let mut one = if mode == "timeout" {
        #[cfg(debug_assertions)]
        {
            spawn_arc_at_rendezvous(
                &first,
                &first.root,
                &["verify", "--gate", "unit"],
                "gate-command-spawned",
                &[],
            )
        }
        #[cfg(not(debug_assertions))]
        {
            unreachable!("timeout rendezvous requires debug assertions")
        }
    } else {
        first.spawn_arc_piped(&first.root, &["verify", "--gate", "unit"])
    };
    ready.wait();
    done.wait();
    if mode == "timeout" {
        elapsed_event(Duration::from_secs(1));
        #[cfg(debug_assertions)]
        release_lock_wait(&mut one);
    }
    if mode == "killed" {
        one.kill().unwrap();
        one.wait().unwrap();
    } else {
        let output = one.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(if mode == "timeout" { 1 } else { 0 }),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut two = shared_spawn(&second, &first.home, &["verify", "--gate", "unit"]);
    let text = waiting(&mut two);
    let output = two.wait_with_output().unwrap();
    release.send();
    let text = text.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{mode}: exclusive lock released while detached child lived: {text}"
    );
    assert_eq!(evidence(&second, "child")["result"], "not-admitted");
    wait_until_released(&first, &name);
    let output = shared_spawn(&second, &first.home, &["verify", "--gate", "unit"])
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Claim: exclusive and admission do not change the check's evidence identity,
/// so delta selection reuses a pass after adding a lock. Rejects putting either
/// admission key into the declaration digest.
#[test]
fn exclusive_admission_keys_do_not_invalidate_evidence() {
    let name = lock_name();
    let repo = Repo::new();
    let script = repo.home.join("select.py");
    fs::write(&script, r#"import json,sys
print(json.dumps({'schema':'arc-gate-selection/1','complete':True,'request':json.load(sys.stdin),'gates':[]}))
"#).unwrap();
    let declaration = format!(
        "[gates.unit]\ncommand='true'\nselector='python3 {}'\n",
        script.display()
    );
    repo.redeclare_gates(&declaration);
    begin(&repo, "identity");
    repo.arc(&repo.root)
        .args(["verify", "--all"])
        .assert()
        .success();
    repo.commit(
        &repo.root,
        "README.md",
        "delta",
        "test: exercise declaration identity",
    );
    repo.arc(&repo.root)
        .args(["verify", "--all"])
        .assert()
        .success();
    let digest = || {
        let events = stdout(repo.arc(&repo.root).args([
            "events",
            "--change",
            "identity",
            "--type",
            "verification-reused",
        ]));
        let event: serde_json::Value =
            serde_json::from_str(events.lines().last().unwrap()).unwrap();
        event["delta"]["declaration"].as_str().unwrap().to_owned()
    };
    let before = digest();
    repo.commit(
        &repo.root,
        ".arc/gates.toml",
        &format!("{declaration}exclusive={name:?}\nadmission='2.5s'\n"),
        "test: add admission",
    );
    let output = stdout(repo.arc(&repo.root).args(["verify", "--all"]));
    assert_eq!(before, digest(), "admission keys changed evidence identity");
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "identity",
        "--type",
        "verification-recorded",
    ]));
    assert_eq!(
        events.lines().count(),
        1,
        "admission keys invalidated evidence: {output}"
    );
    repo.arc(&repo.root)
        .args(["verify", "--all", "--rerun"])
        .assert()
        .success();
    assert!(
        repo.home.join(".local/ai/arc/exclusive").is_dir(),
        "change could not add a lock"
    );
}

/// Claim: a change cannot remove or rename the target's lock or loosen its
/// admission. Rejects execution without the target's lock after either edit.
#[test]
fn exclusive_target_lock_cannot_be_removed_or_renamed() {
    let name = lock_name();
    let repo = Repo::new();
    declare(
        &repo,
        "true",
        &format!("exclusive={name:?}\nadmission='150ms'\n"),
    );
    begin(&repo, "target-lock");
    let path = lock_path(&repo, &name);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    // SAFETY: the fixture owns the descriptor.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    for keys in [
        String::new(),
        format!("exclusive={:?}\nadmission='1s'\n", lock_name()),
    ] {
        repo.commit(
            &repo.root,
            ".arc/gates.toml",
            &format!("[gates.unit]\ncommand='true'\n{keys}"),
            "test: try to loosen admission",
        );
        repo.arc(&repo.root)
            .args(["verify", "--all", "--rerun"])
            .assert()
            .code(1);
        assert_eq!(
            evidence(&repo, "target-lock")["result"],
            "not-admitted",
            "target lock removed or renamed"
        );
    }
}
