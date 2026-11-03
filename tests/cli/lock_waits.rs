//! Worktrees of one repository share a ledger, so a write regularly finds a
//! lock a sibling holds for its own short critical section. These tests hold
//! a lock the way a sibling would, pause arc at the moment it finds the lock
//! held, and keep holding it past one second, longer than any ordinary
//! append takes, so a wait that gives up on a live sibling fails them.

use crate::absent_identity::journal_dir;
use crate::common::*;

/// Longer than an ordinary append holds a ledger lock.
const PAST_ONE_SECOND: Duration = Duration::from_millis(1_500);

/// A repository declaring the gate `check` as `command(repo)`.
fn declaring_gate(command: impl FnOnce(&Repo) -> String) -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        format!(
            "[gates.check]\ncommand = {}\n",
            serde_json::to_string(&command(&repo)).unwrap()
        ),
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare a gate"]);
    repo
}

fn snapshotted_change(repo: &Repo, slug: &str) -> PathBuf {
    repo.arc(&repo.root)
        .args(["begin", slug])
        .assert()
        .success();
    let wt = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(&wt, "feature", "one\n", "feat: fixture");
    repo.arc(&wt).args(["snapshot", slug]).assert().success();
    wt
}

fn check_green_at_head(repo: &Repo, wt: &Path) -> bool {
    let status = json_stdout(repo.arc(wt).args(["status", "--json"]));
    status["gates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|gate| gate["name"] == "check" && gate["green_at_head"] == true)
}

/// Resume a paused arc, keep the lock past one second, and release it: arc
/// must still be waiting when the lock comes free, and then succeed.
#[cfg(debug_assertions)]
fn outwait_sibling(mut child: Child, lock: fs::File, lock_name: &str) -> String {
    release_lock_wait(&mut child);
    thread::sleep(PAST_ONE_SECOND);
    let early = child.try_wait().unwrap();
    lock.unlock().unwrap();
    let status = wait_for_exit(&mut child);
    let stderr = child_stderr(&mut child);
    assert!(
        early.is_none(),
        "arc gave up on the sibling's {lock_name} lock within a second: {stderr}"
    );
    assert!(status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("waiting for {lock_name} lock")),
        "the wait names no lock: {stderr}"
    );
    stderr
}

/// Claim: a verification whose evidence append finds the repository events
/// lock held by a sibling waits for it and records the evidence. Rejects:
/// refusing with "repository events lock ... is busy" while a sibling holds
/// the lock for longer than a second.
#[test]
#[cfg(debug_assertions)]
fn a_sibling_holding_the_repository_events_lock_is_waited_for() {
    let repo = declaring_gate(|_| "true".into());
    let wt = snapshotted_change(&repo, "repository-sibling");
    let lock = hold_named_lock(&repo, "repository-events.lock");
    let child =
        spawn_arc_paused_on_lock(&repo, &wt, &["verify", "--all"], "repository-events", &[]);
    outwait_sibling(child, lock, "repository events");
    assert!(check_green_at_head(&repo, &wt));
}

/// Claim: a change transition held by a sibling is waited for. Rejects:
/// refusing a snapshot because another worktree's command holds the
/// change's transition lock for longer than a second.
#[test]
#[cfg(debug_assertions)]
fn a_sibling_holding_a_change_transition_lock_is_waited_for() {
    let repo = declaring_gate(|_| "true".into());
    let wt = snapshotted_change(&repo, "transition-sibling");
    repo.commit(&wt, "feature", "two\n", "feat: second");
    let change = stdout(repo.arc(&wt).args(["status", "--json"]));
    let change: serde_json::Value = serde_json::from_str(&change).unwrap();
    let change = change["change_id"].as_str().unwrap().to_string();
    let lock = hold_transition_lock(&repo, &change);
    let child = spawn_arc_paused_on_lock(&repo, &wt, &["snapshot"], &change, &[]);
    outwait_sibling(child, lock, "change transition");
}

/// A repository whose gate leaves `gate-ran` under the fixture home and then
/// runs until `relocked` appears beside it.
fn declaring_gate_held_open() -> (Repo, PathBuf, PathBuf) {
    let repo = declaring_gate(|repo| {
        format!(
            "touch '{}' && until [ -e '{}' ]; do sleep 0.05; done",
            repo.home.join("gate-ran").display(),
            repo.home.join("relocked").display()
        )
    });
    let (ran, relocked) = (repo.home.join("gate-ran"), repo.home.join("relocked"));
    (repo, ran, relocked)
}

#[cfg(debug_assertions)]
fn resume_lock_wait(child: &mut Child) {
    use std::io::Write;
    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(b"+").unwrap();
    stdin.flush().unwrap();
}

#[cfg(debug_assertions)]
fn await_lock_wait(child: &mut Child, stem: &str) {
    let marker = format!("arc test: import {stem}-contended");
    let stderr = child.stderr.as_mut().unwrap();
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        assert_eq!(
            stderr.read(&mut byte).unwrap(),
            1,
            "arc exited before reaching the {stem} lock again: {}",
            String::from_utf8_lossy(&line)
        );
        if byte[0] == b'\n' {
            if line == marker.as_bytes() {
                return;
            }
            line.clear();
        } else {
            line.push(byte[0]);
        }
    }
}

/// `arc verify --all` paused where its evidence publication finds the
/// ledger event-ID lock held, after its gate has run to completion. The run's
/// opening append is let through first, so the lock held at the pause is the
/// one guarding the gate's evidence.
#[cfg(debug_assertions)]
fn verify_paused_at_evidence(
    repo: &Repo,
    wt: &Path,
    ran: &Path,
    relocked: &Path,
    envs: &[(&str, &str)],
) -> (Child, fs::File) {
    let lock = hold_named_lock(repo, "event-ids.lock");
    let mut child = spawn_arc_paused_on_lock(repo, wt, &["verify", "--all"], "event-ids", envs);
    assert!(!ran.exists(), "the gate ran before the run was opened");
    lock.unlock().unwrap();
    resume_lock_wait(&mut child);
    let deadline = Instant::now() + HANG_BOUND;
    while !ran.exists() {
        assert!(Instant::now() < deadline, "the gate never started");
        assert!(
            child.try_wait().unwrap().is_none(),
            "arc exited before its gate ran"
        );
        thread::sleep(Duration::from_millis(20));
    }
    lock.lock().unwrap();
    fs::write(relocked, "").unwrap();
    await_lock_wait(&mut child, "event-ids");
    (child, lock)
}

/// Claim: a verification that ran its gate keeps the result through transient
/// contention on the ledger event-ID lock. Rejects: running the gate, then
/// refusing to record its evidence because `event-ids.lock` was busy for
/// longer than a second.
#[test]
#[cfg(debug_assertions)]
fn verification_evidence_outwaits_event_id_contention_after_its_gate_ran() {
    let (repo, ran, relocked) = declaring_gate_held_open();
    let wt = snapshotted_change(&repo, "event-id-sibling");
    let (child, lock) = verify_paused_at_evidence(&repo, &wt, &ran, &relocked, &[]);
    outwait_sibling(child, lock, "ledger event IDs");
    assert!(check_green_at_head(&repo, &wt));
}

/// Claim: when a sibling outlasts the wait after the gate ran, the refusal
/// names the lock, says to retry, and reports the observed result as not
/// recorded, and nothing is green. Rejects: losing the observed result
/// silently, or treating evidence that was never written as green.
#[test]
#[cfg(debug_assertions)]
fn unrecorded_verification_evidence_is_reported_and_not_green() {
    let (repo, ran, relocked) = declaring_gate_held_open();
    let wt = snapshotted_change(&repo, "event-id-holdout");
    let (mut child, lock) = verify_paused_at_evidence(
        &repo,
        &wt,
        &ran,
        &relocked,
        &[("ARC_TEST_LOCK_BUDGET_MS", "200")],
    );
    release_lock_wait(&mut child);
    let status = wait_for_exit(&mut child);
    let stderr = child_stderr(&mut child);
    drop(lock);
    assert!(!status.success(), "{stderr}");
    assert!(
        stderr.contains("ledger event IDs lock") && stderr.contains("retry the command"),
        "{stderr}"
    );
    assert!(
        stderr.contains("not recorded: gate check: Pass"),
        "the observed result was lost: {stderr}"
    );
    assert!(!check_green_at_head(&repo, &wt));
}

/// Claim: a journal write waits for a sibling's journal events lock.
/// Rejects: refusing a note because another worktree is appending to the
/// shared journal for longer than a second.
#[test]
#[cfg(debug_assertions)]
fn a_sibling_holding_the_journal_events_lock_is_waited_for() {
    let repo = Repo::new();
    let journal = journal_dir(&repo);
    let lock_dir = journal.join(".locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_dir.join("events.lock"))
        .unwrap();
    lock.lock().unwrap();
    let body = repo.home.join("note.md");
    fs::write(&body, "# Sibling\n").unwrap();
    let child = spawn_arc_paused_on_lock(
        &repo,
        &repo.root,
        &[
            "journal",
            "note",
            "sibling",
            "--body-file",
            body.to_str().unwrap(),
        ],
        "events",
        &[],
    );
    outwait_sibling(child, lock, "journal events");
}

/// Claim: operator policy publication waits a bounded time for its lock and
/// refuses naming it. Rejects: blocking without bound while another process
/// holds the publication lock.
#[test]
fn a_held_publication_lock_is_waited_for_within_a_bound() {
    let repo = Repo::new();
    let lock = hold_named_lock_at(&repo.root.join(".git/arc/import-publication.lock"));
    let policy = repo.home.join("policy.toml");
    fs::write(&policy, "[integration]\nshapes = ['ff-only']\n").unwrap();
    let mut child = spawn_arc_keeping_stderr_with_env(
        &repo,
        &repo.root,
        &["policy", "write", "--body-file", policy.to_str().unwrap()],
        &[("ARC_TEST_LOCK_BUDGET_MS", "200")],
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("policy publication waited without bound for its lock");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().unwrap();
    let stderr = child_stderr(&mut child);
    drop(lock);
    assert!(!status.success(), "{stderr}");
    assert!(
        stderr.contains("operator policy publication lock") && stderr.contains("retry the command"),
        "{stderr}"
    );
}

/// The wait budget an `integrate` of `slug` announces while `held`, the lock
/// named `lock_name`, keeps it waiting; once the lock comes free the
/// integration must succeed.
fn announced_integration_wait(repo: &Repo, slug: &str, held: fs::File, lock_name: &str) -> String {
    let mut child = spawn_arc_keeping_stderr(
        repo,
        &repo.root,
        &["integrate", "--shape", "no-ff", slug],
        "integrator",
    );
    let notice = format!("waiting for {lock_name} lock");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut seen = String::new();
    let budget = loop {
        let start = seen.len();
        if stderr.read_line(&mut seen).unwrap() == 0 {
            drop(held);
            wait_for_exit(&mut child);
            panic!("integrate {slug} gave up on the {lock_name} lock without waiting: {seen}");
        }
        if let Some((_, budget)) = seen[start..]
            .split_once(&notice)
            .and_then(|(_, rest)| rest.rsplit_once("(up to "))
        {
            break budget.trim_end().trim_end_matches(')').to_string();
        }
    };
    drop(held);
    stderr.read_to_string(&mut seen).unwrap();
    let status = wait_for_exit(&mut child);
    assert!(status.success(), "integrate {slug} exited {status}: {seen}");
    budget
}

/// Claim: an integration waits for the replica-state lock as long as for its
/// target lock. A sibling integration holds the replica-state lock across its
/// whole target-lock wait and merge, so a shorter wait gives up on a sibling
/// still inside its own budget. Rejects: refusing with "replica-state lock ...
/// is busy" before the target-lock budget has passed.
#[test]
fn an_integration_waits_for_the_replica_state_lock_as_long_as_for_its_target() {
    let repo = Repo::new();
    for slug in ["replica-wait", "target-wait"] {
        let (_, worktree, _) = change_with_patchset(&repo, slug);
        repo.arc(&worktree)
            .args(["review", slug, "--verdict", "approved"])
            .assert()
            .success();
    }
    let replica = announced_integration_wait(
        &repo,
        "replica-wait",
        hold_named_lock(&repo, "replica-state.lock"),
        "replica-state",
    );
    let target = announced_integration_wait(
        &repo,
        "target-wait",
        hold_target_lock(&repo, "master"),
        "integration target",
    );
    assert_eq!(
        replica, target,
        "the replica-state wait budget differs from the target-lock budget"
    );
}

fn hold_named_lock_at(path: &Path) -> fs::File {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    file
}
