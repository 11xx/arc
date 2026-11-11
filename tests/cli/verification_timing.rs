use crate::common::*;
use std::os::fd::{AsRawFd, FromRawFd};

const FIELDS: &[&str] = &[
    "wait_ms",
    "prepare_ms",
    "run_ms",
    "cpu_user_ms",
    "cpu_system_ms",
    "load_average",
    "core_count",
];

fn declare(repo: &Repo, command: &str, worktrees: &str) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!("[gates.unit]\ncommand = {command:?}\n"),
    )
    .unwrap();
    fs::write(repo.root.join(".arc/worktrees.toml"), worktrees).unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(
        &repo.root,
        &["commit", "-m", "test: declare receipt fixtures"],
    );
}

fn begin(repo: &Repo, slot: bool) -> (String, PathBuf) {
    let mut command = repo.arc(&repo.root);
    command.args(["begin", "receipts"]);
    if slot {
        command.arg("--slot");
    }
    let out = stdout(&mut command);
    let id = opened_change_id(&out);
    let path = out
        .lines()
        .find_map(|line| line.strip_prefix("worktree: "))
        .unwrap();
    (id, PathBuf::from(path))
}

fn receipt(repo: &Repo, wt: &Path) -> serde_json::Value {
    repo.arc(wt)
        .args(["verify", "--gate", "unit"])
        .assert()
        .success();
    let events = stdout(repo.arc(wt).args([
        "events",
        "--change",
        "receipts",
        "--type",
        "verification-recorded",
    ]));
    serde_json::from_str(events.lines().last().unwrap()).unwrap()
}

/// Claim: command wall time excludes cold preparation while duration retains it,
/// and show/status expose the same receipt. Rejects charging preparation to run_ms.
#[test]
fn command_timing_excludes_cold_preparation() {
    let repo = Repo::new();
    declare(
        &repo,
        "sleep 0.1",
        "[prepare]\ncommand = 'sleep 0.4'\ntimeout = '5s'\n",
    );
    let (_, wt) = begin(&repo, false);
    repo.commit(&wt, "source", "changed", "test: require fresh preparation");
    let event = receipt(&repo, &wt);
    let prepare = event["prepare_ms"].as_u64().unwrap();
    let run = event["run_ms"].as_u64().unwrap();
    let duration = event["duration_ms"].as_u64().unwrap();
    assert!(prepare >= 350, "preparation was not measured: {event}");
    assert!(run >= 90, "command was not measured: {event}");
    assert!(run < prepare, "preparation charged to command: {event}");
    assert!(
        duration >= run,
        "execution duration lost command time: {event}"
    );
    let shown = json_stdout(repo.arc(&wt).args(["show", "receipts", "--json"]));
    let status = json_stdout(repo.arc(&wt).args(["status", "receipts"]));
    for field in FIELDS.iter().copied().chain(["duration_ms"]) {
        assert_eq!(
            shown["verifications"][0][field], event[field],
            "show: {field}"
        );
        assert_eq!(status["gates"][0][field], event[field], "status: {field}");
    }
}

/// Claim: slot admission is measured separately from the command. Rejects a
/// receipt that includes a contended use lock in run_ms or drops the wait.
#[test]
fn command_timing_excludes_a_slot_wait() {
    let repo = Repo::new();
    declare(&repo, "sleep 0.1", "[slots]\nsize = 1\n");
    let (_, wt) = begin(&repo, true);
    let name = wt.file_name().unwrap().to_str().unwrap();
    let lock_path = wt.with_file_name(format!("{name}.use.lock"));
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    // SAFETY: the fixture owns the lock descriptor and the inotify instance.
    let notifications = unsafe {
        assert_eq!(libc::flock(lock.as_raw_fd(), libc::LOCK_EX), 0);
        let fd = libc::inotify_init1(libc::IN_CLOEXEC);
        assert!(fd >= 0);
        let name = std::ffi::CString::new(lock_path.to_str().unwrap()).unwrap();
        assert!(libc::inotify_add_watch(fd, name.as_ptr(), libc::IN_OPEN) >= 0);
        fs::File::from_raw_fd(fd)
    };
    let mut command = repo.arc(&wt);
    command.args(["verify", "--gate", "unit"]);
    let verifier = thread::spawn(move || command.output().unwrap());
    let mut ready = libc::pollfd {
        fd: notifications.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll borrows one initialized pollfd, bounded to ten seconds.
    let observed = unsafe { libc::poll(&mut ready, 1, 10_000) };
    thread::sleep(Duration::from_millis(350));
    drop(lock);
    let output = verifier.join().unwrap();
    assert_eq!(observed, 1, "verifier never opened the contended slot lock");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown = json_stdout(repo.arc(&wt).args(["show", "receipts", "--json"]));
    let timing = &shown["verifications"][0];
    let wait = timing["wait_ms"].as_u64().unwrap();
    let run = timing["run_ms"].as_u64().unwrap();
    let duration = timing["duration_ms"].as_u64().unwrap();
    assert!(wait >= 300, "slot wait was not measured: {timing}");
    assert!(
        wait + run <= duration + 10,
        "slot wait charged to command: {timing}"
    );
}

/// Claim: CPU covers waited and orphaned descendants both in and outside slots,
/// with host observations sampled for the command. Rejects timing the holder or
/// only the leader, which miss the two descendants' CPU work.
#[test]
fn command_cpu_includes_waited_and_adopted_descendants() {
    let script = r#"import os,time
def work():
    start=time.process_time()
    while time.process_time()-start < 0.18: pass
pid=os.fork()
if pid == 0:
    work()
    os._exit(0)
os.waitpid(pid,0)
r,w=os.pipe()
pid=os.fork()
if pid == 0:
    if os.fork() == 0:
        os.close(r)
        work()
        os._exit(0)
    os._exit(0)
os.close(w)
os.waitpid(pid,0)
os.read(r,1)
time.sleep(0.05)
"#;
    for slot in [false, true] {
        let repo = Repo::new();
        let command = format!("python3 - <<'PY'\n{script}PY");
        declare(
            &repo,
            &command,
            if slot { "[slots]\nsize = 1\n" } else { "" },
        );
        let (_, wt) = begin(&repo, slot);
        let event = receipt(&repo, &wt);
        let cpu = event["cpu_user_ms"].as_u64().unwrap() + event["cpu_system_ms"].as_u64().unwrap();
        assert!(cpu >= 320, "descendant CPU missing (slot={slot}): {event}");
        assert!(event["core_count"].as_u64().unwrap() > 0, "{event}");
        let load = event["load_average"].as_array().unwrap();
        assert_eq!(load.len(), 3);
        assert!(load.iter().all(|value| value.as_f64().unwrap() >= 0.0));
    }
}

/// Claim: incomplete descendant accounting supplies no CPU figure. Rejects
/// reporting partial leader usage when cleanup kills an unjoined child.
#[test]
fn cleanup_with_live_descendants_has_no_cpu_figure() {
    for slot in [false, true] {
        let repo = Repo::new();
        declare(
            &repo,
            "sleep 10 >/dev/null 2>&1 & exit 0",
            if slot { "[slots]\nsize = 1\n" } else { "" },
        );
        let (_, wt) = begin(&repo, slot);
        let event = receipt(&repo, &wt);
        for field in ["cpu_user_ms", "cpu_system_ms"] {
            assert!(event.get(field).is_none(), "partial CPU recorded: {event}");
        }
        assert!(event["run_ms"].is_u64(), "{event}");
    }
}

/// Claim: command time extends through output drain after its leader exits.
/// Rejects stopping the run clock at the leader's exit while a child holds stdout.
#[test]
fn command_timing_includes_output_drain() {
    let repo = Repo::new();
    declare(&repo, "sleep 0.3 & exit 0", "");
    let (_, wt) = begin(&repo, false);
    let event = receipt(&repo, &wt);
    assert!(
        event["run_ms"].as_u64().unwrap() >= 250,
        "output drain missing: {event}"
    );
}

/// Claim: timeout receipts omit incomplete CPU, and attestations invent no
/// observations. Rejects partial CPU at timeout or measured-looking attestations.
#[test]
fn timeouts_and_attestations_do_not_invent_observations() {
    let repo = Repo::new();
    declare(&repo, "sleep 10", "");
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.unit]\ncommand = 'sleep 10'\ntimeout = '1s'\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: bound receipt fixture"]);
    let (_, wt) = begin(&repo, false);
    repo.arc(&wt)
        .args(["verify", "--gate", "unit"])
        .assert()
        .code(1);
    let shown = json_stdout(repo.arc(&wt).args(["show", "receipts", "--json"]));
    let timed = &shown["verifications"][0];
    assert_eq!(timed["timed_out"], true, "{shown}");
    assert!(timed["run_ms"].as_u64().unwrap() >= 900, "{shown}");
    for field in ["cpu_user_ms", "cpu_system_ms"] {
        assert!(
            timed.get(field).is_none(),
            "partial CPU at timeout: {shown}"
        );
    }
    let head = repo.head(&wt);
    repo.arc(&wt)
        .args([
            "verify",
            "--gate",
            "unit",
            "--attest",
            "--result",
            "pass",
            "--tested-revision",
            &head,
            "--execution-host",
            "fixture",
            "--runner",
            "external",
        ])
        .assert()
        .success();
    let shown = json_stdout(repo.arc(&wt).args(["show", "receipts", "--json"]));
    for field in FIELDS.iter().copied().chain(["duration_ms"]) {
        assert!(
            shown["verifications"][1].get(field).is_none(),
            "attestation invented {field}: {shown}"
        );
    }
}

/// Claim: stats charges command time and older receipts retain their existing
/// totals and bytes on replay. Rejects using total execution time for a measured
/// command, inventing command timing for old events, or rewriting them on read.
#[test]
fn stats_uses_command_time_and_legacy_events_replay_unchanged() {
    let repo = Repo::new();
    declare(&repo, "true", "");
    let (id, wt) = begin(&repo, false);
    receipt(&repo, &wt);
    rewrite_event(&repo, &id, "verification-recorded", |event| {
        event["duration_ms"] = 19_000.into();
        event["run_ms"] = 2_500.into();
    });
    let report = json_stdout(
        repo.arc(&wt)
            .args(["stats", "--change", "receipts", "--json"]),
    );
    assert_eq!(report["schema"], "arc-stats/2");
    assert_eq!(report["changes"][0]["gate_seconds"]["unit"], 2, "{report}");
    rewrite_event(&repo, &id, "verification-recorded", |event| {
        for field in FIELDS {
            event.as_object_mut().unwrap().remove(*field);
        }
    });
    let bytes = || {
        let mut files = fs::read_dir(event_dir(&repo, &id))
            .unwrap()
            .map(|file| file.unwrap().path())
            .collect::<Vec<_>>();
        files.sort();
        files
            .iter()
            .map(|file| fs::read(file).unwrap())
            .collect::<Vec<_>>()
    };
    let before = bytes();
    let shown = json_stdout(repo.arc(&wt).args(["show", "receipts", "--json"]));
    for field in FIELDS {
        assert!(shown["verifications"][0].get(field).is_none(), "{shown}");
    }
    let exported = stdout(repo.arc(&wt).args([
        "events",
        "--change",
        "receipts",
        "--type",
        "verification-recorded",
    ]));
    let legacy: serde_json::Value = serde_json::from_str(exported.trim()).unwrap();
    assert_eq!(legacy["duration_ms"], 19_000);
    for field in FIELDS {
        assert!(
            legacy.get(field).is_none(),
            "legacy serialization invented {field}: {legacy}"
        );
    }
    let report = json_stdout(
        repo.arc(&wt)
            .args(["stats", "--change", "receipts", "--json"]),
    );
    assert_eq!(report["changes"][0]["gate_seconds"]["unit"], 19, "{report}");
    assert_eq!(bytes(), before, "reading modified legacy events");
}
