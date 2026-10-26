#![cfg(target_os = "linux")]

use crate::common::*;

/// Claim: retirement rechecks the missing anchor before it records the
/// binding. Rejects: a command hiding a project that returned while it waited
/// for the journal's events lock.
#[test]
fn retirement_refuses_an_anchor_recreated_before_its_binding() {
    let repo = Repo::new();
    let anchor = repo.home.join("vanished");
    fs::create_dir_all(&anchor).unwrap();
    git(&anchor, &["init", "-b", "master"]);
    repo.arc(&anchor)
        .args([
            "journal",
            "note",
            "retained",
            "--kind",
            "note",
            "--body-file",
            "-",
        ])
        .write_stdin("retained history\n")
        .assert()
        .success();
    let journal = PathBuf::from(stdout(repo.arc(&anchor).args(["journal", "dir"])).trim());
    fs::remove_dir_all(&anchor).unwrap();
    let before = fs::read(journal.join("bindings.jsonl")).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(journal.join(".locks/events.lock"))
        .unwrap();
    lock.lock().unwrap();
    let journal_arg = journal.display().to_string();
    let mut child = spawn_arc_keeping_stderr(
        &repo,
        &repo.root,
        &[
            "workspace",
            "retire",
            &journal_arg,
            "--reason",
            "the anchor was removed",
        ],
        "session-a",
    );
    let expected = journal.join(".locks/events.lock");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let descriptors = PathBuf::from(format!("/proc/{}/fd", child.id()));
        let waiting = fs::read_dir(descriptors).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| fs::read_link(entry.path()).is_ok_and(|target| target == expected))
        });
        if waiting {
            break;
        }
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            lock.unlock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
            panic!("retirement never reached the held events lock");
        }
        thread::sleep(Duration::from_millis(5));
    }
    fs::create_dir_all(&anchor).unwrap();
    lock.unlock().unwrap();
    let status = wait_for_exit(&mut child);
    let stderr = child_stderr(&mut child);
    assert!(
        !status.success(),
        "retirement succeeded after the anchor returned: {stderr}"
    );
    assert!(
        stderr.contains("its registration cannot be retired"),
        "{stderr}"
    );
    assert_eq!(fs::read(journal.join("bindings.jsonl")).unwrap(), before);
}
