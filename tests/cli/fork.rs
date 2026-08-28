use super::common::*;

fn fork_worktree(repo: &Repo, slug: &str) -> PathBuf {
    repo.home
        .join(".worktrees")
        .join(format!("repo-fork-{slug}"))
}

/// A fork is a worktree on a fork/<slug> branch with a journaled marker,
/// outside the change lifecycle: catchup lists it, integrate refuses inside
/// it, and the contract is printed where the operator reads it.
#[test]
fn fork_begin_creates_worktree_marker_and_refuses_integration() {
    let repo = Repo::new();
    let out = stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    assert!(out.contains("branch: fork/demo"), "{out}");
    assert!(out.contains("Fork contract:"), "{out}");

    let worktree = fork_worktree(&repo, "demo");
    assert!(worktree.is_dir(), "worktree must exist");
    assert_eq!(
        git_out(&repo.root, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        2
    );

    // The marker is a journaled plan under fork-demo, listed by the open
    // queue: the fork is visible without the ledger claiming work.
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(
        open["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["file"]
                .as_str()
                .is_some_and(|file| file.contains("fork-demo-plan"))),
        "{open}"
    );

    // catchup lists the fork.
    let catchup = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(catchup.contains("forks (1):"), "CATCHUP:\n{catchup}");
    assert!(catchup.contains("demo  fork/demo"), "CATCHUP:\n{catchup}");

    // integrate refuses inside the fork worktree, naming the contract.
    repo.arc(&worktree)
        .args(["integrate", "anything"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree demo"))
        .stderr(predicates::str::contains("unintegrated by intent"));

    // A second begin on the same slug points at the existing branch.
    repo.arc(&repo.root)
        .args(["fork", "begin", "demo"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already exists"));
}

/// The lifecycle closes: retirement records the disposition, removes the
/// worktree, keeps the branch, and refuses to retire twice.
#[test]
fn fork_retire_records_outcome_removes_worktree_and_keeps_the_branch() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "shortlived"]));
    let worktree = fork_worktree(&repo, "shortlived");

    repo.commit(&worktree, "work.txt", "work\n", "test: fork work");
    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "retire",
        "shortlived",
        "merged: it was good enough",
    ]));
    assert!(out.contains("retired: shortlived"), "{out}");
    assert!(out.contains("branch kept: fork/shortlived"), "{out}");
    assert!(!worktree.exists(), "worktree must be removed");
    // The branch survives: the commits are the operator's to keep or delete.
    assert!(
        git_out(&repo.root, &["branch", "--list", "fork/shortlived"]).contains("fork/shortlived")
    );

    repo.arc(&repo.root)
        .args(["fork", "retire", "shortlived", "again"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already retired"));

    // Retired forks leave the catchup section but stay in fork list.
    let catchup = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(!catchup.contains("forks (1):"), "{catchup}");
    let forks = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(forks["forks"][0]["slug"], "shortlived");
    assert_eq!(forks["forks"][0]["retired"], "retired");
}

/// A fork the operator made by hand is adoptable rather than invisible.
#[test]
fn fork_adopts_a_hand_made_fork_worktree() {
    let repo = Repo::new();
    let worktree = fork_worktree(&repo, "handmade");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/handmade",
            worktree.to_str().unwrap(),
            "master",
        ],
    );

    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "adopt",
        "handmade",
        "--intent",
        "operator's own branch",
    ]));
    assert!(out.contains("adopted: handmade"), "{out}");
    let catchup = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(catchup.contains("handmade"), "{catchup}");

    // Adopting twice reports rather than duplicates.
    let again = stdout(
        repo.arc(&repo.root)
            .arg("fork")
            .arg("adopt")
            .arg("handmade"),
    );
    assert!(again.contains("already journaled"), "{again}");
}

/// A fork of a fork is refused: the base must be an integrated branch, or
/// the fork chain stops being a chain of records.
#[test]
fn fork_begin_refuses_a_fork_base() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "outer"]));
    repo.arc(&repo.root)
        .args(["fork", "begin", "inner", "--from", "fork/outer"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is itself a fork"));
}

/// Retiring a fork that was never journaled must not leave the fresh marker
/// sitting in the open queue: a retired fork is a record, not live work.
#[test]
fn fork_retire_of_unmarked_fork_consumes_its_marker() {
    let repo = Repo::new();
    let worktree = fork_worktree(&repo, "ghost");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/ghost",
            worktree.to_str().unwrap(),
            "master",
        ],
    );

    stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "ghost", "dropped: never journaled"]),
    );

    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(
        !open["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["file"]
                .as_str()
                .is_some_and(|file| file.contains("fork-ghost-plan"))),
        "retired fork must not be live: {open}"
    );
    let forks = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(forks["forks"][0]["retired"], "retired");
    assert!(forks["forks"][0].get("worktree").is_none());
}

/// Every printed way out of a fork names a command that exists. The refusal
/// and the begin-collision advice are hand-formatted strings, so the actual
/// clap spellings are what they must be checked against — the flag-shaped
/// forms this replaces were believed correct by three surfaces at once.
#[test]
fn fork_advice_names_commands_clap_actually_defines() {
    // Refuse a nonexistent subcommand shape through the real parser: clap
    // itself is the authority on what the commands are called.
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["fork", "demo", "--retire", "dropped"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("unrecognized subcommand"));

    // The refusal text names the positional form.
    stdout(repo.arc(&repo.root).args(["fork", "begin", "named"]));
    let worktree = fork_worktree(&repo, "named");
    repo.arc(&worktree)
        .args(["integrate", "anything"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("arc fork retire named <outcome>"));

    // The slug-collision advice names the adopt subcommand.
    repo.arc(&repo.root)
        .args(["fork", "begin", "named"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("arc fork adopt named"));

    // Retire without an outcome is a usage error naming the real command
    // shape, which is the drift guard: the usage line comes from clap.
    repo.arc(&repo.root)
        .args(["fork", "retire", "named"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("Usage: arc fork retire"));
}

/// Retirement must not claim a disposition the disk has not acted on: a
/// worktree Git refuses to remove (untracked files) leaves nothing
/// recorded, so the retry is ordinary. `--force` is the operator's
/// deliberate discard, which goes through — and only then does the marker
/// get consumed.
#[test]
fn fork_retire_with_untracked_files_records_nothing_until_the_worktree_moves() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "dirty"]));
    let worktree = fork_worktree(&repo, "dirty");
    fs::write(worktree.join("untracked.txt"), "operator's local state\n").unwrap();

    // The removal fails and nothing is recorded: not consumed, not retired.
    repo.arc(&repo.root)
        .args(["fork", "retire", "dirty", "merged: too soon"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot remove"))
        .stderr(predicates::str::contains("--force"));
    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(
        open["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["file"]
                .as_str()
                .is_some_and(|file| file.contains("fork-dirty-plan"))),
        "the marker must still be live: {open}"
    );

    // The operator forces the discard; the record follows the disk.
    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "retire",
        "dirty",
        "merged: for real",
        "--force",
    ]));
    assert!(out.contains("retired: dirty"), "{out}");
    assert!(!worktree.exists(), "worktree must be gone after --force");
    let forks = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(forks["forks"][0]["retired"], "retired");
    let untracked = fs::read_to_string(worktree.join("untracked.txt"));
    assert!(untracked.is_err(), "forced discard removes the files");
}

/// A retire that ran with --keep-worktree can be finished later: the record
/// stands, and removing the leftover worktree is not a second decision.
#[test]
fn fork_retire_keep_worktree_leaves_a_finishable_leftover() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "kept"]));
    let worktree = fork_worktree(&repo, "kept");

    stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "kept", "merged", "--keep-worktree"]),
    );
    assert!(worktree.exists());

    // Re-retiring still refuses the second decision on stderr, but finishes
    // the worktree removal on stdout: the removal is finishing the first
    // retire, not a second one.
    let mut recommit = repo.arc(&repo.root);
    recommit
        .args(["fork", "retire", "kept", "again"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already retired"))
        .stdout(predicates::str::contains("worktree removed"));
    assert!(!worktree.exists());
}
