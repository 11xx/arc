use super::common::*;

fn fork_worktree(repo: &Repo, slug: &str) -> PathBuf {
    repo.home
        .join(".worktrees")
        .join(format!("repo-fork-{slug}"))
}

/// A change whose recorded branch is a fork's. The boundary refuses to make
/// this state now, so the fixture is the record an arc without it wrote — the
/// same ledger a session upgrading arc already holds.
fn change_on_fork_branch(repo: &Repo, slug: &str, branch: &str) -> String {
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", slug])));
    rewrite_event(repo, &change_id, "change-opened", |event| {
        event["branch"] = serde_json::Value::String(branch.to_string());
    });
    change_id
}

/// A fork is a worktree on a fork/<slug> branch with a journaled marker,
/// outside the change lifecycle: catchup lists it, integrate refuses a change
/// on its branch, and the contract is printed where the operator reads it.
#[test]
fn fork_begin_creates_worktree_marker_and_refuses_integration() {
    let repo = Repo::new();
    let out = stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    assert!(out.contains("branch: fork/demo"), "{out}");
    assert!(out.contains("Fork contract:"), "{out}");
    assert!(
        out.contains("`arc integrate` refuses a change on a fork branch."),
        "{out}"
    );

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

    // Integration binds to the change, not to the checkout the caller
    // stands in: from the fork worktree, an unnamed integration is the
    // ordinary refusal to run without naming work.
    repo.arc(&worktree)
        .args(["integrate"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "provide a change or at least one --tag",
        ));

    // A second begin on the same slug points at the recorded fork.
    repo.arc(&repo.root)
        .args(["fork", "begin", "demo"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already recorded"))
        .stderr(predicates::str::contains("arc fork adopt demo"));
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
    let promoted = change_on_fork_branch(&repo, "promoted", "fork/named");
    repo.arc(&repo.root)
        .args(["integrate", &promoted])
        .assert()
        .code(15)
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

    // A retired fork leaves the live-fork listing. Its worktree does not
    // leave the disk, so the accounting keeps reporting it: a record that
    // says retired above a checkout that still costs space is exactly the
    // half-state where worktree cost goes to hide.
    let catchup_text = stdout(repo.arc(&repo.root).arg("catchup"));
    assert!(!catchup_text.contains("forks ("), "{catchup_text}");
    assert!(
        catchup_text.contains("fork worktrees: ") && catchup_text.contains("agreed"),
        "{catchup_text}"
    );
    let catchup = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(catchup["schema"], "arc-catchup/8");
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
    assert_eq!(primary["schema"], "arc-forks/2", "{primary}");
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

/// A detached checkout has no branch symbol, and the porcelain list records
/// only `detached`; the fork's identity comes from the marker's
/// branch-to-worktree binding instead. Integration no longer asks where the
/// caller stands, so the binding is read through `fork list`, which answers
/// from every directory the same way.
#[test]
fn fork_views_bind_a_detached_checkout_through_the_marker() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "detachable"]));
    let worktree = fork_worktree(&repo, "detachable");
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(listed["forks"][0]["worktree"], worktree.to_str().unwrap());

    // From a subdirectory, where the operator works, the answer is the same.
    fs::create_dir_all(worktree.join("src")).unwrap();
    let nested = json_stdout(
        repo.arc(&worktree.join("src"))
            .args(["fork", "list", "--json"]),
    );
    assert_eq!(nested["forks"][0]["worktree"], worktree.to_str().unwrap());
}

/// A detached worktree without a marker has no branch-to-path binding. Its
/// directory name and an existing fork branch do not identify it, so a
/// directory chosen for one fork must not lend that fork's name to another
/// detached checkout.
#[test]
fn a_detached_checkout_is_not_named_by_a_directory_or_an_unrelated_branch() {
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
    // directory is called, and alpha owns nothing.
    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    let owned = |slug: &str| {
        listed["forks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|fork| fork["slug"] == slug)
            .and_then(|fork| fork["worktree"].as_str().map(str::to_string))
    };
    assert_eq!(owned("beta"), Some(worktree.display().to_string()));
    assert_eq!(owned("alpha"), None);

    // Detached, the name suggests alpha and fork/alpha exists — but no
    // marker binds alpha to this path, so neither fork owns the checkout.
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    let detached = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert!(detached["forks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|fork| fork["worktree"].is_null()));
}

/// A detached checkout is no longer the fork once its branch is attached in
/// another worktree. Every caller must use the same branch-first answer: list
/// and retire resolve the branch to its attached worktree.
#[test]
fn fork_callers_agree_after_a_detached_branch_moves_worktrees() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "alpha"]));
    let detached = fork_worktree(&repo, "alpha");
    git(&detached, &["checkout", "--detach", "HEAD"]);

    let attached = repo.home.join(".worktrees/repo-fork-alpha-attached");
    fs::create_dir_all(attached.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &["worktree", "add", attached.to_str().unwrap(), "fork/alpha"],
    );

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(listed["forks"][0]["worktree"], attached.to_str().unwrap());

    stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "alpha", "dropped: moved"]),
    );
    assert!(
        !attached.exists(),
        "retire must remove the attached worktree"
    );
    assert!(
        detached.exists(),
        "retire must not remove the stale detached checkout"
    );
}

/// Git keeps a prunable worktree entry after its checkout is deleted outside
/// Git. Fork views must not turn that administrative record into a live path,
/// and adopting the branch afterwards records the branch alone rather than a
/// path that is not there.
#[test]
fn fork_list_and_adopt_ignore_a_prunable_deleted_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "vanished"]));
    let worktree = fork_worktree(&repo, "vanished");
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    fs::remove_dir_all(&worktree).unwrap();

    let inventory = git_out(&repo.root, &["worktree", "list", "--porcelain"]);
    assert!(inventory.contains(&format!("worktree {}", worktree.display())));
    assert!(inventory.lines().any(|line| line.starts_with("prunable ")));

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert!(listed["forks"][0]["worktree"].is_null(), "{listed}");
    assert!(
        listed["forks"][0].get("dirty_files").is_none(),
        "no checkout means no counts, not zero: {listed}"
    );
}

/// A marker is not a fork without the branch it names. Adopt must apply the
/// same branch identity rule as retire instead of treating the marker alone
/// as sufficient.
#[test]
fn fork_adopt_refuses_after_its_branch_is_deleted() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "gone"]));
    let worktree = fork_worktree(&repo, "gone");
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    git(&repo.root, &["branch", "-D", "fork/gone"]);

    repo.arc(&repo.root)
        .args(["fork", "adopt", "gone"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "branch \"fork/gone\" does not exist",
        ));
}

/// A hand-made fork (no `<repo>-fork-<slug>` gitdir name) is bound to its
/// checkout by the marker's `worktree:` record once it is adopted. An
/// unmarked fork whose branch is detached has no such record and its gitdir
/// name carries no slug, so no fork owns that checkout until the branch is
/// attached again and adopted.
#[test]
fn a_hand_made_fork_is_bound_by_its_marker_when_detached() {
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

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(listed["forks"][0]["worktree"], worktree.to_str().unwrap());

    // An unmarked fork whose gitdir name carries no slug cannot be
    // identified from the name — Git names gitdirs after the directory, not
    // the branch — and no other data arc holds maps this path to a fork.
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
    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert!(listed["forks"][0]["worktree"].is_null(), "{listed}");

    // Attach the branch so adopt can record the durable path binding.
    git(&worktree, &["checkout", "fork/unmarked"]);
    let out = stdout(repo.arc(&repo.root).args(["fork", "adopt", "unmarked"]));
    assert!(out.contains("adopted: unmarked"), "{out}");
    git(&worktree, &["checkout", "--detach", "HEAD"]);
    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(listed["forks"][0]["worktree"], worktree.to_str().unwrap());
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
    let change_id = change_on_fork_branch(&repo, "promoted", &format!("fork/{slug}"));

    repo.arc(&repo.root)
        .args(["integrate", &change_id])
        .assert()
        .code(15)
        .stderr(predicates::str::contains(
            "branch fork/alpha-fork-beta is fork alpha-fork-beta's work",
        ))
        .stderr(predicates::str::contains("is fork beta's work").not());
}

/// A primary checkout has a .git directory rather than a linked-worktree
/// gitdir file, and its marker path still identifies it as an adopted fork.
/// Integration reads the change rather than the checkout the caller stands
/// in, so an ordinary change integrates from inside that fork.
#[test]
fn an_ordinary_change_integrates_from_inside_an_adopted_primary_checkout() {
    let repo = Repo::new();
    // The change targets a branch of its own, so the primary checkout the
    // fork will occupy is not the one holding the target.
    git(&repo.root, &["switch", "-c", "integration-target"]);
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
            target.to_str().unwrap(),
            "integration-target",
        ],
    );
    git(&repo.root, &["checkout", "--detach", "HEAD"]);

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(listed["forks"][0]["worktree"], repo.root.to_str().unwrap());

    repo.arc(&repo.root)
        .args(["integrate", &change_id])
        .assert()
        .success();
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
        .stderr(predicates::str::contains(
            "no fork \"never-made\" is recorded",
        ))
        .stderr(predicates::str::contains("arc fork list"));

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
fn integrate_debt_does_not_record_an_obligation_for_a_fork_branch() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "debt-context"]));
    let change_id = change_on_fork_branch(&repo, "debt-from-fork", "fork/debt-context");

    repo.arc(&repo.root)
        .args(["integrate", &change_id, "--debt", "no reviewer reachable"])
        .assert()
        .code(15)
        .stderr(predicates::str::contains(
            "branch fork/debt-context is fork debt-context's work",
        ));

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id, "--json"]));
    assert_ne!(status["debt_outstanding"], true, "{status}");
    assert!(
        status["debt"].is_null(),
        "a refused integration owes no review: {status}"
    );
}

/// A marker's answer about where a fork lives must be the same from every
/// worktree. A relative record cannot be, so it names no checkout at all
/// rather than one that depends on who asked.
#[test]
fn a_relative_marker_path_names_no_worktree_from_anywhere() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "relative"]));
    let worktree = fork_worktree(&repo, "relative");
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    let dir = PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim());
    let marker = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("-fork-relative-plan.md"))
        })
        .expect("the fork wrote a marker");
    let body = fs::read_to_string(&marker).unwrap();
    let edited: String = body
        .lines()
        .map(|line| {
            if line.starts_with("worktree: ") {
                "worktree: .".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&marker, format!("{edited}\n")).unwrap();

    // Asked from the fork's own checkout and from the primary, the answer is
    // the same: the marker names nothing, so no command claims a live
    // worktree the others cannot see.
    let from_fork = stdout(repo.arc(&worktree).args(["fork", "list"]));
    let from_primary = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(from_fork.contains("no worktree"), "{from_fork}");
    assert!(from_primary.contains("no worktree"), "{from_primary}");
}

/// Fork markers and Git's worktree inventory must come from one repository
/// view. A journal prefix covering the primary checkout must therefore still
/// be used when the question is asked from a linked, detached fork checkout.
#[test]
fn fork_marker_inventory_is_shared_across_worktrees_with_a_journal_prefix() {
    let repo = Repo::new();
    let primary = fs::canonicalize(&repo.root).unwrap();
    let journal = repo.home.join("configured-fork-journal");
    let config = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        format!(
            "[journals.dirs]\n\"{}\" = \"{}\"\n",
            primary.display(),
            journal.display()
        ),
    )
    .unwrap();

    stdout(repo.arc(&repo.root).args(["fork", "begin", "scoped"]));
    let worktree = fork_worktree(&repo, "scoped");
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    let mut from_primary = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    let mut from_fork = json_stdout(repo.arc(&worktree).args(["fork", "list", "--json"]));
    strip_clock_fields(&mut from_primary);
    strip_clock_fields(&mut from_fork);
    assert_eq!(
        from_primary, from_fork,
        "one repository must have one fork view"
    );
    assert_eq!(
        from_fork["forks"][0]["worktree"],
        worktree.to_str().unwrap(),
        "the detached checkout must be resolved from the shared marker"
    );
    assert_eq!(
        stdout(repo.arc(&repo.root).args(["journal", "dir"])),
        stdout(repo.arc(&worktree).args(["journal", "dir"])),
        "both worktrees must read the configured repository journal"
    );
}

/// A relative worktrees_dir is resolved before a fork marker is written, so
/// the marker carries an absolute record. An older relative record is invalid
/// state and must remain visible to the journal repair surface.
#[test]
fn relative_fork_marker_is_written_absolute_and_diagnosed_when_legacy() {
    let repo = Repo::new();
    let config = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "worktrees_dir = \"relative-worktrees\"\n",
    )
    .unwrap();

    let output = stdout(repo.arc(&repo.root).args(["fork", "begin", "legacy"]));
    let worktree = PathBuf::from(
        output
            .lines()
            .find_map(|line| line.strip_prefix("worktree: "))
            .expect("fork output should name its worktree"),
    );
    assert!(
        worktree.is_absolute(),
        "fork output must name an absolute path"
    );

    let dir = PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim());
    let marker = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("-fork-legacy-plan.md"))
        })
        .expect("the fork wrote a marker");
    let body = fs::read_to_string(&marker).unwrap();
    let recorded = body
        .lines()
        .find_map(|line| line.strip_prefix("worktree: "))
        .map(PathBuf::from)
        .expect("the marker should record its worktree");
    assert!(recorded.is_absolute(), "marker records must be absolute");
    assert_eq!(recorded, worktree);

    // This is the state an older writer could leave behind. It has no safe
    // anchor now: resolving it against either caller would invent a different
    // fork location.
    let relative_record = PathBuf::from("relative-worktrees/repo-fork-legacy");
    let legacy_body = body
        .lines()
        .map(|line| {
            if line.starts_with("worktree: ") {
                format!("worktree: {}", relative_record.display())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&marker, format!("{legacy_body}\n")).unwrap();
    git(&worktree, &["checkout", "--detach", "HEAD"]);

    let doctor = |cwd: &Path| {
        let output = repo
            .arc(cwd)
            .args(["journal", "doctor", "--json"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "doctor must reject the marker"
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let from_primary = doctor(&repo.root);
    let from_fork = doctor(&worktree);
    assert_eq!(
        from_primary, from_fork,
        "diagnosis must use the repository journal"
    );
    assert_eq!(from_fork["problems"][0]["code"], "invalid-fork-marker");
    let detail = from_fork["problems"][0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("relative-worktrees/repo-fork-legacy"),
        "the diagnosis should name the invalid marker field: {from_fork}"
    );
}

/// A fork's base is a property of the repository, not of whatever the primary
/// checkout happens to hold. Parking the primary on a change branch must not
/// make every fork measure against it.
#[test]
fn a_fork_base_ignores_a_primary_parked_on_a_working_branch() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "based"]));
    let marker_free = "fork/loose";
    git(&repo.root, &["branch", marker_free]);

    let on_target = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(on_target.contains("over master"), "{on_target}");

    git(&repo.root, &["checkout", "-b", "arc/parked"]);
    let parked = stdout(repo.arc(&repo.root).args(["fork", "list"]));
    assert!(
        !parked.contains("over arc/parked"),
        "a working branch is never an integration target: {parked}"
    );
    assert!(
        parked.contains("over master") || parked.contains("unknown"),
        "{parked}"
    );
}

/// A relative journal destination must anchor to the repository, not to the
/// caller. Otherwise each checkout joins it to its own directory, and an
/// artifact written from one worktree is invisible from another — which is
/// how a fork's marker goes missing from the checkout that most needs it.
#[test]
fn a_relative_journal_destination_is_one_journal_for_every_worktree() {
    let repo = Repo::new();
    let linked = repo.root.parent().unwrap().join("linked-checkout");
    git(
        &repo.root,
        &["worktree", "add", "-b", "side", linked.to_str().unwrap()],
    );

    stdout(
        repo.arc(&repo.root)
            .env("ARC_JOURNAL_DIR", ".arc-journal")
            .args(["fork", "begin", "anchored"]),
    );

    let from_primary = stdout(
        repo.arc(&repo.root)
            .env("ARC_JOURNAL_DIR", ".arc-journal")
            .args(["journal", "list"]),
    );
    let from_linked = stdout(
        repo.arc(&linked)
            .env("ARC_JOURNAL_DIR", ".arc-journal")
            .args(["journal", "list"]),
    );
    assert!(from_primary.contains("fork-anchored"), "{from_primary}");
    assert!(
        from_linked.contains("fork-anchored"),
        "a linked worktree read a different journal: {from_linked}"
    );
}

/// A fork outlives the session that opened it, and the marker records who
/// that was. `fork thread` reads it back, with the command that reopens the
/// session where the harness has a stable resume form.
#[test]
fn fork_thread_prints_the_identity_that_opened_the_fork() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", "session-threaded")
        .env("ARC_MODEL", "model-threaded")
        .args(["fork", "begin", "threaded"])
        .assert()
        .success();

    let out = stdout(repo.arc(&repo.root).args(["fork", "thread", "threaded"]));
    assert!(out.contains("fork: threaded"), "{out}");
    assert!(out.contains("harness: claude"), "{out}");
    assert!(out.contains("session: session-threaded"), "{out}");
    assert!(out.contains("model: model-threaded"), "{out}");
    assert!(out.contains("actor: tester"), "{out}");
    assert!(
        out.contains("resume: claude --resume session-threaded"),
        "{out}"
    );

    // A harness with no known resume form is reported without one: a wrong
    // incantation costs a reader more than an absent one.
    repo.arc(&repo.root)
        .env("ARC_HARNESS", "some-other-harness")
        .args(["fork", "begin", "unresumable"])
        .assert()
        .success();
    let other = stdout(repo.arc(&repo.root).args(["fork", "thread", "unresumable"]));
    assert!(other.contains("harness: some-other-harness"), "{other}");
    assert!(!other.contains("resume: "), "{other}");
}

/// A fork adopted with nothing declared has nothing to attribute, and says
/// so. An identity arc never received is absent, not a plausible name.
#[test]
fn fork_thread_reports_an_undeclared_identity_as_absent() {
    let repo = Repo::new();
    let worktree = fork_worktree(&repo, "handmade");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "fork/handmade",
            worktree.to_str().unwrap(),
        ],
    );
    repo.arc(&repo.root)
        .env_remove("ARC_ACTOR")
        .env_remove("ARC_HARNESS")
        .env_remove("ARC_SESSION")
        .args(["fork", "adopt", "handmade"])
        .assert()
        .success();

    let out = stdout(repo.arc(&repo.root).args(["fork", "thread", "handmade"]));
    assert!(out.contains("harness: absent"), "{out}");
    assert!(out.contains("session: absent"), "{out}");
    assert!(out.contains("model: absent"), "{out}");
    assert!(out.contains("actor: absent"), "{out}");
    assert!(!out.contains("resume: "), "{out}");
}

/// A verification made inside a fork names the fork's own head. The anchor's
/// head is different code, and a stamp that claimed it would credit the check
/// to source nobody opened.
#[test]
fn journal_verified_inside_a_fork_stamps_the_fork_head_and_scope() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "fork-check",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("# Fork check\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    repo.arc(&repo.root)
        .args(["fork", "begin", "checked"])
        .assert()
        .success();
    let worktree = fork_worktree(&repo, "checked");
    // The fork carries a commit the anchor does not, so the two heads are
    // distinguishable and the stamp can be wrong in a visible way.
    repo.commit(&worktree, "forked.txt", "forked\n", "test: fork commit");
    let fork_head = repo.head(&worktree);
    let anchor_head = repo.head(&repo.root);
    assert_ne!(fork_head, anchor_head);

    let out = stdout(
        repo.arc(&worktree)
            .args(["journal", "verified", &file])
            .env("ARC_HARNESS", "test"),
    );
    assert!(out.contains(&format!("at {fork_head}")), "{out}");
    assert!(out.contains("in fork checked"), "{out}");

    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let stamp = open["open"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == file)
        .map(|item| item["verification"].clone())
        .unwrap();
    assert_eq!(stamp["revision"], fork_head, "{stamp}");
    assert_eq!(stamp["scope"], "fork:checked", "{stamp}");
    // The anchor moving says nothing about a revision off its line of
    // history, so no movement is claimed either way.
    assert!(stamp["moved"].is_null(), "{stamp}");

    let text = stdout(repo.arc(&repo.root).args(["journal", "open"]));
    assert!(
        text.contains(&format!("[verified at {} in fork checked", &fork_head[..8])),
        "{text}"
    );
}

/// Outside a fork the stamp is the project anchor's head, with no scope: a
/// check made in the primary checkout means what it always meant.
#[test]
fn journal_verified_outside_a_fork_still_stamps_the_anchor() {
    let repo = Repo::new();
    let seed = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "anchor-check",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("# Anchor check\n"),
    );
    let file = PathBuf::from(seed.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    // A fork exists but is not where the check is made.
    repo.arc(&repo.root)
        .args(["fork", "begin", "elsewhere"])
        .assert()
        .success();
    let anchor_head = repo.head(&repo.root);

    let out = stdout(repo.arc(&repo.root).args(["journal", "verified", &file]));
    assert!(out.contains(&format!("at {anchor_head}")), "{out}");
    assert!(!out.contains("in fork"), "{out}");

    let open = json_stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let stamp = open["open"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == file)
        .map(|item| item["verification"].clone())
        .unwrap();
    assert_eq!(stamp["revision"], anchor_head, "{stamp}");
    assert!(stamp["scope"].is_null(), "{stamp}");
    assert_eq!(stamp["moved"], false, "{stamp}");
}

/// The base a fork begins from is discovered from the repository, never
/// assumed: standing on a fork, the integration branch `origin/HEAD` names
/// is what the new fork carries.
#[test]
fn fork_begin_bases_on_the_discovered_integration_branch() {
    let repo = Repo::new();
    repo.commit(&repo.root, "base.txt", "base\n", "test: integration work");
    git(&repo.root, &["branch", "-m", "master", "main"]);
    git(
        &repo.root,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &repo.root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    // The primary checkout stands on a fork, which is never a base. Its own
    // commit must not reach the new fork.
    git(&repo.root, &["checkout", "-b", "fork/decoy"]);
    repo.commit(&repo.root, "decoy.txt", "decoy\n", "test: decoy work");

    stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    let worktree = fork_worktree(&repo, "demo");
    assert!(worktree.join("base.txt").is_file(), "base must reach fork");
    assert!(
        !worktree.join("decoy.txt").exists(),
        "decoy must not reach fork"
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "fork/demo"]),
        git_out(&repo.root, &["rev-parse", "main"])
    );
}

/// With no branch to stand on and no declared default, there is no base to
/// guess; the refusal names the flag that supplies one.
#[test]
fn fork_begin_refuses_when_no_base_is_discoverable() {
    let repo = Repo::new();
    git(&repo.root, &["checkout", "--detach"]);

    repo.arc(&repo.root)
        .args(["fork", "begin", "demo"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--from <branch>"));
}

/// The boundary binds to the change, not to the directory the caller stands
/// in: a change recorded on a fork branch is refused from the project root,
/// from the fork's own worktree, and from an unrelated worktree, and `check`
/// names the same blocker its exit code reports.
#[test]
fn a_change_on_a_fork_branch_is_refused_from_every_directory() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    let fork_checkout = fork_worktree(&repo, "demo");
    let unrelated = repo.home.join(".worktrees/unrelated");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "unrelated-branch",
            unrelated.to_str().unwrap(),
            "master",
        ],
    );
    let change_id = change_on_fork_branch(&repo, "promoted", "fork/demo");

    for cwd in [&repo.root, &fork_checkout, &unrelated] {
        repo.arc(cwd)
            .args(["integrate", &change_id])
            .assert()
            .code(15)
            .stderr(predicates::str::contains(
                "branch fork/demo is fork demo's work",
            ))
            .stderr(predicates::str::contains("unintegrated by intent"));
    }

    // check reads the same boundary: the blocker is named, so the exit code
    // is not green on something integrate will refuse.
    let checked = repo
        .arc(&repo.root)
        .args(["check", &change_id, "--json"])
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(15));
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert!(
        report["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "fork-branch" && blocker["exit_code"] == 15),
        "{report}"
    );
    assert_eq!(report["ready"], false, "{report}");
}

/// A change on an ordinary branch reads the same wherever it is asked from,
/// including from inside a fork's own worktree, where the directory used to
/// be the whole question.
#[test]
fn a_change_on_an_ordinary_branch_is_unaffected_by_the_callers_directory() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    let fork_checkout = fork_worktree(&repo, "demo");
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "ordinary"])));

    for cwd in [&repo.root, &fork_checkout] {
        let checked = repo
            .arc(cwd)
            .args(["check", &change_id, "--json"])
            .output()
            .unwrap();
        assert_eq!(checked.status.code(), Some(3), "from {}", cwd.display());
        let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
        assert!(
            !report["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker["blocker"] == "fork-branch"),
            "{report}"
        );
        // The refusal is not the fork's either: no fork message is printed.
        repo.arc(cwd)
            .args(["integrate", &change_id, "--dry-run"])
            .assert()
            .code(3)
            .stderr(predicates::str::contains("fork worktree").not());
    }
}

/// `begin --adopt` on a fork branch opens a change that can be gated nowhere
/// and merged nowhere, so the opening refuses and names where the work goes
/// instead. An ordinary branch is untouched.
#[test]
fn begin_refuses_to_adopt_a_fork_branch() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));

    repo.arc(&repo.root)
        .args(["begin", "promoted", "--adopt", "fork/demo", "--no-worktree"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("branch is fork demo"))
        .stderr(predicates::str::contains(
            "arc begin <change> --from-fork demo",
        ));

    // Nothing was opened and the fork's own worktree is still its own.
    let listed = json_stdout(repo.arc(&repo.root).args(["list", "--json"]));
    assert!(listed.as_array().unwrap().is_empty(), "{listed}");
    let forks = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(
        forks["forks"][0]["worktree"],
        fork_worktree(&repo, "demo").to_str().unwrap()
    );

    // An ordinary branch adopts as it always did.
    git(&repo.root, &["branch", "work/plain"]);
    let out = stdout(repo.arc(&repo.root).args([
        "begin",
        "plain",
        "--adopt",
        "work/plain",
        "--no-worktree",
    ]));
    assert!(out.contains("branch: work/plain"), "{out}");
}

/// A change's own branch is never a fork's: naming a new one `fork/<slug>`
/// would open the unintegrable state by another door.
#[test]
fn begin_refuses_to_name_a_new_branch_as_a_fork() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "laundered", "--branch", "fork/laundered"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("branch is fork laundered"));
    let listed = json_stdout(repo.arc(&repo.root).args(["list", "--json"]));
    assert!(listed.as_array().unwrap().is_empty(), "{listed}");
    assert!(
        !git_out(&repo.root, &["branch", "--list", "fork/laundered"]).contains("fork/laundered")
    );
}

/// Opening a change from inside a fork worktree is ordinary work: the new
/// branch comes from the integration target, and the change is not the
/// fork's.
#[test]
fn begin_inside_a_fork_worktree_opens_an_ordinary_change() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    let fork_checkout = fork_worktree(&repo, "demo");

    let out = stdout(repo.arc(&fork_checkout).args(["begin", "outside"]));
    assert!(out.contains("branch: arc/outside"), "{out}");

    let status = json_stdout(repo.arc(&repo.root).args(["status", "outside", "--json"]));
    assert_eq!(status["target_branch"], "master", "{status}");
    assert_eq!(status["branch"], "arc/outside", "{status}");
    assert!(status.get("fork").is_none(), "{status}");
}

/// A fork carrying its own work: two commits on the fork branch, and the head
/// they leave behind.
fn fork_with_work(repo: &Repo, slug: &str) -> (PathBuf, String) {
    stdout(repo.arc(&repo.root).args(["fork", "begin", slug]));
    let worktree = fork_worktree(repo, slug);
    repo.commit(&worktree, "one.txt", "one\n", "test: fork one");
    repo.commit(&worktree, "two.txt", "two\n", "test: fork two");
    let head = repo.head(&worktree);
    (worktree, head)
}

/// `begin --from-fork` promotes a fork onto an ordinary change: its own branch
/// from the target, its own worktree, the fork's commits replayed onto it, and
/// the source recorded. The fork keeps everything it had.
#[test]
fn begin_from_fork_opens_a_change_carrying_the_fork_work() {
    let repo = Repo::new();
    let (fork_checkout, fork_head) = fork_with_work(&repo, "demo");

    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "promoted", "--from-fork", "demo"]),
    );
    assert!(out.contains("from-fork: demo (fork/demo)"), "{out}");
    assert!(out.contains(&format!("fork-head: {fork_head}")), "{out}");

    // The work arrived, on the change's own branch and in its own worktree.
    let change_worktree = repo.home.join(".worktrees/repo-promoted");
    assert!(change_worktree.join("one.txt").is_file());
    assert!(change_worktree.join("two.txt").is_file());
    assert_ne!(change_worktree, fork_checkout);
    let status = json_stdout(repo.arc(&repo.root).args(["status", "promoted", "--json"]));
    assert_eq!(status["branch"], "arc/promoted", "{status}");
    // The link is recorded: fork slug, and the source base, head, and tree.
    let provenance = &status["from_fork"];
    assert_eq!(provenance["slug"], "demo", "{status}");
    assert_eq!(provenance["branch"], "fork/demo", "{status}");
    assert_eq!(provenance["head"], fork_head.as_str(), "{status}");
    assert_eq!(
        provenance["tree"],
        git_out(&repo.root, &["rev-parse", &format!("{fork_head}^{{tree}}")]).as_str(),
        "{status}"
    );
    assert_eq!(
        provenance["base"],
        git_out(&repo.root, &["merge-base", "master", &fork_head]).as_str(),
        "the fork's own range starts at its merge base with the target: {status}"
    );

    // A fork's branch is a fork's branch: the change `--from-fork` created is
    // not refused by the boundary, from any directory, and its ordinary
    // blockers are what stand in the way.
    for cwd in [&repo.root, &fork_checkout, &change_worktree] {
        let checked = repo
            .arc(cwd)
            .args(["check", "promoted", "--json"])
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
        assert!(
            !report["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker["blocker"] == "fork-branch"),
            "from {}: {report}",
            cwd.display()
        );
        assert!(
            report["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker["blocker"] == "no-valid-approval"),
            "fork review evidence is not this change's coverage: {report}"
        );
        repo.arc(cwd)
            .args(["integrate", "promoted", "--dry-run"])
            .assert()
            .code(3)
            .stderr(predicates::str::contains("fork worktree").not());
    }

    // The fork is untouched: same branch, same head, same worktree, live.
    let forks = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(
        forks["forks"][0]["worktree"],
        fork_checkout.to_str().unwrap()
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "fork/demo"]),
        fork_head,
        "promotion must not move the fork"
    );
    assert!(forks["forks"][0].get("retired").is_none(), "{forks}");
}

/// Reading a promoted change surfaces the artifacts filed under the fork it
/// came from: its review evidence, and the findings those artifacts left open.
#[test]
fn resume_surfaces_the_source_forks_journal_artifacts() {
    let repo = Repo::new();
    fork_with_work(&repo, "demo");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "promoted", "--from-fork", "demo"]),
    );
    let (_, review) = journal_artifact(
        &repo,
        "fork-demo",
        "review",
        "# Review of demo\n\nOne finding left open.\n",
    );

    let resumed = json_stdout(repo.arc(&repo.root).args(["resume", "promoted", "--json"]));
    let artifacts = resumed["journal"]["from_fork_items"].as_array().unwrap();
    assert!(
        artifacts.iter().any(|item| item["file"] == review.as_str()),
        "{resumed}"
    );

    let text = stdout(repo.arc(&repo.root).args(["resume", "promoted"]));
    assert!(text.contains("## From Fork"), "{text}");
    assert!(text.contains("Source head:"), "{text}");
    assert!(text.contains("### From the fork"), "{text}");
    assert!(text.contains(&review), "{text}");
    assert!(
        text.contains("not review coverage"),
        "the link grants no credit and says so: {text}"
    );
}

/// One fork can feed several changes: each owns its patchset and its link, and
/// neither inherits coverage from the other.
#[test]
fn one_fork_can_feed_several_changes() {
    let repo = Repo::new();
    let (_, fork_head) = fork_with_work(&repo, "demo");
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "first", "--from-fork", "demo"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "second", "--from-fork", "demo"]),
    );

    for slug in ["first", "second"] {
        let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
        assert_eq!(status["from_fork"]["slug"], "demo", "{status}");
        assert_eq!(status["branch"], format!("arc/{slug}"), "{status}");
        assert_eq!(status["from_fork"]["head"], fork_head.as_str(), "{status}");
    }
    assert_ne!(
        git_out(&repo.root, &["rev-parse", "arc/first"]),
        "",
        "each change keeps its own branch ref"
    );
    assert_ne!(
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "arc/second"]),
        git_out(&repo.root, &["rev-parse", "--abbrev-ref", "fork/demo"]),
    );

    // Each change snapshots its own patchset, and an obligation declared for
    // one lands on that change rather than on the fork.
    let mut patchsets = Vec::new();
    for slug in ["first", "second"] {
        let worktree = repo.home.join(format!(".worktrees/repo-{slug}"));
        repo.commit(
            &worktree,
            &format!("{slug}.txt"),
            "work\n",
            "test: change work",
        );
        stdout(repo.arc(&worktree).args(["snapshot", slug]));
        let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
        patchsets.push((
            status["change_id"].as_str().unwrap().to_string(),
            status["latest_patchset"]["head"]
                .as_str()
                .unwrap()
                .to_string(),
        ));
    }
    assert_ne!(patchsets[0].0, patchsets[1].0, "two changes, two records");
    assert_ne!(
        patchsets[0].1, patchsets[1].1,
        "each change snapshots its own patchset"
    );

    repo.arc(&repo.root)
        .args([
            "debt",
            "first",
            "--kind",
            "independent-review",
            "--reason",
            "review owed",
        ])
        .assert()
        .success();
    let first = json_stdout(repo.arc(&repo.root).args(["status", "first", "--json"]));
    assert_eq!(first["debt"]["reason"], "review owed", "{first}");
    let second = json_stdout(repo.arc(&repo.root).args(["status", "second", "--json"]));
    assert!(second["debt"].is_null(), "{second}");
    let forks = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(forks["forks"].as_array().unwrap().len(), 1, "{forks}");
    assert!(forks["forks"][0].get("debt").is_none(), "{forks}");
}

/// A fork with no commits of its own owes nothing to carry, and the promotion
/// says so rather than inventing an empty range.
#[test]
fn begin_from_fork_with_no_commits_carries_nothing() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "empty"]));
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "promoted", "--from-fork", "empty"]),
    );
    assert!(
        out.contains("from-fork: empty has no commits of its own to carry"),
        "{out}"
    );
    let status = json_stdout(repo.arc(&repo.root).args(["status", "promoted", "--json"]));
    assert_eq!(
        status["from_fork"]["base"], status["from_fork"]["head"],
        "{status}"
    );
}

/// `--from-fork` naming no fork refuses by name, and nothing is created.
#[test]
fn begin_from_fork_naming_no_fork_refuses() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["begin", "promoted", "--from-fork", "ghost"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "no fork \"ghost\" is recorded here",
        ))
        .stderr(predicates::str::contains("arc fork list"));
    let listed = json_stdout(repo.arc(&repo.root).args(["list", "--json"]));
    assert!(listed.as_array().unwrap().is_empty(), "{listed}");
}

/// The flag opens its own branch, so combining it with the adopt path is a
/// usage error rather than a half-promoted change.
#[test]
fn begin_from_fork_refuses_to_combine_with_adopt() {
    let repo = Repo::new();
    fork_with_work(&repo, "demo");
    repo.arc(&repo.root)
        .args([
            "begin",
            "promoted",
            "--from-fork",
            "demo",
            "--adopt",
            "fork/demo",
        ])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("cannot be combined with --adopt"));
    let listed = json_stdout(repo.arc(&repo.root).args(["list", "--json"]));
    assert!(listed.as_array().unwrap().is_empty(), "{listed}");
}

/// A hand-made branch is adoptable under its own name: the marker records it,
/// the branch is not renamed, and the fork is the same fork as one arc made.
#[test]
fn fork_adopt_takes_a_branch_that_is_not_named_fork_slug() {
    let repo = Repo::new();
    let worktree = repo.home.join(".worktrees/browser-control");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "work/cu-browser-control",
            worktree.to_str().unwrap(),
            "master",
        ],
    );

    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "adopt",
        "browser",
        "--branch",
        "work/cu-browser-control",
    ]));
    assert!(out.contains("adopted: browser"), "{out}");

    // The branch keeps the name everybody else knows it by.
    assert_eq!(
        git_out(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "work/cu-browser-control"
    );
    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(listed["forks"][0]["slug"], "browser", "{listed}");
    assert_eq!(
        listed["forks"][0]["branch"], "work/cu-browser-control",
        "{listed}"
    );
    assert_eq!(
        listed["forks"][0]["worktree"],
        worktree.to_str().unwrap(),
        "{listed}"
    );

    // thread, retire, and integration all read the marker's branch.
    let thread = stdout(repo.arc(&repo.root).args(["fork", "thread", "browser"]));
    assert!(
        thread.contains("branch: work/cu-browser-control"),
        "{thread}"
    );

    let checked = repo
        .arc(&repo.root)
        .args(["fork", "retire", "browser", "dropped: superseded"])
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    assert!(!worktree.exists());
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "work/cu-browser-control"]),
        repo.head(&repo.root),
        "retire keeps the branch"
    );
}

/// A branch with no checkout is still a fork: the marker records the branch,
/// no path is invented, and the listing reports the worktree as absent with no
/// counts at all.
#[test]
fn fork_adopt_takes_a_branch_with_no_worktree() {
    let repo = Repo::new();
    git(&repo.root, &["branch", "work/no-checkout"]);

    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "adopt",
        "orphan",
        "--branch",
        "work/no-checkout",
    ]));
    assert!(out.contains("adopted: orphan (no worktree)"), "{out}");

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    let fork = &listed["forks"][0];
    assert_eq!(fork["branch"], "work/no-checkout", "{listed}");
    assert!(fork["worktree"].is_null(), "{listed}");
    assert!(fork["dirty_files"].is_null(), "{listed}");
    assert!(fork["untracked_files"].is_null(), "{listed}");

    // A marker that names no path still names the fork, and it can be retired.
    let out = stdout(
        repo.arc(&repo.root)
            .args(["fork", "retire", "orphan", "dropped: no work"]),
    );
    assert!(out.contains("retired: orphan"), "{out}");
}

/// The boundary reads the marker: a change on a branch adopted under another
/// name is refused from every directory, exactly as one on `fork/<slug>` is.
/// Adoption now refuses an open change's branch, so the state it can no longer
/// create is the fixture here: a marker over a branch that already held a
/// change, which is what an arc without the guard left behind and what an
/// independent reader has to be sure the blocker still catches.
#[test]
fn a_change_on_an_adopted_branch_is_refused_by_integrate() {
    let repo = Repo::new();
    let worktree = repo.home.join(".worktrees/adopted-work");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "work/plain",
            worktree.to_str().unwrap(),
            "master",
        ],
    );
    let change_id = opened_change_id(&stdout(repo.arc(&worktree).args([
        "begin",
        "promoted",
        "--adopt",
        "work/plain",
        "--no-worktree",
    ])));

    // The marker an older arc would have journaled over the branch.
    journal_artifact(
        &repo,
        "fork-demo",
        "plan",
        &format!(
            "branch: work/plain\nworktree: {}\nstatus: adopted\n",
            worktree.display()
        ),
    );

    let unrelated = repo.home.join(".worktrees/unrelated");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "unrelated-branch",
            unrelated.to_str().unwrap(),
            "master",
        ],
    );

    for cwd in [&repo.root, &worktree, &unrelated] {
        let checked = repo
            .arc(cwd)
            .args(["check", &change_id, "--json"])
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&checked.stdout).unwrap();
        assert!(
            report["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|blocker| blocker["blocker"] == "fork-branch"),
            "from {}: {report}",
            cwd.display()
        );
        let status = json_stdout(repo.arc(cwd).args(["status", &change_id, "--json"]));
        assert_eq!(status["fork"], "demo", "{status}");
        repo.arc(cwd)
            .args(["integrate", &change_id])
            .assert()
            .code(15)
            .stderr(predicates::str::contains(
                "branch work/plain is fork demo's work",
            ));
    }
}

/// `fork list` says what a fork holds: when it opened, how old it is, its
/// head, whether its checkout has work arc cannot see, and what was promoted
/// from it. A fresh empty fork and one carrying work read differently.
#[test]
fn fork_list_reports_what_a_fork_holds() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "fresh"]));
    let fresh = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    let fresh = &fresh["forks"][0];
    assert!(fresh["opened_at"].is_string(), "{fresh}");
    assert!(fresh["age_seconds"].as_i64().unwrap() >= 0, "{fresh}");
    assert!(fresh["head"].is_string(), "{fresh}");
    assert_eq!(
        fresh["dirty_files"], 0,
        "a clean fork reports zero: {fresh}"
    );
    assert_eq!(fresh["untracked_files"], 0, "{fresh}");
    assert_eq!(fresh["promoted"].as_array().unwrap().len(), 0, "{fresh}");

    // Work inside the fork and uncommitted edits beside it.
    stdout(repo.arc(&repo.root).args(["fork", "begin", "working"]));
    let worktree = fork_worktree(&repo, "working");
    repo.commit(&worktree, "committed.txt", "committed\n", "test: fork work");
    fs::write(worktree.join("untracked.txt"), "local\n").unwrap();
    fs::write(worktree.join("README.md"), "edited\n").unwrap();

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    let working = listed["forks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fork| fork["slug"] == "working")
        .unwrap();
    assert_eq!(working["dirty_files"], 1, "{listed}");
    assert_eq!(working["untracked_files"], 1, "{listed}");
    assert_eq!(working["ahead"], 1, "{listed}");
    assert_ne!(working["head"], fresh["head"], "{listed}");
    assert!(
        listed["forks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fork| fork["promoted"].as_array().unwrap().is_empty()),
        "nothing was promoted yet: {listed}"
    );

    // Promotion shows up against the fork that fed it.
    let change_id = opened_change_id(&stdout(repo.arc(&repo.root).args([
        "begin",
        "promoted",
        "--from-fork",
        "working",
    ])));
    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    let working = listed["forks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fork| fork["slug"] == "working")
        .unwrap();
    assert_eq!(
        working["promoted"].as_array().unwrap(),
        &vec![serde_json::Value::String(change_id)],
        "{listed}"
    );
}

/// Adoption asks Git which checkout holds the branch, the same question the
/// listing asks, so the path it reports is the path `fork list` reports — not
/// a conventional location derived from a branch name that no longer implies
/// one.
#[test]
fn fork_adopt_reports_the_worktree_git_records() {
    let repo = Repo::new();
    let worktree = repo.home.join(".worktrees/hand-made-place");
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "-b",
            "work/handmade",
            worktree.to_str().unwrap(),
            "master",
        ],
    );

    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "adopt",
        "handmade",
        "--branch",
        "work/handmade",
    ]));
    assert!(
        out.contains(&format!("adopted: handmade at {}", worktree.display())),
        "{out}"
    );

    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert_eq!(
        listed["forks"][0]["worktree"],
        worktree.to_str().unwrap(),
        "{listed}"
    );

    // A branch with genuinely no checkout still reports none.
    git(&repo.root, &["branch", "work/bare"]);
    let out = stdout(
        repo.arc(&repo.root)
            .args(["fork", "adopt", "bare", "--branch", "work/bare"]),
    );
    assert!(out.contains("adopted: bare (no worktree)"), "{out}");
}

/// A marker is what makes a branch a fork, so adopting an open change's branch
/// would make that change unintegrable from every directory without touching
/// the change — and nothing un-adopts a marker. The refusal names the change
/// it would have bricked.
#[test]
fn fork_adopt_refuses_an_open_changes_branch() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args(["begin", "later-fork"]));
    let change_id = opened_change_id(&output);

    repo.arc(&repo.root)
        .args(["fork", "adopt", "lateradopt", "--branch", "arc/later-fork"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(&change_id))
        .stderr(predicates::str::contains("(later-fork)"))
        .stderr(predicates::str::contains("cannot adopt a change's branch"));

    // Nothing was recorded, and the change reads exactly as it did.
    let listed = json_stdout(repo.arc(&repo.root).args(["fork", "list", "--json"]));
    assert!(listed["forks"].as_array().unwrap().is_empty(), "{listed}");
    let status = json_stdout(
        repo.arc(&repo.root)
            .args(["status", "later-fork", "--json"]),
    );
    assert!(status.get("fork").is_none(), "{status}");
    assert!(
        !status["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["blocker"] == "fork-branch"),
        "{status}"
    );

    // Closing the change releases the branch: the guard is the open record,
    // not the branch name.
    stdout(
        repo.arc(&repo.root)
            .args(["close", "later-fork", "--abandoned"]),
    );
    let out = stdout(repo.arc(&repo.root).args([
        "fork",
        "adopt",
        "lateradopt",
        "--branch",
        "arc/later-fork",
    ]));
    assert!(out.contains("adopted: lateradopt"), "{out}");
}

/// The fork refusal binds to the change, so it states what the change records
/// and never where the caller is standing: from the repository root, where no
/// fork worktree is in play, the sentence is the same one the fork's own
/// checkout prints.
#[test]
fn the_fork_refusal_is_about_the_change_not_the_callers_directory() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["fork", "begin", "demo"]));
    let fork_checkout = fork_worktree(&repo, "demo");
    let change_id = change_on_fork_branch(&repo, "promoted", "fork/demo");

    for cwd in [&repo.root, &fork_checkout] {
        let out = repo.arc(cwd).args(["check", &change_id]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(15), "{text}");
        assert!(
            text.contains("branch fork/demo is fork demo's work"),
            "from {}: {text}",
            cwd.display()
        );
        assert!(
            !text.contains("fork worktree"),
            "the refusal must not claim the caller is in one: {text}"
        );
    }
}

/// Work no change and no fork owns is exactly what every queue reports empty.
/// `catchup` and the inbox name the unowned branches, the merged cleanup
/// candidates, and the unowned worktrees with their dirt.
#[test]
fn catchup_surfaces_branches_and_worktrees_no_owner_names() {
    let repo = Repo::new();
    let target = git_out(&repo.root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(target, "master");

    // An unowned branch one commit past the target, held by a worktree with
    // a large dirty file and two untracked ones.
    git(&repo.root, &["branch", "work/loose"]);
    git(&repo.root, &["checkout", "work/loose"]);
    fs::write(repo.root.join("large.bin"), "x".repeat(4 * 1024 * 1024)).unwrap();
    git(&repo.root, &["add", "large.bin"]);
    git(&repo.root, &["commit", "-m", "feat: loose work"]);
    git(&repo.root, &["checkout", &target]);
    let worktree = repo.home.join("loose-worktree");
    git(
        &repo.root,
        &["worktree", "add", worktree.to_str().unwrap(), "work/loose"],
    );
    fs::write(worktree.join("large.bin"), "y".repeat(4 * 1024 * 1024)).unwrap();
    fs::write(worktree.join("scratch-a.txt"), "a\n").unwrap();
    fs::write(worktree.join("scratch-b.txt"), "b\n").unwrap();

    // A merged branch with no owner is a cleanup candidate, not unmerged work.
    git(&repo.root, &["branch", "merged-cleanup"]);

    // An open change's branch and an active fork's branch and checkout are
    // owned and must not appear.
    let owned_change = begin_change(&repo, "owned-branch", None);
    assert!(owned_change.starts_with("owned-branch"));
    repo.arc(&repo.root)
        .args(["fork", "begin", "owned-fork"])
        .assert()
        .success();

    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    assert_eq!(inbox["schema"], "arc-inbox/10");
    let names = |bucket: &str| -> Vec<String> {
        inbox[bucket]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap().to_string())
            .collect()
    };
    let unowned = names("unowned_branches");
    assert!(unowned.contains(&"work/loose".to_string()), "{inbox}");
    assert!(
        !unowned.contains(&"arc/owned-branch".to_string()),
        "an open change's branch must not appear: {inbox}"
    );
    assert!(
        !unowned.contains(&"fork/owned-fork".to_string()),
        "an active fork's branch must not appear: {inbox}"
    );
    let merged = names("merged_branches");
    assert!(merged.contains(&"merged-cleanup".to_string()), "{inbox}");
    assert!(!merged.contains(&"work/loose".to_string()), "{inbox}");

    let loose = inbox["unowned_branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "work/loose")
        .unwrap();
    assert_eq!(loose["ahead"], 1, "{inbox}");
    assert_eq!(loose["behind"], 0, "{inbox}");
    assert!(loose["age_days"].is_u64(), "{inbox}");
    assert_eq!(loose["worktree"], worktree.display().to_string(), "{inbox}");
    assert!(
        loose["action"]
            .as_str()
            .unwrap()
            .contains("--adopt work/loose"),
        "{inbox}"
    );
    let cleanup = inbox["merged_branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "merged-cleanup")
        .unwrap();
    assert!(
        cleanup["action"]
            .as_str()
            .unwrap()
            .contains("git branch -d merged-cleanup"),
        "{inbox}"
    );

    let worktrees = inbox["unowned_worktrees"].as_array().unwrap();
    let loose_worktree = worktrees
        .iter()
        .find(|row| row["path"] == worktree.display().to_string())
        .unwrap();
    assert_eq!(loose_worktree["branch"], "work/loose", "{inbox}");
    assert_eq!(loose_worktree["dirty_files"], 1, "{inbox}");
    assert_eq!(loose_worktree["untracked_files"], 2, "{inbox}");
    assert!(
        !worktrees
            .iter()
            .any(|row| row["path"].as_str().unwrap().contains("owned-fork")),
        "an active fork's checkout is owned: {inbox}"
    );

    // The human catchup rendering names the same surface.
    let text = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(text.contains("unowned branches"), "{text}");
    assert!(text.contains("work/loose"), "{text}");
    assert!(text.contains("merged branches with no owner"), "{text}");
    assert!(text.contains("unowned worktrees"), "{text}");

    git(
        &repo.root,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
}

/// A repository whose refs and worktrees are all owned reports an empty
/// unowned surface, so the ordinary catchup carries no new noise.
#[test]
fn an_owned_repository_reports_no_unowned_surface() {
    let repo = Repo::new();
    let inbox = json_stdout(repo.arc(&repo.root).args(["inbox", "--json"]));
    for bucket in ["unowned_branches", "merged_branches", "unowned_worktrees"] {
        assert!(
            inbox[bucket].as_array().unwrap().is_empty(),
            "{bucket} is not empty: {inbox}"
        );
    }
    let text = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(!text.contains("unowned"), "{text}");
    assert!(!text.contains("merged branches with no owner"), "{text}");
}
