use super::common::*;

fn counted(repo: &Repo, args: &[&str]) -> u64 {
    let output = repo
        .arc(&repo.root)
        .env("ARC_TRACE", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success() || (args[0] == "check" && output.status.code() == Some(3)),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let trace: serde_json::Value = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|v| {
            v["schema"]
                .as_str()
                .is_some_and(|s| s.starts_with("arc-perf/"))
        })
        .unwrap();
    trace["children"]["by_kind"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, _)| name.as_str() == "git" || name.starts_with("git "))
        .map(|(_, value)| value["runs"].as_u64().unwrap())
        .sum()
}

fn launches(command: &str, ambient: usize) -> u64 {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[budget]\ntotal = '2s'\n[gates.alpha]\ncommand = 'true'\nbudget = '2s'\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "test: declare gate"]);
    for index in 0..ambient {
        let slug = format!("ambient-{index}");
        let checkout = repo.root.parent().unwrap().join(&slug);
        repo.arc(&repo.root)
            .args(["begin", &slug, "--worktree"])
            .arg(checkout)
            .assert()
            .success();
        git(
            &repo.root,
            &["update-ref", &format!("refs/ambient/{index}"), "HEAD"],
        );
    }
    let run = |args: &[&str]| counted(&repo, args);
    let count = run(&["begin", "probe", "--no-worktree"]);
    if command == "begin" {
        return count;
    }
    repo.commit(&repo.root, "probe.txt", "probe\n", "test: add probe");
    let count = run(&["snapshot", "probe"]);
    if command == "snapshot" {
        return count;
    }
    if matches!(command, "status" | "check") {
        return run(&[command, "probe"]);
    }
    let count = run(&["verify", "probe", "--all"]);
    if command == "verify" {
        return count;
    }
    let count = run(&["review", "probe", "--verdict", "approved"]);
    if command == "review" {
        return count;
    }
    if command == "journal" {
        return run(&["journal", "memory", "probe", "--body-file", "README.md"]);
    }
    if command == "catchup" {
        return run(&["catchup", "--json"]);
    }
    let count = run(&["rebase", "probe"]);
    if command == "rebase" {
        return count;
    }
    run(&["verify", "probe", "--all"]);
    run(&["review", "probe", "--verdict", "approved"]);
    git(&repo.root, &["checkout", "master"]);
    run(&["integrate", "probe"])
}

macro_rules! budget {
    ($name:ident, $ceiling:expr) => {
        #[doc = concat!("Claim: `", stringify!($name), "` stays within its Git launch ceiling on a complete fixture lifecycle. Rejects additional Git processes for questions already answered in this invocation.")]
        #[test]
        fn $name() {
            let launches = launches(stringify!($name), 0);
            eprintln!("{}: {launches} Git processes", stringify!($name));
            assert!(launches <= $ceiling, "{}: {launches} Git processes exceed {}", stringify!($name), $ceiling);
        }
    };
}

budget!(begin, 11);
budget!(status, 9);
budget!(check, 10);
budget!(snapshot, 9);
budget!(verify, 38);
budget!(review, 13);
budget!(integrate, 65);
budget!(rebase, 9);
budget!(journal, 2);
budget!(catchup, 14);

/// Claim: status, verification and integration launch no extra Git processes
/// for unrelated changes, refs and worktrees at an already observed revision.
/// Rejects per-item Git questions when their answers are identical.
#[test]
fn unrelated_changes_refs_and_worktrees_do_not_add_git_launches() {
    let mut regressions = Vec::new();
    for command in ["status", "verify", "integrate"] {
        let single = launches(command, 0);
        let several = launches(command, 4);
        eprintln!("{command}: single={single}, several={several}");
        if several > single {
            regressions.push(format!("{command}: {} extra launches", several - single));
        }
    }
    assert!(
        regressions.is_empty(),
        "unrelated changes, refs and worktrees add Git launches: {regressions:?}"
    );
}

fn launches_with_history(command: &str, historical: usize) -> u64 {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[budget]\ntotal = '2s'\n[gates.alpha]\ncommand = 'true'\nbudget = '2s'\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "test: declare gate"]);
    for index in 0..historical {
        let slug = format!("history-{index}");
        repo.arc(&repo.root)
            .args(["begin", &slug, "--no-worktree"])
            .assert()
            .success();
        repo.commit(
            &repo.root,
            &format!("history-{index}.txt"),
            &format!("history {index}\n"),
            &format!("test: historical commit {index}"),
        );
        repo.arc(&repo.root)
            .args(["snapshot", &slug])
            .assert()
            .success();
        repo.arc(&repo.root)
            .args(["verify", &slug, "--all"])
            .assert()
            .success();
        repo.arc(&repo.root)
            .args(["review", &slug, "--verdict", "approved"])
            .assert()
            .success();
        git(&repo.root, &["checkout", "master"]);
        repo.arc(&repo.root)
            .args(["integrate", &slug])
            .assert()
            .success();
    }
    let run = |args: &[&str]| counted(&repo, args);
    let count = run(&["begin", "probe", "--no-worktree"]);
    if command == "begin" {
        return count;
    }
    repo.commit(&repo.root, "probe.txt", "probe\n", "test: add probe");
    run(&["snapshot", "probe"]);
    if command == "status" {
        return run(&["status", "probe"]);
    }
    let count = run(&["verify", "probe", "--all"]);
    if command == "verify" {
        return count;
    }
    let count = run(&["review", "probe", "--verdict", "approved"]);
    if command == "review" {
        return count;
    }
    if command == "catchup" {
        return run(&["catchup", "--json"]);
    }
    git(&repo.root, &["checkout", "master"]);
    run(&["integrate", "probe"])
}

/// Claim: closed changes integrated at distinct historical commits add no
/// extra Git process launches to `begin`, `status`, `review`, `verify`,
/// `integrate`, or `catchup`. Rejects per-commit `rev-parse` loops during
/// ledger replay and review-context loading.
#[test]
fn distinct_historical_revisions_do_not_add_git_launches() {
    let mut regressions = Vec::new();
    for (command, base_history) in [
        ("begin", 0),
        ("status", 0),
        ("review", 0),
        ("verify", 0),
        ("integrate", 0),
        // `catchup` runs one batched `for-each-ref` divergence check once any
        // non-open branch exists; compare 1 vs 6 historical changes to verify
        // launches stay O(1) across distinct historical revisions.
        ("catchup", 1),
    ] {
        let baseline = launches_with_history(command, base_history);
        let with_history = launches_with_history(command, 6);
        eprintln!("{command}: history{base_history}={baseline}, history6={with_history}");
        if with_history > baseline {
            regressions.push(format!(
                "{command}: {} extra launches across {} added historical commits",
                with_history - baseline,
                6 - base_history
            ));
        }
    }
    assert!(
        regressions.is_empty(),
        "distinct historical revisions add Git launches: {regressions:?}"
    );
}
