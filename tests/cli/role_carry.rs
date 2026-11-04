//! Which recorded rewrites an approval follows: appending complete role
//! entries to a message's final trailer block keeps it, and every other change
//! to a commit resets it.

use crate::common::*;
use crate::rewrite::{approved_change, repo_with_gates, repository_events};
use std::io::Write as _;

const REVIEWER: &str = "Reviewed-by: Reviewer <reviewer@example.invalid>";

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", cwd.join(".git/fixture-global-config"))
        .env("XDG_CONFIG_HOME", cwd.join(".git/fixture-config-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

/// A commit object's bytes, exactly as Git stores them.
pub(crate) fn raw_commit(cwd: &Path, revision: &str) -> Vec<u8> {
    let out = git_command(cwd, &["cat-file", "commit", revision])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}

/// Write a commit object from its bytes and answer its id.
pub(crate) fn write_commit(cwd: &Path, raw: &[u8]) -> String {
    let mut child = git_command(cwd, &["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(raw).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A commit's headers and message, split where Git splits them.
fn split(raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let at = raw.windows(2).position(|pair| pair == b"\n\n").unwrap();
    (raw[..at + 1].to_vec(), raw[at + 2..].to_vec())
}

fn join(headers: &[u8], message: &[u8]) -> Vec<u8> {
    [headers, b"\n", message].concat()
}

/// `raw` with its message replaced.
fn with_message(raw: &[u8], message: &[u8]) -> Vec<u8> {
    join(&split(raw).0, message)
}

/// `raw` with `entries` appended to its message as `arc rewrite trailers`
/// appends them.
fn appending(raw: &[u8], entries: &[u8]) -> Vec<u8> {
    let (headers, message) = split(raw);
    join(&headers, &[&message, entries].concat())
}

fn approval_valid(repo: &Repo, slug: &str) -> bool {
    let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
    status["verdict"]["valid_for_current_head"] == true
}

/// An approved change whose only commit carries `headers` (prepended to its
/// own) and `message`, gates green.
fn approved_commit(repo: &Repo, slug: &str, extra_headers: &[u8], message: &[u8]) -> PathBuf {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "content\n",
        "placeholder",
    );
    let (headers, _) = split(&raw_commit(&worktree, "HEAD"));
    let crafted = write_commit(
        &worktree,
        &join(&[&headers, extra_headers].concat(), message),
    );
    git(&worktree, &["reset", "-q", "--hard", &crafted]);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["verify", slug, "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    assert!(approval_valid(repo, slug), "the fixture approval is valid");
    worktree
}

/// Move the change's branch from each commit to its successor, the way an
/// external tool does, record the map, and answer whether the approval
/// still covers the head.
fn rewritten(repo: &Repo, worktree: &Path, slug: &str, map: &[(String, String)]) -> bool {
    let head = &map.last().unwrap().1;
    git(worktree, &["reset", "-q", "--hard", head]);
    let text: String = map
        .iter()
        .map(|(old, new)| format!("{old} {new}\n"))
        .collect();
    repo.arc(&repo.root)
        .args(["history", "rewrite", "--map", "-", "--reason", "edited"])
        .write_stdin(text)
        .assert()
        .success();
    approval_valid(repo, slug)
}

/// Whether an approval of a one-commit change carrying `message` survives a
/// recorded rewrite of that commit to `edit(raw)`.
fn carried(message: &[u8], edit: impl Fn(&[u8]) -> Vec<u8>) -> bool {
    let repo = repo_with_gates();
    let worktree = approved_commit(&repo, "alpha", b"", message);
    let old = repo.head(&worktree);
    let new = write_commit(&worktree, &edit(&raw_commit(&worktree, &old)));
    assert_ne!(old, new, "the edit recreated the commit");
    rewritten(&repo, &worktree, "alpha", &[(old, new)])
}

const BLOCK: &[u8] = b"feat: alpha\n\nbody\n\nImplemented-by: a:b#c\n";

/// The acceptance claim: recording who reviewed with `arc rewrite trailers`
/// keeps the approval and with it the authority to integrate. Rejects the
/// reset every message edit used to cost, which made the record of a review
/// unrecord it.
#[test]
fn appending_a_reviewer_keeps_the_approval_and_integration_authority() {
    let repo = repo_with_gates();
    let (worktree, approved) = approved_change(&repo, "alpha");

    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            REVIEWER,
            "--append",
            "Orchestrated-by: lead:model#high",
            "--from",
            &approved,
        ])
        .assert()
        .success();

    assert_ne!(repo.head(&worktree), approved, "the message was edited");
    assert!(
        approval_valid(&repo, "alpha"),
        "appended role entries must keep the approval"
    );
    repo.arc(&worktree)
        .args(["check", "alpha"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["snapshot", "alpha"])
        .assert()
        .success()
        .stdout(predicates::str::contains("patchset: ps-01 (unchanged)"));
    repo.arc(&repo.root)
        .args(["integrate", "alpha"])
        .assert()
        .success();
    let state = json_stdout(repo.arc(&repo.root).args(["show", "alpha", "--json"]));
    assert_eq!(state["verdicts"].as_array().unwrap().len(), 1, "{state}");
    assert!(
        git_out(&repo.root, &["log", "--format=%B", "master"]).contains(REVIEWER),
        "the integrated head is the rewritten one"
    );
}

/// `Assisted-by` is no role entry, so the same edit with it resets. Rejects
/// carrying through any trailer `arc rewrite trailers` appends.
#[test]
fn appending_assisted_by_leaves_the_approval_stale() {
    let repo = repo_with_gates();
    let (worktree, approved) = approved_change(&repo, "alpha");
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            "Assisted-by: a:b#c (review)",
            "--from",
            &approved,
        ])
        .assert()
        .success();
    assert!(!approval_valid(&repo, "alpha"));
    repo.arc(&worktree)
        .args(["check", "alpha"])
        .assert()
        .code(3);
}

/// A message with no trailer block gains one after a blank line, and the
/// approval follows. Rejects counting that separator as a body edit.
#[test]
fn a_message_without_a_block_that_gains_one_keeps_the_approval() {
    assert!(carried(b"feat: alpha\n\nbody\n", |raw| appending(
        raw,
        format!("\n{REVIEWER}\n").as_bytes()
    )));
}

/// An appended entry continuing on a second line is one complete entry.
/// Rejects reading the block line by line.
#[test]
fn a_role_entry_with_a_continuation_keeps_the_approval() {
    assert!(carried(BLOCK, |raw| appending(
        raw,
        b"Reviewed-by: Reviewer\n <reviewer@example.invalid>\n"
    )));
}

/// A message in another encoding is compared byte for byte, under the
/// `encoding` header that names it. Rejects a comparison that needs UTF-8.
#[test]
fn a_non_utf8_message_under_an_encoding_header_keeps_the_approval() {
    let repo = repo_with_gates();
    let worktree = approved_commit(
        &repo,
        "alpha",
        b"encoding ISO-8859-1\n",
        b"feat: caf\xe9\n\nImplemented-by: Jos\xe9 <jose@example.invalid>\n",
    );
    let old = repo.head(&worktree);
    let raw = raw_commit(&worktree, &old);
    assert!(raw.windows(9).any(|window| window == b"encoding "));
    let new = write_commit(
        &worktree,
        &appending(&raw, format!("{REVIEWER}\n").as_bytes()),
    );
    assert!(rewritten(&repo, &worktree, "alpha", &[(old, new)]));
}

/// A changed committer is a different commit whatever the message says.
/// Rejects comparing only author and dates.
#[test]
fn a_changed_committer_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| {
        let text = String::from_utf8(appending(raw, format!("{REVIEWER}\n").as_bytes())).unwrap();
        let committer = text
            .lines()
            .find(|line| line.starts_with("committer "))
            .unwrap()
            .to_string();
        let timestamp = committer.rsplit(' ').nth(1).unwrap();
        let moved = committer.replace(
            timestamp,
            &(timestamp.parse::<i64>().unwrap() + 1).to_string(),
        );
        text.replace(&committer, &moved).into_bytes()
    }));
}

/// A carried header's bytes are part of the approved commit, a carriage
/// return ending one included. Rejects a header comparison that drops it and
/// so judges a successor without it signature-only, or a role addition when
/// role entries are appended too.
#[test]
fn a_dropped_header_carriage_return_resets_the_approval() {
    let without_return = |raw: &[u8]| -> Vec<u8> {
        let at = raw
            .windows(15)
            .position(|window| window == b"x-extra value\r\n")
            .unwrap();
        [&raw[..at + 13], &raw[at + 14..]].concat()
    };
    for entries in [&b""[..], format!("{REVIEWER}\n").as_bytes()] {
        let repo = repo_with_gates();
        let worktree = approved_commit(&repo, "alpha", b"x-extra value\r\nx-tail end\n", BLOCK);
        let old = repo.head(&worktree);
        let new = write_commit(
            &worktree,
            &appending(&without_return(&raw_commit(&worktree, &old)), entries),
        );
        assert!(
            !rewritten(&repo, &worktree, "alpha", &[(old, new)]),
            "the approval followed a successor whose header lost its carriage return, \
             appending {:?}",
            String::from_utf8_lossy(entries)
        );
    }
}

/// A header the old commit did not carry resets. Rejects comparing only the
/// headers arc interprets.
#[test]
fn an_extra_header_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| {
        let (headers, message) = split(raw);
        join(
            &[&headers, &b"x-extra value\n"[..]].concat(),
            &[&message, format!("{REVIEWER}\n").as_bytes()].concat(),
        )
    }));
}

/// A body line edited beside an appended entry resets. Rejects comparing
/// only the trailer block.
#[test]
fn a_changed_body_line_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| with_message(
        raw,
        format!("feat: alpha\n\nbody!\n\nImplemented-by: a:b#c\n{REVIEWER}\n").as_bytes()
    )));
}

/// A non-role trailer resets. Rejects carrying any appended trailer.
#[test]
fn an_added_non_role_trailer_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| appending(
        raw,
        b"Signed-off-by: Reviewer <reviewer@example.invalid>\n"
    )));
}

/// `Assisted-by` resets through the recorded-map path too.
#[test]
fn an_added_assisted_by_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| appending(
        raw,
        b"Assisted-by: a:b#c (review)\n"
    )));
}

/// Replacing a role entry changes what the message declares. Rejects a
/// comparison that strips role entries from both sides.
#[test]
fn a_replaced_role_entry_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| with_message(
        raw,
        format!("feat: alpha\n\nbody\n\nImplemented-by: g:h#i\n{REVIEWER}\n").as_bytes()
    )));
}

/// Removing a role entry resets. Rejects treating any role-only difference
/// as an addition.
#[test]
fn a_removed_role_entry_resets_the_approval() {
    assert!(!carried(
        b"feat: alpha\n\nbody\n\nImplemented-by: a:b#c\nPlanned-by: d:e#f\n",
        |raw| with_message(
            raw,
            format!("feat: alpha\n\nbody\n\nImplemented-by: a:b#c\n{REVIEWER}\n").as_bytes()
        )
    ));
}

/// An appended line that turns the final paragraph into prose leaves no
/// trailer block, and resets. Rejects matching role lines anywhere.
#[test]
fn a_malformed_block_resets_the_approval() {
    assert!(!carried(BLOCK, |raw| appending(
        raw,
        format!("{REVIEWER}\nnot a trailer\n").as_bytes()
    )));
}

/// Every rewritten commit of the range is judged, not only the head: a role
/// entry on the head does not carry an approval over a body edit beneath
/// it. Rejects judging a parent only by whether the map translates it.
#[test]
fn an_edit_beneath_the_head_resets_the_approval() {
    let repo = repo_with_gates();
    stdout(repo.arc(&repo.root).args(["begin", "alpha"]));
    let worktree = repo.home.join(".worktrees/repo-alpha");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one\n\nbody");
    let first = repo.head(&worktree);
    repo.commit(&worktree, "two.txt", "two\n", "feat: two");
    let second = repo.head(&worktree);
    stdout(repo.arc(&worktree).args(["snapshot", "alpha"]));
    repo.arc(&worktree)
        .args(["verify", "alpha", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "alpha", "--verdict", "approved"])
        .assert()
        .success();
    assert!(approval_valid(&repo, "alpha"));

    let first_raw = raw_commit(&worktree, &first);
    let edited = write_commit(
        &worktree,
        &with_message(&first_raw, b"feat: one\n\nanother body\n"),
    );
    let second_raw = String::from_utf8(raw_commit(&worktree, &second))
        .unwrap()
        .replace(&format!("parent {first}"), &format!("parent {edited}"));
    let reviewed = write_commit(
        &worktree,
        &appending(second_raw.as_bytes(), format!("\n{REVIEWER}\n").as_bytes()),
    );
    assert!(!rewritten(
        &repo,
        &worktree,
        "alpha",
        &[(first, edited), (second, reviewed)]
    ));
}

/// The same range with role entries appended to both commits keeps the
/// approval: the rule judges each commit, and each only added entries.
#[test]
fn role_entries_appended_across_a_range_keep_the_approval() {
    let repo = repo_with_gates();
    stdout(repo.arc(&repo.root).args(["begin", "alpha"]));
    let worktree = repo.home.join(".worktrees/repo-alpha");
    repo.commit(&worktree, "one.txt", "one\n", "feat: one");
    let first = repo.head(&worktree);
    repo.commit(&worktree, "two.txt", "two\n", "feat: two");
    stdout(repo.arc(&worktree).args(["snapshot", "alpha"]));
    repo.arc(&worktree)
        .args(["verify", "alpha", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "alpha", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--no-sign",
            "--append",
            REVIEWER,
            "--from",
            &first,
        ])
        .assert()
        .success();
    assert!(approval_valid(&repo, "alpha"));
}

/// A declared waiver follows a role addition as an approval does, and the
/// debt keeps the production record and comparison text it recorded: the
/// appended `Implemented-by` is declared, not adopted. Rejects a waiver that
/// a recorded reviewer line would unrecord, and a debt rewritten by it.
#[test]
fn a_debt_waiver_follows_a_role_addition_and_keeps_its_record() {
    let repo = repo_with_gates();
    let (_, worktree, _) = change_with_patchset(&repo, "alpha");
    repo.declare_gates_locally(
        "[policy]\nforbid_self_approval = true\n[danger]\npaths = [\"alpha.txt\"]\n",
    );
    repo.arc(&worktree)
        .args(["verify", "alpha", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "debt",
            "alpha",
            "--reason",
            "no independent reviewer available",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["check", "alpha"])
        .assert()
        .success();
    let debt = |repo: &Repo| {
        json_stdout(repo.arc(&repo.root).args(["status", "alpha", "--json"]))["debt"].clone()
    };
    let before = debt(&repo);
    assert!(before.is_object(), "the debt is in force: {before}");
    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--from",
            "HEAD",
            "--no-sign",
            "--append",
            "Implemented-by: Someone Else <else@example.invalid>",
        ])
        .assert()
        .success();
    let status = json_stdout(repo.arc(&worktree).args(["status", "alpha", "--json"]));
    assert_eq!(status["head_matches_latest_patchset"], true, "{status}");
    assert_eq!(status["approval_waived_by_debt"], true, "{status}");
    repo.arc(&worktree)
        .args(["check", "alpha"])
        .assert()
        .success();
    assert_eq!(debt(&repo), before, "the debt reads as it was recorded");
}

/// A bundle reaches the same approval carry as dependency-ordered imports,
/// even when event IDs and transport order put children before their parents.
/// Rejects judging each new event against a map missing the other new judgments.
#[test]
fn importing_role_addition_ancestry_together_matches_sequential_imports() {
    let repo = repo_with_gates();
    stdout(repo.arc(&repo.root).args(["begin", "alpha"]));
    let worktree = repo.home.join(".worktrees/repo-alpha");
    let mut originals = Vec::new();
    for name in ["one", "two", "three"] {
        repo.commit(
            &worktree,
            &format!("{name}.txt"),
            name,
            &format!("feat: {name}"),
        );
        originals.push(repo.head(&worktree));
    }
    stdout(repo.arc(&worktree).args(["snapshot", "alpha"]));
    repo.arc(&worktree)
        .args(["verify", "alpha", "--all"])
        .assert()
        .success();
    repo.arc(&worktree)
        .args(["review", "alpha", "--verdict", "approved"])
        .assert()
        .success();
    assert!(approval_valid(&repo, "alpha"));

    let mut mapping: Vec<(String, String)> = Vec::new();
    for old in &originals {
        let mut raw = String::from_utf8(raw_commit(&worktree, old)).unwrap();
        if let Some((parent, successor)) = mapping.last() {
            raw = raw.replace(
                &format!("parent {parent}\n"),
                &format!("parent {successor}\n"),
            );
        }
        let new = write_commit(
            &worktree,
            &appending(raw.as_bytes(), format!("\n{REVIEWER}\n").as_bytes()),
        );
        mapping.push((old.clone(), new));
    }
    git(&worktree, &["reset", "--hard", &mapping.last().unwrap().1]);
    // Recording children first makes event-ID order oppose the dependencies.
    for (old, new) in mapping.iter().rev() {
        repo.arc(&repo.root)
            .args([
                "history",
                "rewrite",
                "--map",
                "-",
                "--reason",
                "role entries appended",
            ])
            .write_stdin(format!("{old} {new}\n"))
            .assert()
            .success();
    }
    let bundle_path = repo.home.join("alpha.json");
    repo.arc(&repo.root)
        .args(["export", "alpha", "--output", bundle_path.to_str().unwrap()])
        .assert()
        .success();
    let bundle: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
    let events = bundle["repository_events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    let clear = || {
        for path in repository_events(&repo) {
            fs::remove_file(path).unwrap();
        }
        let imports = repo.root.join(".git/arc/imports");
        if imports.exists() {
            fs::remove_dir_all(imports).unwrap();
        }
        assert!(!approval_valid(&repo, "alpha"));
    };
    let judgments = || -> Vec<serde_json::Value> {
        let events = stdout(
            repo.arc(&repo.root)
                .args(["events", "--type", "history-rewritten"]),
        );
        events
            .lines()
            .map(|line| {
                let event: serde_json::Value = serde_json::from_str(line).unwrap();
                event["role_additions"].clone()
            })
            .collect()
    };

    clear();
    let mut single = bundle.clone();
    for event in events.iter().rev() {
        single["repository_events"] = serde_json::json!([event]);
        fs::write(&bundle_path, serde_json::to_vec(&single).unwrap()).unwrap();
        repo.arc(&repo.root)
            .args(["import", bundle_path.to_str().unwrap()])
            .assert()
            .success();
    }
    assert!(
        approval_valid(&repo, "alpha"),
        "sequential imports carry the approval"
    );
    let sequential = judgments();
    assert_eq!(
        sequential,
        originals
            .iter()
            .rev()
            .map(|old| serde_json::json!([old]))
            .collect::<Vec<_>>()
    );
    repo.arc(&worktree)
        .args(["check", "alpha"])
        .assert()
        .success();

    for reverse in [false, true] {
        clear();
        let mut together = bundle.clone();
        if reverse {
            together["repository_events"]
                .as_array_mut()
                .unwrap()
                .reverse();
        }
        fs::write(&bundle_path, serde_json::to_vec(&together).unwrap()).unwrap();
        repo.arc(&repo.root)
            .args(["import", bundle_path.to_str().unwrap()])
            .assert()
            .success();
        assert_eq!(
            judgments(),
            sequential,
            "bundle import must retain every receiver-derived parent judgment; reversed={reverse}"
        );
        assert!(
            approval_valid(&repo, "alpha"),
            "bundle import must carry the same approval as sequential imports; reversed={reverse}"
        );
        repo.arc(&worktree)
            .args(["check", "alpha"])
            .assert()
            .success();
        repo.arc(&repo.root)
            .args(["import", bundle_path.to_str().unwrap()])
            .assert()
            .success();
        assert_eq!(judgments(), sequential, "reimport preserves the judgments");
    }
}
