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

fn doctor(repo: &Repo) -> serde_json::Value {
    json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]))
}

fn finding<'a>(report: &'a serde_json::Value, list: &str, code: &str) -> Option<&'a str> {
    report[list]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == code)
        .map(|finding| finding["detail"].as_str().unwrap())
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

#[test]
fn doctor_names_a_retained_commit_whose_object_is_gone() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    rewrite_trailers(&repo, &worktree, &old);
    // Another cited head keeps the reachability walk nonempty.
    repo.arc(&worktree)
        .args(["snapshot", "alpha"])
        .assert()
        .success();
    let clean = doctor(&repo);
    assert!(
        finding(&clean, "problems", "retained-object-missing").is_none(),
        "{clean}"
    );

    fs::remove_file(
        repo.root
            .join(format!(".git/objects/{}/{}", &old[..2], &old[2..])),
    )
    .unwrap();

    let report = doctor(&repo);
    let detail = finding(&report, "problems", "retained-object-missing")
        .unwrap_or_else(|| panic!("{report}"));
    assert!(
        detail.contains(&retention_ref(&change, "ps-01", &old)),
        "{detail}"
    );
    let reachability = finding(&report, "problems", "retention-reachability-failed")
        .unwrap_or_else(|| panic!("{report}"));
    assert!(reachability.contains("bad object"), "{reachability}");
    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("retained-object-missing"))
        .stdout(predicates::str::contains("retention-reachability-failed"));
}

#[test]
fn doctor_reports_a_patchset_head_no_ref_reaches() {
    let repo = repo_with_gates();
    let (change, worktree, head) = change_with_patchset(&repo, "alpha");
    let clean = doctor(&repo);
    assert!(
        finding(&clean, "advice", "unretained-commit").is_none(),
        "{clean}"
    );

    git(&worktree, &["reset", "--hard", "HEAD~1"]);
    git(
        &repo.root,
        &["update-ref", "-d", &format!("refs/arc/keep/{change}/ps-01")],
    );

    let report = doctor(&repo);
    let detail =
        finding(&report, "advice", "unretained-commit").unwrap_or_else(|| panic!("{report}"));
    assert!(detail.contains(&head[..8]), "{detail}");
    assert!(detail.contains("ps-01 head"), "{detail}");
    assert!(
        detail.contains(&format!(
            "git update-ref {}",
            retention_ref(&change, "ps-01", &head)
        )),
        "the report names the command that retains it: {detail}"
    );
}

#[test]
fn an_export_lists_the_retained_commit_and_an_import_retains_it() {
    let source = repo_with_gates();
    let (worktree, old) = approved_change(&source, "alpha");
    let change = only_change(&source);
    rewrite_trailers(&source, &worktree, &old);
    let bundle = source.home.join("bundle.json");
    let objects = source.home.join("objects.json");
    source
        .arc(&worktree)
        .args([
            "export",
            "alpha",
            "--output",
            bundle.to_str().unwrap(),
            "--objects-json",
            objects.to_str().unwrap(),
        ])
        .assert()
        .success();
    let listed: serde_json::Value = serde_json::from_slice(&fs::read(&objects).unwrap()).unwrap();
    assert!(
        listed["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|object| object["oid"] == old.as_str() && object["role"] == "retention-commit"),
        "{listed}"
    );

    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    receiver
        .arc(&receiver.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();

    assert_eq!(
        retained(&receiver),
        vec![format!("{} {old}", retention_ref(&change, "ps-01", &old))]
    );
}

#[test]
fn an_import_without_retention_still_works() {
    let source = repo_with_gates();
    let (worktree, old) = approved_change(&source, "alpha");
    rewrite_trailers(&source, &worktree, &old);
    let bundle = source.home.join("bundle.json");
    source
        .arc(&worktree)
        .args(["export", "alpha", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    value["objects"]
        .as_array_mut()
        .unwrap()
        .retain(|object| object["role"] != "retention-commit");
    for event in value["repository_events"].as_array_mut().unwrap() {
        event.as_object_mut().unwrap().remove("retained");
    }
    fs::write(&bundle, json_file_bytes(&value)).unwrap();

    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);
    receiver
        .arc(&receiver.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();

    assert!(retained(&receiver).is_empty());
}

#[test]
fn an_import_writes_no_ref_a_bundle_names_outside_retention() {
    let source = repo_with_gates();
    let (worktree, old) = approved_change(&source, "alpha");
    let change = only_change(&source);
    rewrite_trailers(&source, &worktree, &old);
    let bundle = source.home.join("bundle.json");
    source
        .arc(&worktree)
        .args(["export", "alpha", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    let receiver = Repo::new();
    deliver_git_objects(&source, &receiver);

    let mut hostile = original.clone();
    hostile["repository_events"][0]["retained"] = serde_json::json!({
        "refs/heads/master": old,
    });
    fs::write(&bundle, json_file_bytes(&hostile)).unwrap();
    let before = ref_snapshot(&receiver.root);
    receiver
        .arc(&receiver.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a retention ref"));
    assert_eq!(ref_snapshot(&receiver.root), before);

    // A name that is under the change but does not say the commit it holds is
    // not a ref this repository would have made, and is not made.
    let mut mismatched = original.clone();
    mismatched["repository_events"][0]["retained"] = serde_json::json!({
        format!("refs/arc/retained/{change}/ps-01/{}", "a".repeat(40)): old,
    });
    fs::write(&bundle, json_file_bytes(&mismatched)).unwrap();
    receiver
        .arc(&receiver.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    assert!(retained(&receiver).is_empty());
}

#[test]
fn integration_releases_the_pin_and_keeps_the_retained_commit() {
    let repo = repo_with_gates();
    let key = signing_key(&repo).expect("signing acceptance requires a sandbox signing key");
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    arc_signing(&repo, &key, &worktree)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["integrate", "alpha"])
        .assert()
        .success();

    assert_eq!(
        git_out(
            &repo.root,
            &["for-each-ref", "--format=%(refname)", "refs/arc/keep/"]
        ),
        "",
        "the pin is released once its head reaches the integration commit"
    );
    assert_eq!(
        retained(&repo),
        vec![format!("{} {old}", retention_ref(&change, "ps-01", &old))],
        "the commit the verdict read is on no branch and stays held"
    );
    collect_garbage(&repo.root);
    assert!(has_object(&repo.root, &old));
}

#[test]
fn closing_without_integration_releases_nothing() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    rewrite_trailers(&repo, &worktree, &old);
    let before = ref_snapshot(&repo.root);

    repo.arc(&repo.root)
        .args(["close", "alpha", "--abandoned"])
        .assert()
        .success();

    assert_eq!(ref_snapshot(&repo.root), before);
    assert_eq!(
        retained(&repo),
        vec![format!("{} {old}", retention_ref(&change, "ps-01", &old))]
    );
}

/// A retained commit that integration reaches is held by history, so its ref
/// is released with the pins whose heads integration reaches.
#[test]
fn a_retained_commit_the_integration_reaches_is_released() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    rewrite_trailers(&repo, &worktree, &old);
    let new = repo.head(&worktree);
    git(&repo.root, &["merge", "--ff-only", &old]);
    git(&repo.root, &["merge", "--no-edit", &new]);
    let merged = repo.head(&repo.root);

    repo.arc(&repo.root)
        .args(["close", "alpha", "--assert-integrated", &merged])
        .assert()
        .success();

    assert!(retained(&repo).is_empty(), "{:?}", retained(&repo));
    assert_eq!(
        git_out(
            &repo.root,
            &["for-each-ref", "--format=%(refname)", "refs/arc/keep/"]
        ),
        ""
    );
}

#[test]
fn withdrawing_a_map_keeps_what_it_retained() {
    let repo = repo_with_gates();
    let (worktree, old) = approved_change(&repo, "alpha");
    let change = only_change(&repo);
    rewrite_trailers(&repo, &worktree, &old);
    let event = repository_events(&repo)[0]
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let name = retention_ref(&change, "ps-01", &old);

    repo.arc(&repo.root)
        .args(["history", "withdraw", &event, "--reason", "wrong map"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "withdrawn map retained: {name} {old}"
        )));

    assert_eq!(retained(&repo), vec![format!("{name} {old}")]);
}

/// Fail only the external history event's publication, leaving Git's ref
/// transactions and the fixture's setup writes alone.
fn history_publication_fault(repo: &Repo) -> PathBuf {
    let source = repo.home.join("publication-fault.c");
    let library = repo.home.join("publication-fault.so");
    fs::write(
        &source,
        r#"#define _GNU_SOURCE
#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
int fsync(int fd) {
    static int (*original)(int);
    if (!original) original = dlsym(RTLD_NEXT, "fsync");
    char link[64], path[4096];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof(path) - 1);
    if (n < 0) return original(fd);
    path[n] = 0;
    const char *mode = getenv("HISTORY_PUBLICATION_FAULT");
    if (!mode || !strstr(path, "/repository/events")) return original(fd);
    struct stat info;
    if (fstat(fd, &info)) return original(fd);
    int before = !strcmp(mode, "absent");
    if (before == S_ISDIR(info.st_mode)) return original(fd);
    if (!strcmp(mode, "uncertain")) {
        DIR *dir = opendir(path);
        struct dirent *entry;
        while (dir && (entry = readdir(dir))) {
            if (!strstr(entry->d_name, ".json")) continue;
            char event[8192];
            snprintf(event, sizeof(event), "%s/%s", path, entry->d_name);
            chmod(event, 0);
        }
        if (dir) closedir(dir);
    }
    errno = EIO;
    return -1;
}
"#,
    )
    .unwrap();
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    library
}

#[test]
fn external_map_publication_failure_withdraws_only_confirmed_absent_retention() {
    use std::os::unix::fs::PermissionsExt;
    for mode in ["published", "uncertain", "absent", "unissued"] {
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
        let library = history_publication_fault(&repo);
        let id_lock = if mode == "unissued" {
            let lock = fs::File::options()
                .read(true)
                .write(true)
                .open(repo.root.join(".git/arc/locks/event-ids.lock"))
                .unwrap();
            lock.lock().unwrap();
            Some(lock)
        } else {
            None
        };
        let failed = repo
            .arc(&repo.root)
            .args([
                "history",
                "rewrite",
                "--map",
                "-",
                "--reason",
                "amended elsewhere",
            ])
            .write_stdin(format!("{old} {new}\n"))
            .env("LD_PRELOAD", library)
            .env("HISTORY_PUBLICATION_FAULT", mode)
            .output()
            .unwrap();
        assert!(!failed.status.success(), "{mode}: {failed:?}");
        drop(id_lock);
        let stderr = String::from_utf8_lossy(&failed.stderr);
        if mode == "unissued" {
            assert!(
                stderr.contains("ledger event IDs lock") && stderr.contains("is busy"),
                "{failed:?}"
            );
        } else {
            assert!(stderr.contains("Input/output error"), "{failed:?}");
        }
        let publication = match mode {
            "absent" | "unissued" => {
                "event is absent; retention refs from this attempt were withdrawn"
            }
            "published" => "is published; retention refs are preserved",
            "uncertain" => "publication is uncertain",
            _ => unreachable!(),
        };
        assert!(stderr.contains(publication), "{failed:?}");
        if mode == "absent" || mode == "unissued" {
            assert!(repository_events(&repo).is_empty());
            assert!(retained(&repo).is_empty());
        } else {
            let events = repository_events(&repo);
            assert_eq!(events.len(), 1, "{mode}");
            fs::set_permissions(&events[0], fs::Permissions::from_mode(0o600)).unwrap();
            let name = retention_ref(&change, "ps-01", &old);
            assert_eq!(retained(&repo), vec![format!("{name} {old}")], "{mode}");
            assert_eq!(recorded_retention(&repo)[&name], old.as_str());
            collect_garbage(&worktree);
            assert!(has_object(&worktree, &old), "{mode}");
        }
    }
}

#[test]
fn a_sha256_external_map_abbreviation_retains_the_full_cited_commit() {
    let repo = Repo::with_object_format("sha256");
    let (change, worktree, old) = change_with_patchset(&repo, "alpha");
    assert_eq!(old.len(), 64);
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
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            "-",
            "--reason",
            "amended elsewhere",
        ])
        .write_stdin(format!("{} {new}\n", &old[..48]))
        .assert()
        .success();
    assert!(stdout(repo.arc(&repo.root).args(["history", "resolve", &old])).contains(&new));
    let name = retention_ref(&change, "ps-01", &old);
    assert_eq!(retained(&repo), vec![format!("{name} {old}")]);
    assert_eq!(recorded_retention(&repo)[&name], old.as_str());
    collect_garbage(&worktree);
    assert!(has_object(&worktree, &old));
}
