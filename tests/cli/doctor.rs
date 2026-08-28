use super::common::*;

/// Doctor's job is to report malformed state, so a malformed repository event
/// must be a finding rather than a fatal error raised by whichever inspection
/// happened to read it first.
#[test]
fn a_malformed_repository_event_is_reported_rather_than_fatal() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "readable", "--no-worktree"]),
    );
    let dir = repo.root.join(".git/arc/repository/events");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("01BADBADBADBADBADBADBADBAD.json"), "not json\n").unwrap();

    let out = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert!(out.contains("malformed-repository-event"), "{out}");

    // An ID no write path could have produced is reported too, once, rather
    // than passing because nothing on the read path looked at it.
    fs::write(dir.join("bad name.json"), "{}\n").unwrap();
    let out = stdout(repo.arc(&repo.root).args(["doctor"]));
    // Both broken files are reported: one unreadable file must not hide the
    // state of another, which is the point of the report.
    assert!(out.contains("01BADBADBADBADBADBADBADBAD"), "{out}");
    assert_eq!(
        out.lines().filter(|line| line.contains("bad name")).count(),
        1,
        "one finding per broken file: {out}"
    );

    // A well-named file whose contents are not an event is one finding too,
    // not one per field the checks below it would have read.
    fs::write(dir.join("01VALIDBUTEMPTY0000000000.json"), "{}\n").unwrap();
    let out = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert_eq!(
        out.lines()
            .filter(|line| line.contains("01VALIDBUTEMPTY0000000000"))
            .count(),
        1,
        "one finding per broken file: {out}"
    );
}

#[test]
fn doctor_clean_ledger_exits_zero() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("problems:\n  (none)"));
    repo.arc(&repo.root)
        .args(["doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"schema\":\"arc-doctor/1\""));
}

#[test]
fn doctor_reports_malformed_event_as_problem() {
    let repo = Repo::new();
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "doctor-bad-event", "--no-worktree"]),
    );
    let change_id = opened_change_id(&output);
    fs::write(event_dir(&repo, &change_id).join("BAD.json"), b"not json\n").unwrap();

    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("malformed-event"));
}

#[test]
fn doctor_reports_orphaned_tmp_as_advice_without_failing() {
    let repo = Repo::new();
    let output = stdout(
        repo.arc(&repo.root)
            .args(["begin", "doctor-tmp", "--no-worktree"]),
    );
    let change_id = opened_change_id(&output);
    let temporary = event_dir(&repo, &change_id).join(".event.TEST.tmp");
    fs::write(&temporary, b"partial").unwrap();

    repo.arc(&repo.root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("orphaned-temporary-file"));
    assert!(temporary.is_file(), "doctor must be read-only");
}

pub(crate) fn doctor_groups_advice_and_ignores_closed_claims() {
    let repo = Repo::new();
    let expired_claim = |slug: &str| {
        let change_id = begin_no_worktree(&repo, slug, &[]);
        repo.arc(&repo.root)
            .args(["claim", slug, "--ttl", "1s"])
            .assert()
            .success();
        age_event(&repo, &change_id, "claim-set", 5);
        change_id
    };
    let first = expired_claim("doctor-open-one");
    let second = expired_claim("doctor-open-two");
    let closed = expired_claim("doctor-closed");
    repo.arc(&repo.root)
        .args(["close", "doctor-closed", "--abandoned"])
        .assert()
        .success();

    let default = stdout(repo.arc(&repo.root).arg("doctor"));
    assert_eq!(
        default.matches("long-expired-claim").count(),
        1,
        "{default}"
    );
    assert!(
        default.contains(
            "long-expired-claim: 2 open changes have claims expired for more than one TTL; \
             run arc doctor --verbose to identify them"
        ),
        "{default}"
    );
    assert!(!default.contains(&first), "{default}");
    assert!(!default.contains(&second), "{default}");
    assert!(!default.contains(&closed), "{default}");

    let verbose = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    assert_eq!(
        verbose.matches("long-expired-claim").count(),
        2,
        "{verbose}"
    );
    assert!(verbose.contains(&first), "{verbose}");
    assert!(verbose.contains(&second), "{verbose}");
    assert!(!verbose.contains(&closed), "{verbose}");

    let json = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    let claims = json["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "long-expired-claim")
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 2);
    assert!(claims
        .iter()
        .all(|finding| !finding["detail"].as_str().unwrap().contains(&closed)));

    repo.arc(&repo.root)
        .args(["doctor", "--verbose", "--json"])
        .assert()
        .failure()
        .code(2);
}

/// What the open changes' worktrees occupy is reported by `catchup` and
/// `doctor` without being asked: disk is the resource `begin` spends and
/// nothing reported, and a full filesystem fails as exit 0 elsewhere.
#[test]
fn catchup_and_doctor_report_open_worktree_usage() {
    let repo = Repo::new();
    // The shared begin_change helper runs --no-worktree; this surface needs
    // real worktrees to account for.
    begin_change_no_helper(&repo, "usage-one");
    begin_change_no_helper(&repo, "usage-two");

    let catchup = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(catchup.contains("worktrees: "), "{catchup}");
    assert!(catchup.contains("2 open worktree(s)"), "{catchup}");
    assert!(catchup.contains("usage-one"), "{catchup}");
    assert!(catchup.contains("repo-usage-two"), "{catchup}");

    let doctor = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    assert!(doctor.contains("open-worktree-usage: "), "{doctor}");
    assert!(doctor.contains("open-worktree-usage-total"), "{doctor}");

    // A ledger with no open worktrees stays silent on the subject.
    let cleaned = Repo::new();
    let quiet = stdout(cleaned.arc(&cleaned.root).args(["catchup"]));
    assert!(!quiet.contains("worktrees: "), "{quiet}");
}

fn begin_change_no_helper(repo: &Repo, slug: &str) {
    repo.arc(&repo.root)
        .args(["begin", slug])
        .assert()
        .success();
}

pub(crate) fn doctor_reports_closed_registered_worktrees_without_removing_them() {
    let repo = Repo::new();
    let close_with_worktree = |slug: &str| {
        let opened = stdout(repo.arc(&repo.root).args(["begin", slug]));
        let change_id = opened_change_id(&opened);
        let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
        repo.arc(&repo.root)
            .args(["close", slug, "--abandoned"])
            .assert()
            .success();
        (change_id, worktree)
    };
    let (first_id, first_path) = close_with_worktree("doctor-closed-one");
    let (second_id, second_path) = close_with_worktree("doctor-closed-two");
    let (removed_id, removed_path) = close_with_worktree("doctor-closed-removed");
    git(
        &repo.root,
        &["worktree", "remove", removed_path.to_str().unwrap()],
    );

    let registrations_before = git_out(&repo.root, &["worktree", "list", "--porcelain"]);
    let default = stdout(repo.arc(&repo.root).arg("doctor"));
    assert!(
        default.contains(
            "closed-change-worktree: 2 registered worktrees belong to closed changes; \
             run arc doctor --verbose to list change/path pairs; remove only with \
             git worktree remove <path>"
        ),
        "{default}"
    );
    assert!(!default.contains(&first_id), "{default}");
    assert!(!default.contains(&second_id), "{default}");
    assert!(!default.contains(&removed_id), "{default}");

    let verbose = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    for (change_id, path) in [(&first_id, &first_path), (&second_id, &second_path)] {
        assert!(
            verbose.contains(&format!(
                "closed-change-worktree: {change_id} [abandoned]: {}",
                path.display()
            )),
            "{verbose}"
        );
    }
    assert!(!verbose.contains(&removed_id), "{verbose}");
    assert!(first_path.is_dir());
    assert!(second_path.is_dir());
    assert_eq!(
        git_out(&repo.root, &["worktree", "list", "--porcelain"]),
        registrations_before
    );
}

/// A history rewrite leaves the ledger intact and its evidence unreachable:
/// every recorded revision still says what was verified, and none of it can be
/// checked out. Patchset heads survive because arc keeps a retention ref for
/// each; everything else it records — a verification revision, a brief base —
/// has nothing holding it. The ledger is not malformed, so this is advice, but
/// it is the difference between evidence and a claim.
#[test]
fn doctor_reports_a_recorded_revision_git_can_no_longer_resolve() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "rewritten"]));
    let wt = repo.home.join(".worktrees/repo-rewritten");
    repo.commit(&wt, "work.rs", "first\n", "feat: first");
    let recorded = repo.head(&wt);
    repo.arc(&wt)
        .args(["verify", "rewritten", "--command", "true"])
        .assert()
        .success();

    // Rewrite the branch out from under the recorded evidence, as an amend or
    // a rebase would.
    git(&wt, &["reset", "--hard", "HEAD~1"]);
    git(&wt, &["reflog", "expire", "--expire=now", "--all"]);
    git(&repo.root, &["reflog", "expire", "--expire=now", "--all"]);
    git(&repo.root, &["gc", "--prune=now", "--quiet"]);

    let report = json_stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    let dangling: Vec<&serde_json::Value> = report["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["code"] == "dangling-revision")
        .collect();
    assert!(!dangling.is_empty(), "{report}");
    assert!(
        dangling
            .iter()
            .any(|item| item["detail"].as_str().unwrap().contains(&recorded[..8])),
        "{report}"
    );
    // Advice never fails the command: the ledger is not malformed.
    repo.arc(&repo.root).args(["doctor"]).assert().success();
}

/// A rewrite is a fact about the repository, so it is recorded rather than
/// applied. Every event keeps saying exactly what it said; what changes is
/// that a reader can follow a recorded revision forward, and that doctor stops
/// calling a moved revision a casualty.
#[test]
fn a_recorded_rewrite_translates_revisions_instead_of_migrating_events() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "rewritten"]));
    let worktree = repo.home.join(".worktrees/repo-rewritten");
    repo.commit(&worktree, "work.rs", "one\n", "feat: work");
    stdout(repo.arc(&worktree).args(["snapshot", "rewritten"]));
    let recorded = repo.head(&worktree);

    // The operator rewrites history. arc's retention ref is what keeps a
    // recorded revision reachable, so a real rewrite takes it with everything
    // else: dropping it here is what a force-pushed, garbage-collected
    // repository looks like from the ledger's side.
    git(
        &worktree,
        &["commit", "--amend", "-m", "feat: work, rewritten"],
    );
    let rewritten = repo.head(&worktree);
    for ref_name in git_out(
        &worktree,
        &["for-each-ref", "--format=%(refname)", "refs/arc/"],
    )
    .lines()
    .map(str::to_string)
    .collect::<Vec<_>>()
    {
        git(&worktree, &["update-ref", "-d", &ref_name]);
    }
    git(&worktree, &["reflog", "expire", "--expire=now", "--all"]);
    git(&worktree, &["gc", "--prune=now", "--quiet"]);

    // Before the rewrite is recorded, the revision is simply gone.
    let before = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert!(before.contains("dangling-revision"), "{before}");

    let map = repo.root.join("commit-map");
    fs::write(&map, format!("{recorded} {rewritten}\n")).unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "signed an unsigned commit",
            "--tool",
            "git commit --amend",
        ])
        .assert()
        .success();

    // The event is untouched, and no migrated duplicate was appended beside
    // it: one patchset event, still naming what it named.
    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "rewritten",
        "--type",
        "patchset-added",
    ]));
    assert!(events.contains(&recorded), "{events}");
    assert_eq!(events.lines().count(), 1, "{events}");
    assert!(!events.contains(&rewritten), "{events}");

    // The rewrite itself is readable, which is what makes it a fact rather
    // than a private note.
    let repository_events = stdout(repo.arc(&repo.root).args([
        "events",
        "--repository",
        "--type",
        "history-rewritten",
    ]));
    assert!(repository_events.contains(&recorded), "{repository_events}");

    // What changed is the reading.
    let after = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert!(after.contains("revision-rewritten"), "{after}");
    assert!(!after.contains("dangling-revision"), "{after}");

    // `repository` is a perfectly good slug, and naming it must select that
    // change rather than the repository's own events.
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "repository", "--no-worktree"]),
    );
    let opened = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        "repository",
        "--type",
        "change-opened",
    ]));
    assert!(opened.contains("\"slug\":\"repository\""), "{opened}");

    let resolved = stdout(repo.arc(&repo.root).args(["history", "resolve", &recorded]));
    assert!(resolved.contains(&rewritten), "{resolved}");

    // A revision nothing rewrote is reported as unmoved, with exit 2 so a
    // script can tell the two apart.
    repo.arc(&repo.root)
        .args(["history", "resolve", &rewritten])
        .assert()
        .code(2);

    // An abbreviation resolves, because a map may abbreviate what the ledger
    // records in full and vice versa.
    repo.arc(&repo.root)
        .args(["history", "resolve", &recorded[..10]])
        .assert()
        .success();

    // A successor that exists but is not a commit is not a survivor either:
    // `diff` would follow a recorded revision into something Git refuses.
    let blob = {
        let out = std::process::Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(&repo.root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.as_mut().unwrap().write_all(b"not a commit\n")?;
                child.wait_with_output()
            })
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let blob_map = repo.root.join("blob-map");
    fs::write(&blob_map, format!("{recorded} {blob}\n")).unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            blob_map.to_str().unwrap(),
            "--reason",
            "a map naming a blob",
        ])
        .assert()
        .failure();

    // A map naming a successor this repository does not have describes some
    // other repository's history, and is refused rather than recorded.
    let bogus = repo.root.join("bogus-map");
    fs::write(
        &bogus,
        format!("{recorded} 0123456789012345678901234567890123456789\n"),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            bogus.to_str().unwrap(),
            "--reason",
            "a map from somewhere else",
        ])
        .assert()
        .failure();

    // The rewrite travels with a bundle: a receiver that has the rewritten
    // history can still follow the change's recorded revisions.
    let bundle = repo.home.join("bundle.json");
    repo.arc(&repo.root)
        .args(["export", "rewritten", "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let exported: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(
        exported["repository_events"].as_array().unwrap().len(),
        1,
        "{exported}"
    );
}
