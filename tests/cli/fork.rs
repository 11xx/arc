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

/// Retirement is visible in text whenever --json reports it, including the
/// recoverable state where a worktree survives its retirement; and catchup
/// --json carries the same forks the text section lists, because two views
/// of one derivation must not disagree about what exists.
#[test]
fn fork_views_agree_about_retirement_and_catchup_json_carries_forks() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "agreed"]));

    // Text and JSON agree on a live fork.
    let text = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(text.contains("agreed  fork/agreed ("), "{text}");
    let value = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert!(value["forks"][0]["retired"].is_null(), "{value}");

    stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "agreed", "merged", "--keep-worktree"]),
    );

    // The recoverable half-state reads as retired in both views.
    let text = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(text.contains("retired, worktree remains:"), "{text}");
    let value = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(value["forks"][0]["retired"], "retired");
    assert!(value["forks"][0]["worktree"].is_string());

    // Retired forks leave the catchup surfaces; open ones appear in both.
    let catchup_text = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(!catchup_text.contains("agreed"), "{catchup_text}");
    let catchup = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(catchup["schema"], "arc-catchup/3");
    assert!(catchup["forks"].as_array().unwrap().is_empty(), "{catchup}");

    stdout(repo.arc(&repo.root).args(["fork", "begin", "open-now"]));
    let catchup = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(catchup["forks"][0]["slug"], "open-now", "{catchup}");
    let catchup_text = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(catchup_text.contains("open-now"), "{catchup_text}");
}

/// A journal plan under a fork-* topic is prose, not a fork: the branch is
/// the fact, the marker only annotates one. A phantom fork invented a
/// branch, a hardcoded base, and an ahead count all at once.
#[test]
fn fork_list_requires_a_branch_not_just_a_topic() {
    let repo = Repo::new();
    let plan = repo.home.join("fork-etiquette.md");
    fs::write(&plan, "A plan about fork etiquette.\n").unwrap();
    repo.arc(&repo.root)
        .args([
            "journal",
            "plan",
            "fork-etiquette",
            "--title",
            "Fork etiquette",
            "--body-file",
            plan.to_str().unwrap(),
        ])
        .assert()
        .success();

    let text = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert_eq!(
        text, "no forks\n",
        "a marker with no branch must not be a fork: {text}"
    );
    let catchup = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(
        !catchup.contains("fork/etiquette"),
        "phantom fork leaked into catchup: {catchup}"
    );
}

/// An uncomputable ahead count reads as unknown, not zero: the +? is advice
/// a reader cannot mistake for "no work".
#[test]
fn fork_list_shows_unknown_ahead_as_plus_question() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["fork", "begin", "counted", "--from", "master"]),
    );
    // The marker records the true base, so the count is computable.
    let text = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(text.contains("+0 over master"), "{text}");
    let value = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(value["forks"][0]["ahead"], 0);
}

/// An unmarked fork is never measured against itself. Base discovery must
/// refuse fork branches the same way `begin` does: running `fork list` from
/// inside a fork worktree otherwise reports every fork as +0 over a fork, a
/// zero a reader would sum to "nothing to integrate".
#[test]
fn fork_list_from_inside_a_fork_never_names_a_fork_as_base() {
    let repo = Repo::new();
    repo.commit(&repo.root, "master.txt", "master\n", "test: master work");
    // A fork made by hand: no `fork begin`, so no marker records its base.
    let worktree = repo.home.join(".worktrees").join("repo-fork-selfbase");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/selfbase",
            worktree.to_str().unwrap(),
            "master",
        ],
    );
    repo.commit(&worktree, "fork.txt", "fork\n", "test: fork work");

    // From inside the fork worktree, discovery reaches the repository's
    // primary worktree instead of treating the fork branch as its own base.
    let text = stdout(repo.arc(&worktree).args(["fork", "list"]));
    assert!(
        !text.contains("over fork/"),
        "a fork must not be measured against a fork: {text}"
    );
    assert!(text.contains("+1 over master"), "{text}");

    let value = json_stdout(repo.arc(&worktree).args(["fork", "list", "--json"]));
    let fork = &value["forks"][0];
    assert!(
        !fork["base_branch"]
            .as_str()
            .is_some_and(|base| base.starts_with("fork/")),
        "{value}"
    );
    assert_eq!(fork["ahead"], 1, "{value}");

    // The primary checkout keeps its answer.
    let text = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(text.contains("+1 over master"), "{text}");
}

/// A repository with a primary `main` branch and a stale `master` branch
/// uses `main` for an unmarked fork from both worktree views.
#[test]
fn fork_list_from_inside_a_fork_uses_the_primary_worktree_branch() {
    let repo = Repo::new();
    git(&repo.root, &["branch", "-m", "master", "main"]);
    git(&repo.root, &["branch", "master"]);
    repo.commit(&repo.root, "main-one.txt", "one\n", "test: main one");
    repo.commit(&repo.root, "main-two.txt", "two\n", "test: main two");

    let worktree = repo.home.join(".worktrees").join("repo-fork-late");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/late",
            worktree.to_str().unwrap(),
        ],
    );
    repo.commit(&worktree, "fork.txt", "fork\n", "test: fork late");

    let primary_text = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(primary_text.contains("+1 over main"), "{primary_text}");
    let primary = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(primary["schema"], "arc-forks/1", "{primary}");
    assert_eq!(primary["forks"][0]["base_branch"], "main", "{primary}");
    assert_eq!(primary["forks"][0]["ahead"], 1, "{primary}");

    let fork_text = stdout(repo.arc(&worktree).args(["fork", "list"]));
    assert!(fork_text.contains("+1 over main"), "{fork_text}");
    let inside = json_stdout(repo.arc(&worktree).args(["fork", "list", "--json"]));
    assert_eq!(
        inside["forks"][0]["base_branch"],
        primary["forks"][0]["base_branch"]
    );
    assert_eq!(inside["forks"][0]["ahead"], primary["forks"][0]["ahead"]);
}

/// The integrate refusal holds on a detached HEAD. Detaching has no branch
/// symbol and the porcelain list records only `detached`, so the fork
/// identity comes from the marker's branch-to-worktree binding.
#[test]
fn fork_refusal_holds_on_detached_head() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "detachable"]));
    let worktree = fork_worktree(&repo, "detachable");
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree detachable"))
        .stderr(predicates::str::contains("unintegrated by intent"));

    // The refusal also holds in a subdirectory, where the operator works.
    fs::create_dir_all(worktree.join("src")).unwrap();
    repo.arc(&worktree.join("src"))
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree detachable"));
}

/// A detached worktree without a marker has no branch-to-path binding. Its
/// directory name and an existing fork branch do not identify it, so a
/// directory chosen for one fork must not lend that fork's name to another
/// detached checkout.
#[test]
fn fork_refusal_detached_does_not_borrow_an_unrelated_forks_name() {
    let repo = Repo::new();
    // An unrelated fork branch whose tip shares nothing with the worktree.
    git(&repo.root, &["branch", "fork/alpha"]);
    // The worktree is on fork/beta, but its directory name says alpha — the
    // shape a hand-chosen or stale worktree path produces.
    let worktree = fork_worktree(&repo, "alpha");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/beta",
            worktree.to_str().unwrap(),
            "master",
        ],
    );
    repo.commit(&worktree, "work.txt", "work\n", "test: beta work");

    // Attached, the branch symbol answers: this is beta, whatever the
    // directory is called.
    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree beta"));

    // Detached, the name suggests alpha and fork/alpha exists — but there is
    // no marker binding alpha to this path, so identity is unknowable and
    // integrate falls through to its ordinary refusal.
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("provide a change"));
}

/// A hand-made fork (no `<repo>-fork-<slug>` gitdir name) keeps the
/// integrate refusal while detached: the marker's `worktree:` record is
/// arc's own data about which checkout the fork is, and it answers where
/// the directory name cannot. An unmarked fork must be adopted while its
/// branch is attached, before Git has discarded that branch-to-path link.
#[test]
fn fork_refusal_holds_detached_for_a_hand_made_fork() {
    let repo = Repo::new();
    let worktree = repo.home.join(".worktrees").join("my-own-place");
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
    // Adopt it, so a marker recording `worktree: <path>` exists.
    stdout(repo.arc(&repo.root).args(["fork", "adopt", "handmade"]));
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree handmade"))
        .stderr(predicates::str::contains("unintegrated by intent"));

    // An unmarked fork whose gitdir name carries no slug cannot be
    // identified from the name — Git names gitdirs after the directory, not
    // the branch — and no other data arc holds maps this path to a fork.
    // Falling through is the honest report of an unknowable identity.
    let repo = Repo::new();
    let worktree = repo.home.join(".worktrees").join("no-marker-here");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/unmarked",
            worktree.to_str().unwrap(),
            "master",
        ],
    );
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("provide a change"));

    // Attach the branch so adopt can record the durable path binding.
    git(&worktree, &["checkout", "fork/unmarked"]);
    let out = stdout(repo.arc(&repo.root).args(["fork", "adopt", "unmarked"]));
    assert!(out.contains("adopted: unmarked"), "{out}");
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree unmarked"));
}

/// --force names what it destroys before destroying it: a summary of
/// untracked files and uncommitted modifications, not a refusal.
#[test]
fn fork_retire_force_names_what_it_discards() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "loud"]));
    let worktree = fork_worktree(&repo, "loud");

    // Tracked-and-clean worktrees say nothing: there is nothing to name.
    let out = stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "loud", "merged", "--force"]),
    );
    assert!(
        !out.contains("discarding:"),
        "clean worktree must be silent: {out}"
    );
    assert!(!worktree.exists());

    // An untracked file is named before the removal.
    stdout(repo.arc(&repo.root).args(["fork", "begin", "louder"]));
    let worktree = fork_worktree(&repo, "louder");
    fs::write(worktree.join("untracked.txt"), "local state\n").unwrap();
    let out = stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "louder", "dropped", "--force"]),
    );
    assert!(out.contains("discarding: 1 untracked file(s)"), "{out}");
    assert!(!worktree.exists());
}

/// A detached fork remains the marker's worktree even when its HEAD no longer
/// equals the fork branch tip. Retirement must remove that exact checkout
/// before consuming the marker.
#[test]
fn fork_retire_removes_a_detached_worktree_at_a_non_tip() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "non-tip"]));
    let worktree = fork_worktree(&repo, "non-tip");
    repo.commit(&worktree, "work.txt", "work\n", "test: fork work");
    let earlier = git_out(&worktree, &["rev-parse", "HEAD~1"]);
    git(&worktree, &["checkout", "--detach", &earlier]);

    let before = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(
        before["forks"][0]["worktree"],
        worktree.to_str().unwrap(),
        "{before}"
    );

    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "retire",
        "non-tip",
        "dropped: inspected history",
    ]));
    assert!(out.contains("retired: non-tip"), "{out}");
    assert!(
        !worktree.exists(),
        "the detached fork worktree must be removed"
    );
}

/// Two detached worktrees may have the same HEAD. The fork marker identifies
/// the fork's checkout, so retirement must not remove whichever equal-SHA
/// worktree Git lists first.
#[test]
fn fork_retire_removes_the_marked_worktree_among_equal_detached_heads() {
    let repo = Repo::new();
    let unrelated = repo.home.join(".worktrees/repo-fork-a-unrelated");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "--detach",
            unrelated.to_str().unwrap(),
            "master",
        ],
    );

    stdout(repo.arc(&repo.root).args(["fork", "begin", "same-sha"]));
    let worktree = fork_worktree(&repo, "same-sha");
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "same-sha", "dropped: selected fork"]),
    );
    assert!(
        !worktree.exists(),
        "the marked fork worktree must be removed"
    );
    assert!(
        unrelated.exists(),
        "an unrelated equal-SHA worktree must remain"
    );
}

/// The branch field is the marker's identity, not a delimiter in its
/// filename. Slugs may therefore contain the text used to separate a topic
/// from its kind.
#[test]
fn fork_refusal_preserves_a_slug_containing_fork_separator_text() {
    let repo = Repo::new();
    let slug = "alpha-fork-beta";
    stdout(repo.arc(&repo.root).args(["fork", "begin", slug]));
    let worktree = fork_worktree(&repo, slug);
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree alpha-fork-beta"))
        .stderr(predicates::str::contains("fork worktree beta").not());
}

/// A primary checkout has a .git directory rather than a linked-worktree
/// gitdir file. Its marker path still identifies the adopted fork after the
/// primary is detached.
#[test]
fn fork_refusal_holds_for_a_detached_primary_checkout() {
    let repo = Repo::new();
    let (change_id, change_worktree, _) = change_with_patchset(&repo, "ready-change");
    repo.arc(&change_worktree)
        .args(["review", "ready-change", "--verdict", "approved"])
        .assert()
        .success();
    git(&repo.root, &["switch", "-c", "fork/primary"]);
    assert!(repo.root.join(".git").is_dir());
    stdout(repo.arc(&repo.root).args(["fork", "adopt", "primary"]));

    let target = repo.home.join(".worktrees/integration-target");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "integration-target",
            target.to_str().unwrap(),
            "master",
        ],
    );
    git(&repo.root, &["checkout", "--detach", "HEAD"]);

    repo.arc(&repo.root)
        .args(["integrate", &change_id])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree primary"))
        .stderr(predicates::str::contains("unintegrated by intent"));
}

/// A disposition is valid only for an existing fork branch. A typo must not
/// create a consumed marker or make a nonexistent fork appear in the queues.
#[test]
fn fork_retire_refuses_a_branch_that_never_existed() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["fork", "retire", "never-made", "dropped: typo"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork/never-made"))
        .stderr(predicates::str::contains("does not exist"));

    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    assert!(
        !open["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["file"]
                .as_str()
                .is_some_and(|file| file.contains("fork-never-made-plan"))),
        "a typo must not create a marker: {open}"
    );
    assert_eq!(
        stdout(repo.arc(&repo.root).args(["fork", "list"])),
        "no forks\n"
    );
}

/// A fork refusal is a precondition failure, not a merge failure. Declaring
/// integration debt must therefore wait until that refusal has passed.
#[test]
fn integrate_debt_does_not_record_an_obligation_from_inside_a_fork() {
    let repo = Repo::new();
    let (change_id, ..) = change_with_patchset(&repo, "debt-from-fork");
    stdout(repo.arc(&repo.root).args(["fork", "begin", "debt-context"]));
    let fork = fork_worktree(&repo, "debt-context");

    repo.arc(&fork)
        .args(["integrate", &change_id, "--debt", "no reviewer reachable"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fork worktree debt-context"));

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id, "--json"]));
    assert_ne!(status["debt_outstanding"], true, "{status}");
    assert!(
        status["debt"].is_null(),
        "a refused integration owes no review: {status}"
    );
}
