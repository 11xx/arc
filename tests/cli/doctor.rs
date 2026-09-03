use super::common::*;
use std::os::unix::fs::PermissionsExt;

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
        .stdout(predicates::str::contains("\"schema\":\"arc-doctor/3\""));
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

    let catchup_json = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    assert_eq!(catchup_json["schema"], "arc-catchup/4", "{catchup_json}");
    assert_eq!(
        catchup_json["worktrees"]["changes"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{catchup_json}"
    );
    assert!(
        catchup_json["worktrees"]["total_bytes"].as_u64().is_some(),
        "{catchup_json}"
    );

    let doctor = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    assert!(doctor.contains("open-worktree-usage: "), "{doctor}");
    assert!(doctor.contains("open-worktree-usage-total"), "{doctor}");

    // A ledger with no open worktrees stays silent on the subject.
    let cleaned = Repo::new();
    let quiet = stdout(cleaned.arc(&cleaned.root).args(["catchup"]));
    assert!(!quiet.contains("worktrees: "), "{quiet}");
}

/// Recorded worktree paths may be relative when a caller supplied
/// --worktree. Accounting resolves them from the command cwd before matching
/// Git's inventory and before invoking du; paths that do not match remain
/// visible as unknown instead of disappearing.
#[test]
fn worktree_accounting_resolves_relative_paths_and_reports_mismatches() {
    let repo = Repo::new();
    let relative_output = stdout(repo.arc(&repo.root).args(["begin", "relative-accounted"]));
    let relative_id = opened_change_id(&relative_output);
    let relative_path = repo.home.join(".worktrees/repo-relative-accounted");
    let relative_recorded = PathBuf::from("..")
        .join(repo.home.file_name().unwrap())
        .join(".worktrees/repo-relative-accounted");
    rewrite_event(&repo, &relative_id, "change-opened", |event| {
        event["worktree"] =
            serde_json::Value::String(relative_recorded.to_string_lossy().into_owned());
    });

    let missing_output = stdout(repo.arc(&repo.root).args(["begin", "missing-accounted"]));
    let missing_id = opened_change_id(&missing_output);
    rewrite_event(&repo, &missing_id, "change-opened", |event| {
        event["worktree"] =
            serde_json::Value::String("../home/.worktrees/no-such-worktree".to_string());
    });

    let report = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    let changes = report["worktrees"]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{report}");
    assert_eq!(changes[0]["change_id"], relative_id);
    assert_eq!(
        changes[0]["path"],
        relative_path.to_string_lossy().as_ref(),
        "{report}"
    );
    assert!(changes[0]["bytes"].as_u64().is_some(), "{report}");

    let unknown = report["worktrees"]["unknown"].as_array().unwrap();
    assert_eq!(unknown.len(), 1, "{report}");
    assert_eq!(unknown[0]["change_id"], missing_id);
    assert!(
        unknown[0]["reason"]
            .as_str()
            .unwrap()
            .contains("does not match Git"),
        "{report}"
    );
    assert!(report["worktrees"]["total_bytes"].is_null(), "{report}");
}

/// A failed Git inventory is not an empty inventory. Catchup keeps the open
/// worktree visible and says that its accounting is unknown, in both views.
#[test]
fn catchup_reports_unknown_worktree_accounting_when_git_inventory_fails() {
    let repo = Repo::new();
    let output = stdout(repo.arc(&repo.root).args(["begin", "inventory-failure"]));
    let change_id = opened_change_id(&output);

    let bin = repo.home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .expect("git must be available on PATH");
    let fake_git = bin.join("git");
    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = list ] && [ \"$3\" = --porcelain ]; then\n  echo inventory unavailable >&2\n  exit 42\nfi\nexec \"{}\" \"$@\"\n",
            real_git.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();

    let text = stdout(repo.arc(&repo.root).env("PATH", &path).args(["catchup"]));
    assert!(text.contains("accounting unavailable"), "{text}");
    assert!(text.contains(&change_id), "{text}");

    let json = json_stdout(
        repo.arc(&repo.root)
            .env("PATH", &path)
            .args(["catchup", "--json"]),
    );
    assert!(json["worktrees"]["total_bytes"].is_null(), "{json}");
    assert_eq!(json["worktrees"]["unknown"][0]["change_id"], change_id);
    assert!(
        json["worktrees"]["unknown"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("inventory unavailable"),
        "{json}"
    );
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

/// Fork checkouts are measured beside the changes and totalled apart from
/// them. A fork sits outside the change lifecycle, so no integration ever
/// retires its worktree; an accounting that omitted it understated exactly
/// the checkouts that persist, and one that summed it in would answer
/// neither question.
#[test]
fn worktree_accounting_counts_forks_apart_from_changes() {
    let repo = Repo::new();
    begin_change_no_helper(&repo, "usage-change");
    repo.arc(&repo.root)
        .args(["fork", "begin", "usage-fork"])
        .assert()
        .success();

    let value = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    let accounting = &value["worktrees"];
    let changes = accounting["changes"].as_array().unwrap();
    let forks = accounting["forks"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{accounting}");
    assert_eq!(forks.len(), 1, "{accounting}");
    assert_eq!(forks[0]["slug"], "usage-fork", "{accounting}");
    assert!(
        forks[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("repo-fork-usage-fork"),
        "{accounting}"
    );

    // The change total is the changes alone: the fork's bytes are reported,
    // never folded in.
    assert_eq!(
        accounting["total_bytes"].as_u64().unwrap(),
        changes[0]["bytes"].as_u64().unwrap(),
        "{accounting}"
    );
    assert_eq!(
        accounting["fork_total_bytes"].as_u64().unwrap(),
        forks[0]["bytes"].as_u64().unwrap(),
        "{accounting}"
    );

    let text = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(text.contains("fork worktrees: "), "{text}");
    assert!(text.contains("usage-fork"), "{text}");

    let doctor = stdout(repo.arc(&repo.root).args(["doctor", "--verbose"]));
    assert!(
        doctor.contains("fork-worktree-usage: usage-fork"),
        "{doctor}"
    );
    assert!(doctor.contains("fork-worktree-usage-total"), "{doctor}");
}

/// A size is reported with the method that produced it and the filesystem it
/// was taken on. `du` sums apparent size, which is physical cost only where
/// bytes are stored one for one, so the number travels with what it means.
#[test]
fn worktree_accounting_names_its_method_and_the_mount_it_measured() {
    let repo = Repo::new();
    begin_change_no_helper(&repo, "measured");

    let value = json_stdout(repo.arc(&repo.root).args(["catchup", "--json"]));
    let measurement = &value["worktrees"]["measurement"];
    assert_eq!(measurement["method"], "du-apparent", "{measurement}");
    assert_eq!(measurement["physical"], "unknown", "{measurement}");
    assert!(
        measurement["filesystem"]["free_bytes"].as_u64().is_some(),
        "{measurement}"
    );
    assert!(
        measurement["filesystem"]["mount"].as_str().is_some(),
        "{measurement}"
    );

    // Every total a reader could spend carries the method in text too.
    let text = stdout(repo.arc(&repo.root).args(["catchup"]));
    assert!(text.contains("[du-apparent; physical: unknown"), "{text}");
    assert!(text.contains("worktree root: "), "{text}");
}

/// Every binary the accounting consults is optional. Without `findmnt` the
/// filesystem type is unknown and the sizes are still reported: a missing
/// tool degrades one field rather than failing a command that only advises.
#[test]
fn worktree_accounting_without_findmnt_reports_an_unknown_filesystem() {
    let repo = Repo::new();
    begin_change_no_helper(&repo, "no-findmnt");

    // A PATH holding only the tools the accounting may still use. Git
    // resolves its own helpers from the real binary behind the symlink, so
    // the repository keeps working while `findmnt` is genuinely absent.
    let shim = repo.home.join("shim");
    fs::create_dir_all(&shim).unwrap();
    for tool in ["git", "du", "df"] {
        std::os::unix::fs::symlink(which(tool), shim.join(tool)).unwrap();
    }

    let value = json_stdout(
        repo.arc(&repo.root)
            .env("PATH", &shim)
            .args(["catchup", "--json"]),
    );
    let measurement = &value["worktrees"]["measurement"];
    assert_eq!(
        measurement["filesystem"]["fstype"], "unknown",
        "{measurement}"
    );
    // Whether the mount compresses is unread, which is not the claim that it
    // does not, so the field is absent rather than false.
    assert!(
        measurement["filesystem"]["compressed"].is_null(),
        "{measurement}"
    );
    assert_eq!(measurement["method"], "du-apparent", "{measurement}");
    assert!(
        value["worktrees"]["changes"][0]["bytes"].as_u64().is_some(),
        "sizes must survive a missing findmnt: {value}"
    );
    // `df` still answers, so the mount and its free space remain.
    assert!(
        measurement["filesystem"]["free_bytes"].as_u64().is_some(),
        "{measurement}"
    );
}

/// The first binary of `tool` on the invoking PATH.
fn which(tool: &str) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{tool} must be on PATH for this test"))
}

/// Creating a worktree spends disk, and the space it will land in is
/// reported before it is spent. A filesystem that fills reports as success
/// everywhere else, which is why the number is printed rather than assumed.
#[test]
fn begin_and_fork_begin_report_what_the_worktree_root_has_left() {
    let repo = Repo::new();
    let begun = stdout(repo.arc(&repo.root).args(["begin", "preflighted"]));
    assert!(begun.contains("worktree root free: "), "{begun}");
    assert!(begun.contains(" on "), "{begun}");

    let forked = stdout(repo.arc(&repo.root).args(["fork", "begin", "preflighted"]));
    assert!(forked.contains("worktree root free: "), "{forked}");
}
