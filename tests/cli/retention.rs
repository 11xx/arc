//! A commit a verdict read, or that a patchset recorded as its head, stays
//! reachable after a rewrite moves the pin that held it.

use crate::common::*;
use crate::rewrite::{
    approved_change, arc_signing, intent_path, ref_snapshot, repo_with_gates, repository_events,
    signing_key,
};

const PLANNER: &str = "Planner <planner@example.invalid>";

/// The only change in the fixture's ledger.
fn only_change(repo: &Repo) -> String {
    let mut ids: Vec<String> = fs::read_dir(repo.root.join(".git/arc/changes"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "the fixture holds one change: {ids:?}");
    ids.pop().unwrap()
}

/// Every retention ref with the commit it holds.
fn retained(repo: &Repo) -> Vec<String> {
    git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/arc/retained/",
        ],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect()
}

fn retention_ref(change: &str, subject: &str, commit: &str) -> String {
    format!("refs/arc/retained/{change}/{subject}/{commit}")
}

fn has_object(cwd: &Path, commit: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", commit])
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", cwd.join(".git/fixture-global-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success()
}

/// What Git's garbage collection does to every object nothing reaches: expire
/// the reflogs that would otherwise hold the old commits, then prune.
fn collect_garbage(cwd: &Path) {
    git(
        cwd,
        &[
            "reflog",
            "expire",
            "--expire=now",
            "--expire-unreachable=now",
            "--all",
        ],
    );
    git(cwd, &["gc", "--prune=now", "--quiet"]);
}

fn trailer_args(from: &str) -> Vec<String> {
    [
        "rewrite",
        "trailers",
        "--from",
        from,
        "--no-sign",
        "--append",
        &format!("Planned-by: {PLANNER}"),
    ]
    .map(str::to_string)
    .to_vec()
}

fn rewrite_trailers(repo: &Repo, worktree: &Path, from: &str) {
    repo.arc(worktree)
        .args(trailer_args(from))
        .assert()
        .success();
}

/// The `retained` map of the one rewrite event the ledger holds.
fn recorded_retention(repo: &Repo) -> serde_json::Value {
    let events = repository_events(repo);
    assert_eq!(events.len(), 1, "one rewrite is recorded");
    let event: serde_json::Value = serde_json::from_slice(&fs::read(&events[0]).unwrap()).unwrap();
    event["retained"].clone()
}

#[test]
fn a_trailer_rewrite_keeps_the_reviewed_commit_reachable() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    let pin = format!("refs/arc/keep/{change}/ps-01");
    assert_eq!(git_out(&worktree, &["rev-parse", &pin]), old);

    rewrite_trailers(&repo, &worktree, &old);

    let new = repo.head(&worktree);
    assert_ne!(new, old);
    let name = retention_ref(&change, "ps-01", &old);
    assert_eq!(
        git_out(&worktree, &["rev-parse", &pin]),
        new,
        "the pin keeps following the successor"
    );
    assert_eq!(
        retained(&repo),
        vec![format!("{name} {old}")],
        "retention holds the commit that was read and nothing else"
    );
    assert_eq!(recorded_retention(&repo)[&name], old.as_str());

    collect_garbage(&worktree);
    assert!(has_object(&worktree, &old), "garbage collection kept it");
    assert!(has_object(&worktree, &new));
}

#[test]
fn a_signing_rewrite_keeps_the_reviewed_commit_reachable() {
    let repo = repo_with_gates();
    let key = signing_key(&repo).expect("signing acceptance requires a sandbox signing key");
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);

    arc_signing(&repo, &key, &worktree)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .assert()
        .success();

    let new = repo.head(&worktree);
    assert_ne!(new, old);
    let name = retention_ref(&change, "ps-01", &old);
    assert_eq!(retained(&repo), vec![format!("{name} {old}")]);
    assert_eq!(recorded_retention(&repo)[&name], old.as_str());
    assert_eq!(
        git_out(
            &worktree,
            &["rev-parse", &format!("refs/arc/keep/{change}/ps-01")]
        ),
        new
    );

    collect_garbage(&worktree);
    assert!(has_object(&worktree, &old), "garbage collection kept it");
}

#[test]
fn a_dry_run_names_the_retention_and_creates_none() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    let before = ref_snapshot(&worktree);

    let out = stdout(
        repo.arc(&worktree)
            .args(trailer_args(&old))
            .arg("--dry-run"),
    );

    assert!(
        out.contains(&format!(
            "would retain: {}",
            retention_ref(&change, "ps-01", &old)
        )),
        "{out}"
    );
    assert_eq!(ref_snapshot(&worktree), before);
}

#[test]
fn a_rolled_back_rewrite_leaves_no_retention_ref() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    let before = ref_snapshot(&worktree);
    let directory = repo.root.join(".git/arc/repository/events");
    fs::create_dir_all(&directory).unwrap();
    let release = repo.home.join("release-rewrite");
    let out = thread::scope(|scope| {
        let blocker = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while repo.head(&worktree) == old {
                assert!(
                    Instant::now() < deadline,
                    "the rewrite never moved its refs"
                );
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                retained(&repo),
                vec![format!("{} {old}", retention_ref(&change, "ps-01", &old))],
                "retention lands in the transaction that moves the refs"
            );
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
            fs::write(&release, "").unwrap();
        });
        let out = repo
            .arc(&worktree)
            .args(trailer_args(&old))
            .env("ARC_REWRITE_PAUSE", &release)
            .output()
            .unwrap();
        blocker.join().unwrap();
        out
    });
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(!out.status.success());
    assert!(retained(&repo).is_empty(), "{:?}", retained(&repo));
    assert_eq!(ref_snapshot(&worktree), before);
    assert!(intent_path(&repo).exists());

    rewrite_trailers(&repo, &worktree, &old);
    assert_eq!(
        retained(&repo),
        vec![format!("{} {old}", retention_ref(&change, "ps-01", &old))]
    );
    assert!(!intent_path(&repo).exists());
}

/// A rewrite finished by the next run holds the same retention a rewrite that
/// was never interrupted does, whichever phase the interruption fell after.
#[test]
fn an_interrupted_rewrite_retains_once_whenever_it_is_finished() {
    for phase in ["intent", "refs", "record"] {
        let repo = repo_with_gates();
        let (worktree, old) = approved_change(&repo, "alpha");
        let change = only_change(&repo);
        let name = retention_ref(&change, "ps-01", &old);
        repo.arc(&worktree)
            .args(trailer_args(&old))
            .env("ARC_REWRITE_INTERRUPT", phase)
            .assert()
            .failure();
        assert!(intent_path(&repo).exists(), "{phase}");
        let held = retained(&repo);
        if phase == "intent" {
            assert!(held.is_empty(), "nothing is created before the refs move");
        } else {
            assert_eq!(held, vec![format!("{name} {old}")], "{phase}");
        }

        rewrite_trailers(&repo, &worktree, &old);

        assert_eq!(retained(&repo), vec![format!("{name} {old}")], "{phase}");
        assert_eq!(recorded_retention(&repo)[&name], old.as_str(), "{phase}");
        assert!(!intent_path(&repo).exists(), "{phase}");
        collect_garbage(&worktree);
        assert!(has_object(&worktree, &old), "{phase}");
    }
}

#[test]
fn a_retained_commit_never_follows_a_later_rewrite() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    rewrite_trailers(&repo, &worktree, &old);
    let new = repo.head(&worktree);
    // Back onto the history the retention ref holds, and the same edit again:
    // the map names `old` once more, and the ref holding it must stay put.
    git(&worktree, &["reset", "--hard", &old]);

    rewrite_trailers(&repo, &worktree, &old);

    assert_eq!(repo.head(&worktree), new);
    assert_eq!(
        retained(&repo),
        vec![format!("{} {old}", retention_ref(&change, "ps-01", &old))]
    );
}

#[test]
fn a_verdict_revision_no_pin_holds_is_retained_under_its_event() {
    let repo = repo_with_gates();
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "alpha"])));
    let worktree = repo.home.join(".worktrees/repo-alpha");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    let first = repo.head(&worktree);
    repo.commit(&worktree, "two.txt", "two\n", "feat: two");
    let head = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", "alpha"]));
    let recorded = stdout(repo.arc(&worktree).args([
        "external",
        "verdict",
        "alpha",
        "--verdict",
        "approved",
        "--decided-by",
        "Upstream Maintainer",
        "--reference",
        "https://example.invalid/pull/7",
        "--revision",
        &first,
    ]));
    let event = recorded
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap();

    rewrite_trailers(&repo, &worktree, &first);

    let mut held = retained(&repo);
    held.sort();
    let mut expected = vec![
        format!("{} {head}", retention_ref(&change, "ps-01", &head)),
        format!("{} {first}", retention_ref(&change, event, &first)),
    ];
    expected.sort();
    assert_eq!(held, expected);
    collect_garbage(&worktree);
    assert!(has_object(&worktree, &first));
    assert!(has_object(&worktree, &head));
}

#[test]
fn recording_a_rewrite_made_elsewhere_retains_the_commits_the_ledger_cites() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    git(
        &worktree,
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--date",
            "2020-01-01T00:00:00Z",
        ],
    );
    let new = repo.head(&worktree);
    assert_ne!(new, old);
    // The tool that rewrote the branch carried the pin with it.
    git(
        &worktree,
        &["update-ref", &format!("refs/arc/keep/{change}/ps-01"), &new],
    );
    let map = repo.home.join("map");
    fs::write(&map, format!("{old} {new}\n")).unwrap();

    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "amended elsewhere",
        ])
        .assert()
        .success();

    let name = retention_ref(&change, "ps-01", &old);
    assert_eq!(retained(&repo), vec![format!("{name} {old}")]);
    assert_eq!(recorded_retention(&repo)[&name], old.as_str());
    collect_garbage(&worktree);
    assert!(has_object(&worktree, &old));
}

#[test]
fn a_cited_commit_already_gone_is_reported_not_retained() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    git(
        &worktree,
        &[
            "commit",
            "--amend",
            "--no-edit",
            "--date",
            "2020-01-01T00:00:00Z",
        ],
    );
    let new = repo.head(&worktree);
    git(
        &worktree,
        &["update-ref", &format!("refs/arc/keep/{change}/ps-01"), &new],
    );
    collect_garbage(&worktree);
    assert!(!has_object(&worktree, &old));
    let map = repo.home.join("map");
    fs::write(&map, format!("{old} {new}\n")).unwrap();

    let out = stdout(repo.arc(&repo.root).args([
        "history",
        "rewrite",
        "--map",
        map.to_str().unwrap(),
        "--reason",
        "amended elsewhere",
    ]));

    assert!(out.contains("not retained"), "{out}");
    assert!(out.contains(&old[..8]), "{out}");
    assert!(retained(&repo).is_empty());
}
