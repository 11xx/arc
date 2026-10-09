//! `arc rewrite trailers --edits`: replacing or dropping one exact trailer
//! line of one named commit, with every other commit and every other line left
//! as it was.

use crate::common::*;
use crate::rewrite::{
    arc_signing, git_signing, git_signing_write, intent_path, ref_snapshot, repository_events,
    signing_key,
};

const UNKNOWN: &str = "Planned-by: codex:unknown";
const REAL: &str = "Planned-by: codex:gpt-6.1-sol#xhigh";
const ORCHESTRATOR_UNKNOWN: &str = "Orchestrated-by: codex:unknown";
const ASSISTED: &str = "Assisted-by: codex:gpt-5#high (review)";
const REVIEWER: &str = "Reviewed-by: Rev <rev@example.invalid>";

/// One commit of the fixture history, with its own author, committer and
/// dates so that a rewrite which normalised any of them would show.
fn commit(cwd: &Path, file: &str, message: &str, author: &str, authored: &str, committed: &str) {
    fs::write(cwd.join(file), format!("{file}\n")).unwrap();
    git(cwd, &["add", "--", file]);
    let out = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", cwd.join(".git/fixture-global-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", author)
        .env("GIT_AUTHOR_EMAIL", format!("{author}@example.invalid"))
        .env("GIT_AUTHOR_DATE", authored)
        .env("GIT_COMMITTER_NAME", "Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
        .env("GIT_COMMITTER_DATE", committed)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A branch of four commits on top of the fixture's first, each with its own
/// message and identities.
struct History {
    repo: Repo,
    /// The commit below the range, which no edit may name.
    base: String,
    /// The four commits in the range, oldest first.
    commits: Vec<String>,
}

impl History {
    fn new() -> History {
        let repo = Repo::new();
        let base = repo.head(&repo.root);
        for (file, message, author, authored, committed) in [
            (
                "one.txt",
                format!("feat: one\n\n{UNKNOWN}\n{REAL}\n"),
                "Alice",
                "2024-01-01T10:00:00+0100",
                "2024-01-02T11:00:00-0300",
            ),
            (
                "two.txt",
                format!("feat: two\n\n{ORCHESTRATOR_UNKNOWN}\n{ASSISTED}\nSigned-off-by: A <a@example.invalid>\n"),
                "Bea",
                "2024-02-01T10:00:00+0000",
                "2024-02-02T11:30:00+0530",
            ),
            (
                "three.txt",
                format!("feat: three\n\n{REVIEWER}\n{REVIEWER}\n"),
                "Cy",
                "2024-03-01T10:00:00-0700",
                "2024-03-02T11:00:00+0000",
            ),
            (
                "four.txt",
                "feat: four\n\nA body with no trailers.\n".to_string(),
                "Di",
                "2024-04-01T10:00:00+0200",
                "2024-04-02T11:00:00+0200",
            ),
        ] {
            commit(&repo.root, file, &message, author, authored, committed);
        }
        let commits = git_out(
            &repo.root,
            &["rev-list", "--reverse", &format!("{base}..HEAD")],
        )
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
        assert_eq!(commits.len(), 4);
        History {
            repo,
            base,
            commits,
        }
    }

    fn message(&self, commit: &str) -> String {
        git_out(&self.repo.root, &["show", "-s", "--format=%B", commit])
    }

    /// A file of edits, one entry per `(commit, action, old, new)`.
    fn edits(&self, name: &str, entries: &[(&str, &str, &str, &str)]) -> PathBuf {
        let mut text = String::from("# arc-trailer-edits/1\n");
        for (commit, action, old, new) in entries {
            text.push_str(&format!("{commit}\t{action}\t{old}\t{new}\n"));
        }
        let path = self.repo.home.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    fn rewrite(&self, file: &Path) -> AssertCommand {
        let mut command = self.repo.arc(&self.repo.root);
        command.args([
            "rewrite",
            "trailers",
            "--from",
            &self.commits[0],
            "--no-sign",
            "--edits",
            file.to_str().unwrap(),
        ]);
        command
    }

    /// The commits now in the range, oldest first.
    fn rewritten(&self) -> Vec<String> {
        git_out(
            &self.repo.root,
            &["rev-list", "--reverse", &format!("{}..HEAD", self.base)],
        )
        .lines()
        .map(str::to_string)
        .collect()
    }

    /// Nothing moved, recorded or left half-done.
    fn assert_untouched(&self, before: &str) {
        assert_eq!(ref_snapshot(&self.repo.root), before);
        assert!(!intent_path(&self.repo).exists());
        assert!(repository_events(&self.repo).is_empty());
    }
}

/// The header lines a rewrite is never to change: the tree, the author and
/// the committer, with their dates and zones.
fn identity(cwd: &Path, commit: &str) -> Vec<String> {
    git_out(cwd, &["cat-file", "commit", commit])
        .lines()
        .take_while(|line| !line.is_empty())
        .filter(|line| {
            line.starts_with("tree ")
                || line.starts_with("author ")
                || line.starts_with("committer ")
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn replace_and_drop_apply_to_different_commits_in_one_run() {
    let history = History::new();
    let [one, two, three, four] = [0, 1, 2, 3].map(|n| history.commits[n].clone());
    let file = history.edits(
        "edits",
        &[
            (&one, "replace", UNKNOWN, "Planned-by: codex:gpt-6.1#high"),
            (&two, "drop", ORCHESTRATOR_UNKNOWN, ""),
        ],
    );

    history.rewrite(&file).assert().success();

    let now = history.rewritten();
    assert_eq!(now.len(), 4);
    assert_eq!(
        history.message(&now[0]),
        format!("feat: one\n\nPlanned-by: codex:gpt-6.1#high\n{REAL}"),
        "the replacement keeps its position"
    );
    assert_eq!(
        history.message(&now[1]),
        format!("feat: two\n\n{ASSISTED}\nSigned-off-by: A <a@example.invalid>"),
    );
    assert_eq!(
        history.message(&now[2]),
        history.message(&three),
        "a commit no edit names keeps its message though it is recreated"
    );
    assert_eq!(history.message(&now[3]), history.message(&four));
    assert_ne!(now[2], three, "its ancestry moved, so it is recreated");
    assert_eq!(history.repo.head(&history.repo.root), now[3]);
    assert!(
        git_out(&history.repo.root, &["cat-file", "-t", &history.base]) == "commit"
            && history.rewritten().len() == 4,
        "the commit below the range is untouched"
    );
}

#[test]
fn only_the_named_commit_is_edited_when_another_carries_the_same_line() {
    let history = History::new();
    commit(
        &history.repo.root,
        "extra.txt",
        &format!("feat: extra\n\n{UNKNOWN}\n"),
        "Ed",
        "2024-05-01T10:00:00+0000",
        "2024-05-02T11:00:00+0000",
    );
    let extra = history.repo.head(&history.repo.root);
    let file = history.edits("edits", &[(&extra, "drop", UNKNOWN, "")]);

    history.rewrite(&file).assert().success();

    let now = history.rewritten();
    assert_eq!(now.len(), 5);
    assert!(
        history.message(&now[0]).contains(UNKNOWN),
        "the first commit carries the same line and no edit names it"
    );
    assert_eq!(now[0], history.commits[0], "and it is not recreated");
    assert!(!history.message(&now[4]).contains(UNKNOWN));
}

#[test]
fn an_old_line_missing_or_occurring_twice_refuses() {
    let history = History::new();
    let [one, _, three, _] = [0, 1, 2, 3].map(|n| history.commits[n].clone());
    let before = ref_snapshot(&history.repo.root);

    // Not in the block, and in no other commit's block under this name.
    let missing = history.edits(
        "missing",
        &[
            (&one, "drop", UNKNOWN, ""),
            (&one, "drop", "Planned-by: codex:absent", ""),
        ],
    );
    history
        .rewrite(&missing)
        .assert()
        .failure()
        .stderr(predicates::str::contains("missing:3"))
        .stderr(predicates::str::contains("Planned-by: codex:absent"));
    history.assert_untouched(&before);

    // A line the commit names for another key is not the line.
    let key_only = history.edits("key-only", &[(&one, "drop", "Planned-by", "")]);
    history
        .rewrite(&key_only)
        .assert()
        .failure()
        .stderr(predicates::str::contains("key-only:2"));
    history.assert_untouched(&before);

    // Twice in one block: the edit does not say which.
    let twice = history.edits("twice", &[(&three, "drop", REVIEWER, "")]);
    history
        .rewrite(&twice)
        .assert()
        .failure()
        .stderr(predicates::str::contains("twice:2"))
        .stderr(predicates::str::contains("2 times"));
    history.assert_untouched(&before);

    // Two edits of one line say two things about it.
    let same = history.edits(
        "same",
        &[
            (&one, "drop", UNKNOWN, ""),
            (&one, "replace", UNKNOWN, "Planned-by: codex:gpt-6.1#high"),
        ],
    );
    history
        .rewrite(&same)
        .assert()
        .failure()
        .stderr(predicates::str::contains("same:3"))
        .stderr(predicates::str::contains("same:2"));
    history.assert_untouched(&before);
}

#[test]
fn a_sha_outside_the_range_refuses() {
    let history = History::new();
    let before = ref_snapshot(&history.repo.root);
    let below = history.edits(
        "below",
        &[(&history.base, "drop", "Planned-by: codex:x", "")],
    );
    history
        .rewrite(&below)
        .assert()
        .failure()
        .stderr(predicates::str::contains("below:2"))
        .stderr(predicates::str::contains("not in the range"));
    history.assert_untouched(&before);

    let unknown = history.edits("unknown", &[(&"a".repeat(40), "drop", UNKNOWN, "")]);
    history
        .rewrite(&unknown)
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown:2"));
    history.assert_untouched(&before);

    let abbreviated = history.edits(
        "abbreviated",
        &[(&history.commits[0][..12], "drop", UNKNOWN, "")],
    );
    history
        .rewrite(&abbreviated)
        .assert()
        .failure()
        .stderr(predicates::str::contains("abbreviated:2"))
        .stderr(predicates::str::contains("full commit sha"));
    history.assert_untouched(&before);
}

#[test]
fn a_new_line_that_is_no_coordinate_refuses_even_in_a_dry_run() {
    let history = History::new();
    let one = history.commits[0].clone();
    let two = history.commits[1].clone();
    let before = ref_snapshot(&history.repo.root);
    for (new, why) in [
        ("Planned-by: codex:unknown", "placeholder"),
        (
            "Planned-by: codex:019f0000-0000-7000-8000-00000000000a",
            "session id",
        ),
        ("Planned-by: Somebody <somebody>", "address"),
        ("Planned-by: not a coordinate", "expected"),
        ("Reviewed-by: n/a:model", "placeholder"),
    ] {
        let file = history.edits(
            "bad",
            &[
                (&two, "drop", ORCHESTRATOR_UNKNOWN, ""),
                (&one, "replace", UNKNOWN, new),
            ],
        );
        for dry in [false, true] {
            let mut command = history.rewrite(&file);
            if dry {
                command.arg("--dry-run");
            }
            command
                .assert()
                .failure()
                .stderr(predicates::str::contains("bad:3"))
                .stderr(predicates::str::contains(new))
                .stderr(predicates::str::contains(why));
            history.assert_untouched(&before);
        }
    }
}

#[test]
fn an_assisted_by_replacement_is_judged_without_its_role_suffix() {
    let history = History::new();
    let two = history.commits[1].clone();
    let before = ref_snapshot(&history.repo.root);

    let bad = history.edits(
        "bad",
        &[(
            &two,
            "replace",
            ASSISTED,
            "Assisted-by: codex:unknown (review)",
        )],
    );
    history
        .rewrite(&bad)
        .assert()
        .failure()
        .stderr(predicates::str::contains("bad:2"))
        .stderr(predicates::str::contains("placeholder"));
    history.assert_untouched(&before);

    let good = history.edits(
        "good",
        &[(
            &two,
            "replace",
            ASSISTED,
            "Assisted-by: codex:gpt-6.1-sol#xhigh (review)",
        )],
    );
    history.rewrite(&good).assert().success();
    let now = history.rewritten();
    assert_eq!(
        history.message(&now[1]),
        format!(
            "feat: two\n\n{ORCHESTRATOR_UNKNOWN}\nAssisted-by: codex:gpt-6.1-sol#xhigh (review)\nSigned-off-by: A <a@example.invalid>"
        )
    );
}

#[test]
fn the_edit_file_format_is_read_strictly() {
    let history = History::new();
    let one = history.commits[0].clone();
    let before = ref_snapshot(&history.repo.root);
    let write = |name: &str, text: String| {
        let path = history.repo.home.join(name);
        fs::write(&path, text).unwrap();
        path
    };
    for (name, text, expect) in [
        (
            "version",
            "# arc-trailer-edits/2\n".to_string(),
            "arc-trailer-edits/2",
        ),
        (
            "fields",
            format!("# arc-trailer-edits/1\n\n{one}\tdrop\n"),
            "fields:3",
        ),
        ("action", format!("{one}\tdelete\t{UNKNOWN}\t\n"), "delete"),
        (
            "empty-replacement",
            format!("{one}\treplace\t{UNKNOWN}\t\n"),
            "empty-replacement:1",
        ),
        (
            "dropped-with-a-line",
            format!("{one}\tdrop\t{UNKNOWN}\t{REAL}\n"),
            "dropped-with-a-line:1",
        ),
        (
            "not-a-trailer",
            format!("{one}\treplace\t{UNKNOWN}\tjust some prose\n"),
            "not-a-trailer:1",
        ),
        (
            "old-not-a-trailer",
            format!("{one}\tdrop\tjust some prose\t\n"),
            "old-not-a-trailer:1",
        ),
    ] {
        let path = write(name, text);
        history
            .rewrite(&path)
            .assert()
            .failure()
            .stderr(predicates::str::contains(expect));
        history.assert_untouched(&before);
    }

    // A comment, a blank line and a drop whose trailing tab an editor
    // stripped are all read.
    let lenient = write(
        "lenient",
        format!("# arc-trailer-edits/1\n# why: the ledger observed no model\n\n{one}\tdrop\t{UNKNOWN}\n"),
    );
    history.rewrite(&lenient).assert().success();
    let now = history.rewritten();
    assert_eq!(history.message(&now[0]), format!("feat: one\n\n{REAL}"));
}

#[test]
fn edits_files_repeat_and_combine_with_drop_and_append_in_a_stated_order() {
    let history = History::new();
    let [one, two] = [0, 1].map(|n| history.commits[n].clone());
    let first = history.edits(
        "first",
        &[(&one, "replace", UNKNOWN, "Planned-by: codex:gpt-6.1#high")],
    );
    let second = history.edits("second", &[(&two, "drop", ORCHESTRATOR_UNKNOWN, "")]);

    history
        .repo
        .arc(&history.repo.root)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &one,
            "--no-sign",
            "--edits",
            first.to_str().unwrap(),
            "--edits",
            second.to_str().unwrap(),
            "--drop",
            "Signed-off-by",
            "--append",
            "Implemented-by: codex:gpt-6.1-sol#xhigh",
        ])
        .assert()
        .success();

    let now = history.rewritten();
    // Edits read the lines as the commit carries them, then --drop removes
    // whatever keys are left, then --append adds.
    assert_eq!(
        history.message(&now[0]),
        format!(
            "feat: one\n\nPlanned-by: codex:gpt-6.1#high\n{REAL}\nImplemented-by: codex:gpt-6.1-sol#xhigh"
        )
    );
    assert_eq!(
        history.message(&now[1]),
        format!("feat: two\n\n{ASSISTED}\nImplemented-by: codex:gpt-6.1-sol#xhigh")
    );
}

#[test]
fn a_dry_run_lists_each_edit_and_the_commits_it_would_recreate() {
    let history = History::new();
    let [one, two, three, four] = [0, 1, 2, 3].map(|n| history.commits[n].clone());
    let file = history.edits(
        "edits",
        &[
            (&one, "replace", UNKNOWN, "Planned-by: codex:gpt-6.1#high"),
            (&two, "drop", ORCHESTRATOR_UNKNOWN, ""),
        ],
    );
    let before = ref_snapshot(&history.repo.root);

    let out = stdout(history.rewrite(&file).arg("--dry-run"));

    assert!(
        out.contains(&format!(
            "{} replace {UNKNOWN} -> Planned-by: codex:gpt-6.1#high",
            &one[..8]
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!("{} drop {ORCHESTRATOR_UNKNOWN} -> ", &two[..8])),
        "{out}"
    );
    for recreated in [&one, &two, &three, &four] {
        assert!(
            out.lines().any(|line| line.starts_with(recreated.as_str())),
            "{recreated} is listed as recreated: {out}"
        );
    }
    assert!(out.contains("4 commits would be rewritten"), "{out}");
    history.assert_untouched(&before);
}

#[test]
fn a_rewrite_keeps_every_tree_author_committer_and_date() {
    let history = History::new();
    let [one, two] = [0, 1].map(|n| history.commits[n].clone());
    let file = history.edits(
        "edits",
        &[
            (&one, "replace", UNKNOWN, "Planned-by: codex:gpt-6.1#high"),
            (&two, "drop", ORCHESTRATOR_UNKNOWN, ""),
        ],
    );
    let old = history.commits.clone();

    history.rewrite(&file).assert().success();

    let new = history.rewritten();
    assert_eq!(new.len(), old.len());
    for (old, new) in old.iter().zip(&new) {
        assert_ne!(old, new, "every commit in the range sits on an edited one");
        let kept = identity(&history.repo.root, old);
        assert_eq!(kept.len(), 3, "{kept:?}");
        assert_eq!(identity(&history.repo.root, new), kept);
    }
}

#[test]
fn a_change_branch_keeps_its_pin_and_the_commit_the_review_read() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "alpha"])));
    let worktree = repo.home.join(".worktrees/repo-alpha");
    repo.commit(
        &worktree,
        "alpha.txt",
        "alpha\n",
        &format!("feat: alpha\n\n{UNKNOWN}\n"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", "alpha"]));
    let old = repo.head(&worktree);
    let path = repo.home.join("edits");
    fs::write(
        &path,
        format!(
            "# arc-trailer-edits/1\n{old}\treplace\t{UNKNOWN}\tPlanned-by: codex:gpt-6.1#high\n"
        ),
    )
    .unwrap();

    repo.arc(&worktree)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &old,
            "--no-sign",
            "--edits",
            path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let new = repo.head(&worktree);
    assert_ne!(new, old);
    assert_eq!(
        git_out(
            &worktree,
            &["rev-parse", &format!("refs/arc/keep/{change}/ps-01")]
        ),
        new
    );
    assert_eq!(
        git_out(
            &worktree,
            &[
                "rev-parse",
                &format!("refs/arc/retained/{change}/ps-01/{old}")
            ]
        ),
        old
    );
}

#[test]
fn a_signed_annotated_tag_in_range_is_recreated_signed() {
    let history = History::new();
    let key =
        signing_key(&history.repo).expect("signing acceptance requires a sandbox signing key");
    let [one, two] = [0, 1].map(|n| history.commits[n].clone());
    git_signing_write(
        &history.repo,
        &key,
        &[
            "tag",
            "-a",
            "-s",
            "-u",
            &key.fingerprint,
            "-m",
            "the first release",
            "v0.1.0",
            &two,
        ],
    );
    let old_tag = git_out(&history.repo.root, &["rev-parse", "v0.1.0"]);
    let file = history.edits(
        "edits",
        &[
            (&one, "replace", UNKNOWN, "Planned-by: codex:gpt-6.1#high"),
            (&two, "drop", ORCHESTRATOR_UNKNOWN, ""),
        ],
    );
    let old = history.commits.clone();

    arc_signing(&history.repo, &key, &history.repo.root)
        .args([
            "rewrite",
            "trailers",
            "--from",
            &one,
            "--key",
            &key.fingerprint,
            "--retag",
            "--edits",
            file.to_str().unwrap(),
        ])
        .assert()
        .success();

    let new = history.rewritten();
    let tag = git_out(&history.repo.root, &["rev-parse", "v0.1.0"]);
    assert_ne!(tag, old_tag, "the tag object is recreated");
    assert_eq!(
        git_out(&history.repo.root, &["rev-parse", "v0.1.0^{commit}"]),
        new[1],
        "the tag names the commit that replaced its target"
    );
    git_signing(&history.repo.root, &key, &["verify-tag", "v0.1.0"]);
    for (old, new) in old.iter().zip(&new) {
        git_signing(&history.repo.root, &key, &["verify-commit", new]);
        assert_eq!(
            identity(&history.repo.root, new),
            identity(&history.repo.root, old)
        );
    }
}

#[test]
fn the_help_states_the_format_and_the_order() {
    let repo = Repo::new();
    let help = stdout(repo.arc(&repo.root).args(["rewrite", "trailers", "--help"]));
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("--edits <FILE>"), "{flat}");
    assert!(flat.contains("arc-trailer-edits/1"), "{flat}");
    assert!(
        flat.contains("edits apply first, then --drop, then --append"),
        "{flat}"
    );
}
