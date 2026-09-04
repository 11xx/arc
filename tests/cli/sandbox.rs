//! One prefix for every root arc writes, and the copy of a project made under
//! one.
//!
//! The fixture declares its own prefix, so these tests give arc a *second*
//! prefix and keep the fixture's home as the thing that must stay untouched.
//! That is what makes the containment assertion mean something: the roots arc
//! would otherwise write to are real, populated, and observable.

use super::common::*;

/// The tree under a directory, as path plus content digest, so a changed byte
/// counts as a change and a mere read does not.
fn snapshot(root: &Path) -> Vec<String> {
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries.sort();
    entries
}

fn walk(root: &Path, at: &Path, entries: &mut Vec<String>) {
    let Ok(dir) = fs::read_dir(at) else {
        return;
    };
    for entry in dir.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap().display().to_string();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                entries.push(format!("{relative}/"));
                walk(root, &path, entries);
            }
            Ok(_) => {
                let digest = fs::read(&path)
                    .map(|bytes| hex::encode(Sha256::digest(&bytes)))
                    .unwrap_or_else(|error| error.to_string());
                entries.push(format!("{relative} {digest}"));
            }
            Err(_) => entries.push(relative),
        }
    }
}

/// A prefix has to be absolute: every root is derived by joining onto it, and a
/// relative one would mean a different set of roots per working directory.
#[test]
fn a_relative_prefix_is_refused() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .env("ARC_SANDBOX", "relative/prefix")
        .args(["config"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("must be an absolute path"));
}

/// The prefix moves the roots together, and `doctor` says where they are.
#[test]
fn the_prefix_relocates_every_root_and_doctor_reports_them() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");

    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_SANDBOX", &prefix)
            .args(["begin", "boxed-change"]),
    );
    assert!(out.contains("change: boxed-change-"), "{out}");
    assert!(prefix.join(".worktrees/repo-boxed-change").is_dir());

    let roots: serde_json::Value = json_stdout(
        repo.arc(&repo.root)
            .env("ARC_SANDBOX", &prefix)
            .args(["doctor", "--json"]),
    );
    let roots = &roots["roots"];
    assert_eq!(roots["sandbox"], prefix.display().to_string());
    assert_eq!(
        roots["ai_home"],
        prefix.join(".local/ai").display().to_string()
    );
    assert_eq!(
        roots["worktrees_dir"],
        prefix.join(".worktrees").display().to_string()
    );
    assert_eq!(
        roots["journal_dir"],
        prefix
            .join(".local/ai/journals")
            .join(config_path_slug(&repo.root))
            .display()
            .to_string()
    );
    // The ledger lives in the repository's own Git dir, which is the one place
    // a sandbox does not move: it is the repository arc was pointed at.
    assert_eq!(
        roots["ledger"],
        repo.root.join(".git/arc").display().to_string()
    );

    // Absent a prefix, the roots are the caller's own and nothing claims a
    // sandbox is in force.
    let plain: serde_json::Value = json_stdout(
        repo.arc(&repo.root)
            .env_remove("ARC_SANDBOX")
            .args(["doctor", "--json"]),
    );
    assert_eq!(plain["roots"]["sandbox"], serde_json::Value::Null);
    assert_eq!(
        plain["roots"]["worktrees_dir"],
        repo.home.join(".worktrees").display().to_string()
    );
}

/// A variable naming one exact directory keeps naming it: the prefix replaces
/// defaults, not statements.
#[test]
fn an_exact_root_override_wins_over_the_prefix() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");
    let worktrees = repo.home.join("elsewhere");

    stdout(
        repo.arc(&repo.root)
            .env("ARC_SANDBOX", &prefix)
            .env("ARC_WORKTREES_DIR", &worktrees)
            .args(["begin", "stated"]),
    );
    assert!(worktrees.join("repo-stated").is_dir());
    assert!(!prefix.join(".worktrees/repo-stated").exists());
}

/// A configured `~/…` path follows the prefix the way a default does, so a
/// sandbox cannot be escaped by a config the source project happened to hold.
#[test]
fn a_tilde_path_in_the_configuration_expands_under_the_prefix() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");
    fs::create_dir_all(prefix.join(".local/ai/arc")).unwrap();
    fs::write(
        prefix.join(".local/ai/arc/config.toml"),
        "worktrees_dir = \"~/trees\"\n",
    )
    .unwrap();

    stdout(
        repo.arc(&repo.root)
            .env("ARC_SANDBOX", &prefix)
            .args(["begin", "tilded"]),
    );
    assert!(prefix.join("trees/repo-tilded").is_dir());
    assert!(!repo.home.join("trees").exists());
}

/// The ledger is the one root that stays in the repository by default, so the
/// prefix has to reach it wherever a data root moves it out.
#[test]
fn a_data_root_under_the_prefix_takes_the_ledger_with_it() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");
    fs::create_dir_all(prefix.join(".local/ai/arc")).unwrap();
    fs::write(
        prefix.join(".local/ai/arc/config.toml"),
        "data_root = \"~/ledgers\"\n",
    )
    .unwrap();

    let arc = || {
        let mut command = repo.arc(&repo.root);
        command.env("ARC_SANDBOX", &prefix);
        command
    };
    stdout(arc().args(["begin", "rooted", "--no-worktree"]));
    let ledger = prefix
        .join("ledgers")
        .join(config_path_slug(&repo.root))
        .join("config.json");
    assert!(ledger.is_file(), "ledger should be at {}", ledger.display());
    assert!(!repo.root.join(".git/arc").exists());
    assert_eq!(
        json_stdout(arc().args(["doctor", "--json"]))["roots"]["ledger"],
        prefix
            .join("ledgers")
            .join(config_path_slug(&repo.root))
            .display()
            .to_string()
    );
}

/// The acceptance test for containment: every write verb, run under a prefix,
/// against a home that holds a real project's roots — and nothing there moves.
#[test]
fn no_write_verb_under_a_prefix_touches_anything_outside_it() {
    let repo = Repo::new();
    // Populate the roots that must stay untouched: a change, a journal
    // artifact, a worktree, and the registry entry that comes with them.
    let (_, worktree, _) = change_with_patchset(&repo, "outside");
    journal_artifact(&repo, "outside-topic", "todo", "# Outside\n\nbody\n");
    assert!(worktree.is_dir());

    let prefix = repo.home.join("boxed");
    fs::create_dir_all(&prefix).unwrap();
    let arc = |cwd: &Path| {
        let mut command = repo.arc(cwd);
        command.env("ARC_SANDBOX", &prefix);
        command
    };
    // The prefix sits inside the fixture's home, so it is excluded from the
    // snapshot by taking it before the prefix holds anything and comparing
    // only what is outside it.
    let before = snapshot(&repo.home)
        .into_iter()
        .filter(|entry| !entry.starts_with("boxed"))
        .collect::<Vec<_>>();

    stdout(arc(&repo.root).args(["begin", "inside"]));
    let inside = prefix.join(".worktrees/repo-inside");
    repo.commit(&inside, "inside.txt", "inside\n", "feat: inside");
    stdout(arc(&inside).args(["snapshot", "inside"]));
    arc(&inside)
        .args(["review", "inside", "--verdict", "approved"])
        .assert()
        .success();
    arc(&inside)
        .args(["keep", "--kind", "verified", "--body", "held up"])
        .assert()
        .success();
    let body = prefix.join("note.md");
    fs::write(&body, "# Inside\n\nbody\n").unwrap();
    arc(&repo.root)
        .env("ARC_SANDBOX", &prefix)
        .args([
            "journal",
            "note",
            "inside-topic",
            "--kind",
            "todo",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();
    arc(&repo.root).args(["doctor"]).assert().success();
    arc(&repo.root).args(["catchup"]).assert().success();
    arc(&repo.root)
        .args(["config", "--check-writable"])
        .assert()
        .success();

    let after = snapshot(&repo.home)
        .into_iter()
        .filter(|entry| !entry.starts_with("boxed"))
        .collect::<Vec<_>>();
    assert_eq!(
        before,
        after,
        "a sandboxed run wrote outside its prefix; \
         entries only after: {:?}; entries only before: {:?}",
        after
            .iter()
            .filter(|entry| !before.contains(entry))
            .collect::<Vec<_>>(),
        before
            .iter()
            .filter(|entry| !after.contains(entry))
            .collect::<Vec<_>>(),
    );
    // The ledger of the repository arc was pointed at is the one root a
    // sandbox does not move, so the change landed there rather than nowhere.
    let out = stdout(arc(&repo.root).args(["list"]));
    assert!(out.contains("inside"), "{out}");
}

/// A clone answers the way the source does, from state that is entirely its own.
#[test]
fn a_clone_answers_catchup_the_way_the_source_does() {
    let repo = Repo::new();
    begin_change(&repo, "cloned-change", None);
    journal_artifact(&repo, "cloned-topic", "todo", "# Cloned\n\nbody\n");
    let prefix = repo.home.join("boxed");

    let report = json_stdout(repo.arc(&repo.root).args([
        "sandbox",
        "clone",
        prefix.to_str().unwrap(),
        "--json",
    ]));
    let clone = PathBuf::from(report["repository"].as_str().unwrap());
    assert_eq!(clone, prefix.join("repo"));
    assert!(clone.join(".git/arc/config.json").is_file());
    assert!(clone.join("README.md").is_file());

    // The copy holds the change and the artifact, keyed to its own paths.
    let queues = stdout(
        repo.arc(&clone)
            .env("ARC_SANDBOX", &prefix)
            .args(["list", "--open"]),
    );
    assert!(queues.contains("cloned-change"), "{queues}");
    let journal = PathBuf::from(report["journal"].as_str().unwrap());
    assert_eq!(
        journal,
        prefix
            .join(".local/ai/journals")
            .join(config_path_slug(&clone))
    );
    assert!(journal_event_log(&journal)
        .iter()
        .any(|event| event["topic"] == "cloned-topic"));
    // A journal states which project it belongs to; the copy's says the copy.
    let bindings = fs::read_to_string(journal.join("bindings.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(bindings.lines().last().unwrap()).unwrap();
    assert_eq!(last["anchor"], clone.display().to_string());
    assert_eq!(last["previous_anchor"], repo.root.display().to_string());

    // Every ref the source has, under its own name rather than a
    // remote-tracking one, and no remote to push back through.
    assert_eq!(
        git_out(&clone, &["for-each-ref", "--format=%(refname)"]),
        git_out(&repo.root, &["for-each-ref", "--format=%(refname)"])
    );
    assert!(git_out(&clone, &["remote"]).is_empty());

    // Nothing in the copy's configuration points at the source's paths.
    let config = fs::read_to_string(prefix.join(".local/ai/arc/config.toml")).unwrap();
    assert!(
        !config.contains(repo.root.to_str().unwrap()),
        "the copy inherited a path from the source: {config}"
    );
}

/// A prefix that already holds something is refused, so a clone never merges
/// two projects' copies into one set of roots.
#[test]
fn cloning_refuses_an_occupied_prefix() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");
    repo.arc(&repo.root)
        .args(["sandbox", "clone", prefix.to_str().unwrap()])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["sandbox", "clone", prefix.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("already holds a sandbox"));

    let occupied = repo.home.join("occupied");
    fs::create_dir_all(occupied.join("something")).unwrap();
    repo.arc(&repo.root)
        .args(["sandbox", "clone", occupied.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is not empty"));
}

/// What the sandbox gained, in both directions, across all three stores.
#[test]
fn diff_reports_what_the_sandbox_gained() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");
    let report = json_stdout(repo.arc(&repo.root).args([
        "sandbox",
        "clone",
        prefix.to_str().unwrap(),
        "--json",
    ]));
    let clone = PathBuf::from(report["repository"].as_str().unwrap());

    let fresh = stdout(
        repo.arc(&repo.root)
            .args(["sandbox", "diff", prefix.to_str().unwrap()]),
    );
    assert!(fresh.contains("identical"), "{fresh}");

    stdout(
        repo.arc(&clone)
            .env("ARC_SANDBOX", &prefix)
            .args(["begin", "only-inside"]),
    );
    let body = prefix.join("note.md");
    fs::write(&body, "# Inside\n\nbody\n").unwrap();
    repo.arc(&clone)
        .env("ARC_SANDBOX", &prefix)
        .args([
            "journal",
            "note",
            "inside-topic",
            "--kind",
            "todo",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();

    let diff = json_stdout(repo.arc(&repo.root).args([
        "sandbox",
        "diff",
        prefix.to_str().unwrap(),
        "--json",
    ]));
    let gained = |field: &str| -> Vec<String> {
        diff[field]["only_in_sandbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        gained("ledger_events")
            .iter()
            .any(|event| event.starts_with("only-inside-")),
        "{diff}"
    );
    assert!(
        gained("journal_events")
            .iter()
            .any(|event| event.contains("inside-topic")),
        "{diff}"
    );
    assert!(
        gained("refs")
            .iter()
            .any(|line| line.contains("refs/heads/arc/only-inside")),
        "{diff}"
    );
    for field in ["ledger_events", "journal_events", "refs"] {
        assert_eq!(
            diff[field]["only_in_source"].as_array().unwrap().len(),
            0,
            "the source gained nothing: {diff}"
        );
    }
}

/// Discard removes a prefix arc recorded as a sandbox, and refuses anything
/// else — it deletes a whole tree, so it acts only where the record says what
/// the tree is.
#[test]
fn discard_removes_only_a_sandbox_arc_made() {
    let repo = Repo::new();
    begin_change(&repo, "source-change", None);
    let prefix = repo.home.join("boxed");
    repo.arc(&repo.root)
        .args(["sandbox", "clone", prefix.to_str().unwrap()])
        .assert()
        .success();

    let bystander = repo.home.join("not-a-sandbox");
    fs::create_dir_all(&bystander).unwrap();
    fs::write(bystander.join("keepme"), "precious\n").unwrap();
    repo.arc(&repo.root)
        .args(["sandbox", "discard", bystander.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is not a sandbox arc made"));
    assert!(bystander.join("keepme").is_file());

    // Removing the tree the caller is standing in would leave them nowhere.
    repo.arc(&prefix)
        .args(["sandbox", "discard", prefix.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("from inside it"));
    assert!(prefix.is_dir());

    repo.arc(&repo.root)
        .args(["sandbox", "discard", prefix.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("discarded"));
    assert!(!prefix.exists());
    // The source keeps its own ledger and its answers through all of it.
    assert!(repo.root.join(".git/arc").is_dir());
    assert!(stdout(repo.arc(&repo.root).args(["list", "--open"])).contains("source-change"));
}

/// A gate to ask for, committed before the copy so both trees declare it.
fn commit_gates(repo: &Repo) {
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.alpha]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: add gates"]);
}

/// A clone, and the source paths its copied ledger states. Everything a
/// sandboxed command must refuse to follow is here: an open change whose
/// recorded checkout is the source's, and the two trees that must not move.
struct Cloned {
    prefix: PathBuf,
    clone: PathBuf,
    worktree: PathBuf,
}

fn cloned_with_an_open_change(repo: &Repo, slug: &str) -> Cloned {
    commit_gates(repo);
    let (_, worktree, _) = change_with_patchset(repo, slug);
    let prefix = repo.home.join("boxed");
    repo.arc(&repo.root)
        .args(["sandbox", "clone", prefix.to_str().unwrap()])
        .assert()
        .success();
    Cloned {
        clone: prefix.join("repo"),
        prefix,
        worktree,
    }
}

/// The copied ledger states the source's absolute checkout path, so a gate run
/// that trusted it would run in the source tree. The prefix bounds recorded
/// paths as well as derived ones: the path is named, and nothing outside moves.
#[test]
fn a_sandboxed_gate_run_refuses_the_recorded_checkout_outside_the_prefix() {
    let repo = Repo::new();
    let boxed = cloned_with_an_open_change(&repo, "outside-gate");

    let before = (snapshot(&repo.root), snapshot(&boxed.worktree));
    let refusal = repo
        .arc(&boxed.clone)
        .env("ARC_SANDBOX", &boxed.prefix)
        .args(["verify", "outside-gate", "--gate", "alpha"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refusal.get_output().stderr).into_owned();
    assert!(
        stderr.contains(boxed.worktree.to_str().unwrap()),
        "the refusal should name the outside path: {stderr}"
    );
    assert!(stderr.contains("lies outside the sandbox"), "{stderr}");
    assert!(
        stderr.contains(&format!("git worktree add {}/", boxed.prefix.display())),
        "the tip should name a checkout inside the prefix: {stderr}"
    );
    assert_eq!(
        before,
        (snapshot(&repo.root), snapshot(&boxed.worktree)),
        "a sandboxed gate run touched the source"
    );
}

/// A replay rewrites the tree it runs in, and the recorded path is the only
/// thing that names one once Git has no answer. The same bound applies.
#[test]
fn a_sandboxed_rebase_refuses_the_recorded_checkout_outside_the_prefix() {
    let repo = Repo::new();
    let boxed = cloned_with_an_open_change(&repo, "outside-replay");
    // Something to replay, in the copy's own target branch.
    repo.commit(&boxed.clone, "moved.txt", "moved\n", "feat: move target");

    let before = (snapshot(&repo.root), snapshot(&boxed.worktree));
    let refusal = repo
        .arc(&boxed.clone)
        .env("ARC_SANDBOX", &boxed.prefix)
        .args(["rebase", "outside-replay"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&refusal.get_output().stderr).into_owned();
    assert!(
        stderr.contains(boxed.worktree.to_str().unwrap()),
        "the refusal should name the outside path: {stderr}"
    );
    assert!(stderr.contains("lies outside the sandbox"), "{stderr}");
    assert_eq!(
        before,
        (snapshot(&repo.root), snapshot(&boxed.worktree)),
        "a sandboxed rebase touched the source"
    );
}

/// `catchup` lists what a session can act on, and filing a spool is refused
/// from a worktree the sandbox does not admit. A row naming one would send the
/// reader to a directory arc will not write, so the display holds the same
/// boundary the promotion does.
#[test]
fn a_sandboxed_catchup_omits_a_spool_outside_the_prefix() {
    let repo = Repo::new();
    let boxed = cloned_with_an_open_change(&repo, "outside-spool");
    stdout(repo.arc(&boxed.worktree).args([
        "journal",
        "log",
        "sandboxed",
        "--spool",
        "waiting to be filed",
    ]));
    let outbox = boxed.worktree.join(".arc").join("outbox");

    let sandboxed = stdout(
        repo.arc(&boxed.clone)
            .env("ARC_SANDBOX", &boxed.prefix)
            .args(["catchup"]),
    );
    assert!(!sandboxed.contains("waiting spools"), "{sandboxed}");
    assert!(
        !sandboxed.contains(&outbox.display().to_string()),
        "{sandboxed}"
    );

    // The source admits its own worktree, so the same spool is still waiting.
    let source = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(source.contains("waiting spools (1)"), "{source}");
    assert!(source.contains(&outbox.display().to_string()), "{source}");
}

/// A marker is a file anyone can write, and being parseable is not evidence
/// that arc built the tree around it. Discard removes a directory only where
/// the marker and the directory agree about what is there.
#[test]
fn discard_refuses_a_marker_that_does_not_describe_its_directory() {
    let repo = Repo::new();
    let prefix = repo.home.join("boxed");
    repo.arc(&repo.root)
        .args(["sandbox", "clone", prefix.to_str().unwrap()])
        .assert()
        .success();
    let genuine: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(prefix.join(".arc-sandbox.json")).unwrap())
            .unwrap();

    // A fabricated marker over a directory arc never built: every path it
    // claims is real, and none of it is in the directory it sits in.
    let forged = repo.home.join("precious");
    fs::create_dir_all(&forged).unwrap();
    fs::write(forged.join("keepme"), "precious\n").unwrap();
    fs::write(
        forged.join(".arc-sandbox.json"),
        serde_json::to_vec_pretty(&genuine).unwrap(),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["sandbox", "discard", forged.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "does not describe this directory",
        ));
    assert!(forged.join("keepme").is_file());

    // Corrected to name this directory, the marker still describes a sandbox
    // that is not here: the repository it claims lies outside the prefix.
    let mut relabelled = genuine.clone();
    relabelled["prefix"] = serde_json::Value::String(forged.display().to_string());
    fs::write(
        forged.join(".arc-sandbox.json"),
        serde_json::to_vec_pretty(&relabelled).unwrap(),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["sandbox", "discard", forged.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("outside the prefix"));
    assert!(forged.join("keepme").is_file());

    // A marker claiming the home directory is refused before its own contents
    // are weighed: no sandbox is the home directory.
    let mut at_home = genuine.clone();
    at_home["prefix"] = serde_json::Value::String(repo.home.display().to_string());
    fs::write(
        repo.home.join(".arc-sandbox.json"),
        serde_json::to_vec_pretty(&at_home).unwrap(),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args(["sandbox", "discard", repo.home.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is the home directory"));
    assert!(repo.home.is_dir());

    // The sandbox arc did build still discards.
    repo.arc(&repo.root)
        .args(["sandbox", "discard", prefix.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("discarded"));
    assert!(!prefix.exists());
}

/// The same slug function the journal keys projects by, so a test names the
/// directory arc will choose rather than restating the rule.
fn config_path_slug(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}
