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

budget!(begin, 15);
budget!(status, 11);
budget!(check, 12);
budget!(snapshot, 12);
budget!(verify, 65);
budget!(review, 27);
budget!(integrate, 86);
budget!(rebase, 11);
budget!(journal, 2);
budget!(catchup, 23);

/// Claim: status, verification and integration launch no extra Git processes
/// for unrelated changes, refs and worktrees at an already observed revision.
/// Rejects per-item Git questions when their answers are identical. Distinct
/// immutable revisions may require their own object reads.
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
