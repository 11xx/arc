//! `arc rewrite sign`: re-signing a branch's history without stranding
//! anything the ledger recorded about it.

use crate::common::*;
use std::collections::BTreeMap;

/// A throwaway signing key in a keyring of its own, so the suite never reads
/// or writes the machine's.
struct Key {
    home: PathBuf,
    fingerprint: String,
}

fn signing_key(repo: &Repo) -> Option<Key> {
    let home = repo.home.join("gnupg");
    fs::create_dir_all(&home).unwrap();
    // gpg refuses a keyring directory anybody else can read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let generated = Command::new("gpg")
        .args([
            "--batch",
            "--quiet",
            "--passphrase",
            "",
            "--quick-generate-key",
            "Arc Rewrite Fixture <fixture@example.invalid>",
            "default",
            "default",
            "never",
        ])
        .env("GNUPGHOME", &home)
        .output();
    let generated = match generated {
        Ok(generated) if generated.status.success() => generated,
        // A machine with no gpg, or one whose agent cannot start, cannot
        // demonstrate a signing rewrite at all: recreating commits without
        // signing them reproduces their exact ids, so there would be no
        // rewrite to check.
        _ => {
            eprintln!("skipped: no usable gpg in this environment");
            return None;
        }
    };
    drop(generated);
    let listed = Command::new("gpg")
        .args(["--list-secret-keys", "--with-colons"])
        .env("GNUPGHOME", &home)
        .output()
        .unwrap();
    let fingerprint = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("fpr:"))
        .and_then(|line| line.split(':').find(|field| field.len() == 40))
        .map(str::to_string)?;
    Some(Key { home, fingerprint })
}

/// `arc`, with the fixture keyring in scope.
fn arc_signing(repo: &Repo, key: &Key, cwd: &Path) -> AssertCommand {
    let mut cmd = repo.arc(cwd);
    cmd.env("GNUPGHOME", &key.home);
    cmd
}

/// Git, with the fixture keyring in scope, for the reads that verify a
/// signature rather than merely notice one.
fn git_signing(cwd: &Path, key: &Key, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GNUPGHOME", &key.home)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn repo_with_gates() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare gates"]);
    repo
}

fn opened_change_id(out: &str) -> String {
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

/// Every distinct full object name in a text.
fn revisions_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut start = 0;
    while start < bytes.len() {
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
            end += 1;
        }
        if end - start == 40 {
            let candidate: String = bytes[start..end].iter().collect();
            if !found.contains(&candidate) {
                found.push(candidate);
            }
        }
        start = if end > start { end } else { start + 1 };
    }
    found
}

/// What the recorded rewrite says became of each of these revisions, read
/// back through arc's own resolver rather than from what the rewrite printed.
fn recorded_map(repo: &Repo, revisions: &[String]) -> BTreeMap<String, String> {
    revisions
        .iter()
        .filter_map(|revision| {
            let out = stdout(repo.arc(&repo.root).args(["history", "resolve", revision]));
            let successor = out.split('→').nth(1)?.trim().to_string();
            (successor.len() == 40).then(|| (revision.clone(), successor))
        })
        .collect()
}

fn apply_map(text: &str, map: &BTreeMap<String, String>) -> String {
    let mut mapped = text.to_string();
    for (old, new) in map {
        mapped = mapped.replace(old, new);
    }
    // How long ago something happened is a clock reading rather than a
    // recorded fact, and it moves between any two readings.
    mapped
        .lines()
        .map(|line| match line.split_once("\"age_seconds\":") {
            Some((before, _)) => format!("{before}\"age_seconds\": _"),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything the ledger says about every change, plus the journal queue,
/// one labelled reading at a time so a difference names where it is.
fn dump(repo: &Repo, changes: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for change_id in changes {
        for command in ["status", "show"] {
            let read = repo
                .arc(&repo.root)
                .args([command, change_id, "--json"])
                .output()
                .unwrap();
            assert!(
                read.status.success(),
                "arc {command} {change_id} failed: {}",
                String::from_utf8_lossy(&read.stderr)
            );
            out.push((
                format!("{command} {change_id}"),
                String::from_utf8_lossy(&read.stdout).into_owned(),
            ));
        }
    }
    out.push((
        "journal open".to_string(),
        stdout(repo.arc(&repo.root).args(["journal", "open", "--json"])),
    ));
    out
}

/// Assert two dumps agree, once every revision in the first is followed
/// through the recorded map.
fn assert_same(
    before: &[(String, String)],
    after: &[(String, String)],
    map: &BTreeMap<String, String>,
    why: &str,
) {
    assert_eq!(before.len(), after.len());
    for ((label, before), (_, after)) in before.iter().zip(after) {
        assert_eq!(
            apply_map(before, map),
            apply_map(after, &BTreeMap::new()),
            "{why}: {label}"
        );
    }
}

/// The benchmark: a repository holding every kind of recorded revision reads
/// exactly the same after its history is re-signed, once each revision is
/// followed through the recorded map.
#[test]
fn resigning_the_history_leaves_every_recorded_revision_resolvable() {
    let repo = repo_with_gates();
    let Some(key) = signing_key(&repo) else {
        return;
    };

    // An integrated change carrying gate evidence and a verdict.
    let alpha = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "alpha"])));
    let alpha_tree = repo.home.join(".worktrees/repo-alpha");
    repo.commit(&alpha_tree, "alpha.txt", "alpha\n", "feat: alpha");
    stdout(repo.arc(&alpha_tree).args(["snapshot", "alpha"]));
    repo.arc(&alpha_tree)
        .args(["verify", "alpha", "--all"])
        .assert()
        .success();
    repo.arc(&alpha_tree)
        .args(["review", "alpha", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "alpha"])
        .assert()
        .success();

    // A second integrated change, whose evidence includes a run against the
    // target and one a dirty-tree waiver let count.
    let beta = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "beta"])));
    let beta_tree = repo.home.join(".worktrees/repo-beta");
    repo.commit(&beta_tree, "beta.txt", "beta\n", "feat: beta");
    stdout(repo.arc(&beta_tree).args(["snapshot", "beta"]));
    repo.arc(&repo.root)
        .args(["verify", &beta, "--against", "master"])
        .assert()
        .success();
    fs::write(beta_tree.join("scratch.txt"), "dirt\n").unwrap();
    repo.arc(&beta_tree)
        .args([
            "verify",
            "beta",
            "--all",
            "--waive-dirty",
            "the fixture leaves a scratch file behind",
        ])
        .assert()
        .success();
    fs::remove_file(beta_tree.join("scratch.txt")).unwrap();
    repo.arc(&beta_tree)
        .args(["review", "beta", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "beta"])
        .assert()
        .success();

    // An open change, and a fork: both hold branches that point into the
    // history about to be rewritten.
    let gamma = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", "gamma"])));
    let gamma_tree = repo.home.join(".worktrees/repo-gamma");
    let gamma_base = repo.head(&repo.root);
    repo.commit(&gamma_tree, "gamma.txt", "gamma\n", "feat: gamma");
    stdout(repo.arc(&gamma_tree).args(["snapshot", "gamma"]));
    repo.arc(&repo.root)
        .args(["fork", "begin", "spike"])
        .assert()
        .success();

    // The dumps that must survive the rewrite unchanged belong to the
    // integrated changes: their commits are on the branch being rewritten, so
    // every revision they recorded is in the map. An open change's own commits
    // are not, which is a separate fact the rewrite reports rather than fixes.
    // A journal artifact checked at the project anchor: its stamp records a
    // revision on the branch about to be rewritten, and a reader must not
    // conclude the anchor moved when only its name did.
    let artifact = stdout(repo.arc(&repo.root).args(["journal", "open", "--json"]));
    let artifact: serde_json::Value = serde_json::from_str(&artifact).unwrap();
    let artifact = artifact["open"][0]["file"].as_str().unwrap().to_string();
    repo.arc(&repo.root)
        .args(["journal", "verified", &artifact])
        .assert()
        .success();

    let changes = vec![alpha.clone(), beta.clone()];
    let before = dump(&repo, &changes);
    assert_same(
        &before,
        &dump(&repo, &changes),
        &BTreeMap::new(),
        "the dump has to be stable before it can be compared across a rewrite",
    );

    let rewritten = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
    ]));
    assert!(rewritten.contains("rewritten on master"), "{rewritten}");
    // A branch whose tip is one of the rewritten commits is carried across by
    // moving its ref, which is the fork's case: it holds no commit of its own
    // yet.
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "fork/spike"]),
        git_out(&repo.root, &["rev-parse", "master"]),
        "a fork sitting on the branch tip moves with it"
    );
    // A branch with commits of its own on top of the replaced line has no
    // successor to move to, and the rewrite says which branches those are and
    // how to replay them rather than leaving them to be discovered.
    assert!(
        rewritten.lines().any(|line| line.starts_with("stranded: ")
            && line.contains("refs/heads/arc/gamma")
            && line.contains("git rebase --onto")),
        "{rewritten}"
    );

    // Every commit on the branch now carries the fixture's signature. Git
    // verifies it through gpg, which needs the fixture keyring to find the key
    // that made it.
    let signatures = git_signing(&repo.root, &key, &["log", "--format=%G? %GK", "master"]);
    for line in signatures.lines() {
        let (verification, signer) = line.split_once(' ').unwrap_or((line, ""));
        assert_eq!(verification, "G", "{signatures}");
        assert!(
            key.fingerprint.ends_with(signer),
            "signed by {signer}, not the fixture key: {signatures}"
        );
    }

    // Trees never change under a signing rewrite, so the content the branch
    // holds is the content it held.
    let after = dump(&repo, &changes);
    let mut recorded: Vec<String> = before
        .iter()
        .flat_map(|(_, text)| revisions_in(text))
        .collect();
    recorded.push(gamma_base.clone());
    let map = recorded_map(&repo, &recorded);
    assert!(!map.is_empty(), "the rewrite recorded no mapping");
    assert_same(
        &before,
        &after,
        &map,
        "a recorded revision reads differently than the map accounts for",
    );

    // Replaying the stranded branch onto the new line makes it a branch of
    // this history again, and the change it belongs to reads normally.
    git(
        &gamma_tree,
        &["rebase", "--onto", "master", &map[&gamma_base]],
    );
    repo.arc(&repo.root)
        .args(["status", &gamma, "--json"])
        .assert()
        .success();

    let doctor = stdout(repo.arc(&repo.root).args(["doctor", "--json"]));
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    assert_eq!(
        doctor["problems"].as_array().map(Vec::len),
        Some(0),
        "{doctor}"
    );
}

/// A rewrite that only re-signs is idempotent. gpg signs differently every
/// time, so a second run that believed it had work to do would rewrite the
/// history again, and again, forever.
#[test]
fn a_history_already_signed_by_the_key_is_left_alone() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    repo.commit(&repo.root, "one.txt", "one\n", "feat: one");

    arc_signing(&repo, &key, &repo.root)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .assert()
        .success();
    let head = repo.head(&repo.root);

    let second = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        // A key id is a suffix of its fingerprint, and Git reports whichever
        // the signature carries.
        "--key",
        &key.fingerprint[24..],
    ]));
    assert!(second.contains("nothing to do"), "{second}");
    assert_eq!(repo.head(&repo.root), head, "nothing moved");
}

/// Recreating commits without signing them reproduces their exact ids: the
/// author, committer, dates, encoding and message bytes all travel through
/// unchanged, so nothing but a signature could have changed.
#[test]
fn recreating_a_commit_unsigned_reproduces_it_exactly() {
    let repo = Repo::new();
    fs::write(repo.root.join("odd.txt"), "odd\n").unwrap();
    git(&repo.root, &["add", "."]);
    // A message that would not survive being decoded, reformatted, or
    // stripped: non-ASCII, an interior blank line, and trailing whitespace.
    git(
        &repo.root,
        &[
            "-c",
            "i18n.commitEncoding=ISO-8859-1",
            "commit",
            "--cleanup=verbatim",
            "-m",
            "f\u{e9}at: caf\u{e9}   \n\n\nbody\n",
        ],
    );
    let before = git_out(&repo.root, &["log", "--format=%H", "master"]);

    let out = stdout(repo.arc(&repo.root).args(["rewrite", "sign", "--no-sign"]));
    assert!(out.contains("changed none of them"), "{out}");
    assert_eq!(
        before,
        git_out(&repo.root, &["log", "--format=%H", "master"]),
        "an unsigned recreation is the commit it recreated"
    );
}

/// A dry run answers with the map and changes nothing.
#[test]
fn a_dry_run_moves_and_records_nothing() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    repo.commit(&repo.root, "one.txt", "one\n", "feat: one");
    let head = repo.head(&repo.root);

    let out = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
        "--dry-run",
    ]));
    assert!(out.contains("nothing was moved or recorded"), "{out}");
    assert!(out.contains(&head), "the map names the head: {out}");
    assert_eq!(repo.head(&repo.root), head);
    assert!(
        !intent_path(&repo).exists(),
        "a dry run commits to nothing, so there is nothing to finish"
    );
    repo.arc(&repo.root)
        .args(["history", "resolve", &head])
        .assert()
        .code(2);
}

/// Refusals where carrying on would strand somebody's work or guess at it.
#[test]
fn a_dirty_worktree_and_a_detached_head_are_refused() {
    let repo = Repo::new();
    fs::write(repo.root.join("dirt.txt"), "dirt\n").unwrap();
    repo.arc(&repo.root)
        .args(["rewrite", "sign", "--no-sign"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("uncommitted changes"));
    fs::remove_file(repo.root.join("dirt.txt")).unwrap();

    git(&repo.root, &["checkout", "--detach", "HEAD"]);
    repo.arc(&repo.root)
        .args(["rewrite", "sign", "--no-sign"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("detached"));
}

/// A recorded rewrite whose successor is not here either explains nothing.
/// Reporting it as a rewrite would suppress the warning while naming a commit
/// no reader can reach, so it is a problem rather than advice.
#[test]
fn a_rewrite_leading_to_a_missing_commit_is_a_problem() {
    let repo = Repo::new();
    let (_, worktree, recorded) = change_with_patchset(&repo, "stranded");

    // Amending and pruning is how a commit stops being here at all: the
    // retention refs arc pins it with are what keep it, so they go too.
    let amend_and_prune = |message: &str| {
        git(&worktree, &["commit", "--amend", "-m", message]);
        let head = repo.head(&worktree);
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
        head
    };
    let successor = amend_and_prune("test: add stranded, rewritten");

    let map = repo.root.join("commit-map");
    fs::write(&map, format!("{recorded} {successor}\n")).unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "amended the snapshot",
        ])
        .assert()
        .success();
    let advised = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert!(
        advised.contains("revision-rewritten") && !advised.contains("unresolved-revision"),
        "a rewrite whose successor is present is advice: {advised}"
    );

    // The successor goes the same way as the revision it replaced.
    amend_and_prune("test: add stranded, rewritten again");

    let doctor = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert!(doctor.contains("unresolved-revision"), "{doctor}");
    repo.arc(&repo.root).args(["doctor"]).assert().failure();
}

/// A merge of a signed tag carries a `mergetag` header holding the whole tag
/// object. Recreating the commit from a fixed set of headers would drop it,
/// so the rewrite assembles such a commit itself and says which ones it did
/// that for.
#[test]
fn a_commit_carrying_another_header_keeps_it() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    // A signed annotated tag on a side branch, merged with a merge commit, is
    // how Git writes a `mergetag`.
    git(&repo.root, &["checkout", "-q", "-b", "side"]);
    repo.commit(&repo.root, "side.txt", "side\n", "feat: side");
    let signed_tag = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo.root)
            .env("GNUPGHOME", &key.home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    signed_tag(&[
        "tag",
        "-a",
        "-s",
        "-u",
        &key.fingerprint,
        "-m",
        "the side release",
        "vside",
    ]);
    git(&repo.root, &["checkout", "-q", "master"]);
    signed_tag(&["merge", "--no-ff", "-q", "-m", "merge: side", "vside"]);

    let merge = repo.head(&repo.root);
    let before = git_out(&repo.root, &["cat-file", "commit", &merge]);
    assert!(before.contains("mergetag object "), "{before}");
    let tree = git_out(&repo.root, &["rev-parse", &format!("{merge}^{{tree}}")]);

    let out = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
    ]));
    assert!(
        out.lines()
            .any(|line| line.starts_with("carried through on ") && line.contains("mergetag")),
        "the rewrite says which commits carried a header: {out}"
    );

    let after = repo.head(&repo.root);
    let rebuilt = git_out(&repo.root, &["cat-file", "commit", &after]);
    // The header travels byte for byte: it holds the tag object that was
    // merged, which is a fact about the merge rather than a reference the
    // rewrite could remap.
    let carried = |text: &str| {
        text.lines()
            .skip_while(|line| !line.starts_with("mergetag "))
            .take_while(|line| line.starts_with("mergetag ") || line.starts_with(' '))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(carried(&before), carried(&rebuilt), "{rebuilt}");
    assert_eq!(
        tree,
        git_out(&repo.root, &["rev-parse", &format!("{after}^{{tree}}")]),
        "a signing rewrite never changes a tree"
    );
    // The signature the rewrite made covers the object it assembled, headers
    // and all.
    assert_eq!(
        git_signing(&repo.root, &key, &["log", "-1", "--format=%G?", &after]),
        "G",
        "{rebuilt}"
    );
    // Every object the rewrite wrote by hand has to satisfy Git itself.
    let fsck = Command::new("git")
        .args(["fsck", "--no-progress"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "git fsck rejected the rewritten objects: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

/// Git, with the fixture keyring in scope, for the writes that make a
/// signature rather than read one.
fn git_signing_write(repo: &Repo, key: &Key, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(&repo.root)
        .env("GNUPGHOME", &key.home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A history whose second commit carries a signed annotated tag, with a
/// commit on top of it so the tag is not the branch tip.
fn repo_with_a_signed_tag(repo: &Repo, key: &Key) -> String {
    repo.commit(&repo.root, "one.txt", "one\n", "feat: one");
    git_signing_write(
        repo,
        key,
        &[
            "tag",
            "-a",
            "-s",
            "-u",
            &key.fingerprint,
            "-m",
            "the first release\n\nwith a body\n",
            "v0.1.0",
        ],
    );
    repo.commit(&repo.root, "two.txt", "two\n", "feat: two");
    git_out(&repo.root, &["rev-parse", "v0.1.0"])
}

/// An annotated tag names its commit through its own object, so a rewrite
/// that moves refs leaves it naming the commit it named. Reporting that has
/// to say what it costs, because a release boundary nothing on the branch
/// reaches is not a visible failure.
#[test]
fn an_annotated_tag_left_alone_is_reported_with_the_consequence() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    let old_tag = repo_with_a_signed_tag(&repo, &key);

    let left = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
    ]));
    let reported = left
        .lines()
        .find(|line| line.starts_with("left alone: ") && line.contains("refs/tags/v0.1.0"))
        .unwrap_or_else(|| panic!("{left}"));
    assert!(reported.contains("git describe"), "{reported}");
    assert!(reported.contains("--retag"), "{reported}");
    assert_eq!(git_out(&repo.root, &["rev-parse", "v0.1.0"]), old_tag);
    let describe = Command::new("git")
        .args(["describe", "--tags", "HEAD"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert!(
        !describe.status.success(),
        "a tag left on the replaced line describes nothing on this history: {}",
        String::from_utf8_lossy(&describe.stdout)
    );
}

/// `--retag` recreates the tag on the commit that replaced its target,
/// carrying everything else the tag said.
#[test]
fn an_annotated_tag_is_re_pointed_on_request() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    let old_tag = repo_with_a_signed_tag(&repo, &key);
    let tagger = git_out(
        &repo.root,
        &[
            "for-each-ref",
            "--format=%(taggerdate:raw) %(taggeremail)",
            "refs/tags/v0.1.0",
        ],
    );

    let retagged = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
        "--retag",
    ]));
    assert!(
        retagged
            .lines()
            .any(|line| line.starts_with("re-pointed refs/tags/v0.1.0:")),
        "{retagged}"
    );
    let new_tag = git_out(&repo.root, &["rev-parse", "v0.1.0"]);
    assert_ne!(new_tag, old_tag, "a re-pointed tag is a new object");
    assert!(
        retagged.contains(&new_tag[..8]) && retagged.contains(&old_tag[..8]),
        "the report names both tag objects: {retagged}"
    );
    // What the tag says is the tag's own: only the commit it names changed.
    assert_eq!(
        git_out(
            &repo.root,
            &["for-each-ref", "--format=%(contents)", "refs/tags/v0.1.0"]
        )
        .lines()
        .take_while(|line| !line.starts_with("-----BEGIN PGP SIGNATURE-----"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string(),
        "the first release\n\nwith a body",
    );
    assert_eq!(
        git_out(
            &repo.root,
            &[
                "for-each-ref",
                "--format=%(taggerdate:raw) %(taggeremail)",
                "refs/tags/v0.1.0"
            ]
        ),
        tagger,
        "the tagger and the date are the original's"
    );
    assert_eq!(
        git_signing(
            &repo.root,
            &key,
            &["tag", "--format=%(objectname)", "--verify", "v0.1.0"]
        ),
        new_tag,
        "a tag that was signed is signed again"
    );
    // The point of re-pointing: the release boundary is on this history, so
    // `git describe` measures the branch from the tag again.
    assert_eq!(
        git_out(&repo.root, &["describe", "--tags", "HEAD"])
            .split('-')
            .next(),
        Some("v0.1.0")
    );
    let fsck = Command::new("git")
        .args(["fsck", "--no-progress"])
        .current_dir(&repo.root)
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "git fsck rejected the tag object: {}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

/// Every repository-scoped event file, oldest first. A repository that has
/// recorded none has no directory, which is an answer rather than an error.
fn repository_events(repo: &Repo) -> Vec<PathBuf> {
    let dir = repo.root.join(".git/arc/repository/events");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    paths
}

/// Two recorded rewrites that disagree about one revision leave the map
/// unreadable, and an unreadable map is not an empty one.
///
/// Answering as though no rewrite had been recorded is the worst available
/// answer: every projection reverts to pre-rewrite revisions and presents
/// commits this repository does not hold as the current state, with nothing
/// on screen to say the map was even consulted. So a projection refuses, and
/// says where the contradiction is reported.
#[test]
fn a_contradictory_rewrite_record_refuses_a_projection_rather_than_reverting_it() {
    let repo = Repo::new();
    let (change, worktree, recorded) = change_with_patchset(&repo, "contested");
    repo.commit(&worktree, "b.txt", "b\n", "test: a successor");
    let successor = repo.head(&worktree);

    let map = repo.root.join("commit-map");
    fs::write(&map, format!("{recorded} {successor}\n")).unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "moved the snapshot",
        ])
        .assert()
        .success();
    let resolved = stdout(repo.arc(&repo.root).args(["status", &change]));
    assert!(resolved.contains(&successor[..8]), "{resolved}");

    // A second event naming a different successor for the same revision. The
    // write paths refuse this pair, so it arrives the only way it can: as an
    // event file that did not come through them.
    let recorded_event = repository_events(&repo).pop().unwrap();
    let body = fs::read_to_string(&recorded_event).unwrap();
    let other = repo.head(&repo.root);
    assert_ne!(other, successor);
    let events = recorded_event.parent().unwrap();
    fs::write(
        events.join("01ZZZZZZZZZZZZZZZZZZZZZZZZ.json"),
        body.replace(&successor, &other).replace(
            recorded_event.file_stem().unwrap().to_str().unwrap(),
            "01ZZZZZZZZZZZZZZZZZZZZZZZZ",
        ),
    )
    .unwrap();

    let refused = repo
        .arc(&repo.root)
        .args(["status", &change])
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "a projection built on an unreadable map must refuse"
    );
    let complaint = String::from_utf8_lossy(&refused.stderr);
    assert!(
        complaint.contains("arc doctor"),
        "the refusal points at where the contradiction is reported: {complaint}"
    );
    assert!(
        !String::from_utf8_lossy(&refused.stdout).contains(&recorded[..8]),
        "a refused projection reports no revision at all, least of all the pre-rewrite one"
    );

    let doctor = stdout(repo.arc(&repo.root).args(["doctor"]));
    assert!(doctor.contains("invalid-rewrite-mapping"), "{doctor}");
}

/// An abbreviation two recorded revisions answer names neither.
///
/// Resolution answers what a recorded revision is called here, so it has to
/// distinguish three things: a revision no rewrite touched, one a rewrite
/// moved, and a query the map cannot resolve. Picking whichever recorded
/// revision came first would answer the third as the second and send a reader
/// to somebody else's commit.
#[test]
fn an_ambiguous_abbreviation_is_refused_rather_than_resolved() {
    let repo = Repo::new();
    let base = repo.head(&repo.root);
    repo.commit(&repo.root, "a.txt", "a\n", "test: one");
    let first = repo.head(&repo.root);
    repo.commit(&repo.root, "b.txt", "b\n", "test: two");
    let second = repo.head(&repo.root);

    // Two recorded revisions sharing a prefix, which is what makes an
    // abbreviation of that prefix ambiguous.
    let shared = format!("{}0", &base[..7]);
    let map = repo.root.join("commit-map");
    fs::write(
        &map,
        format!("{shared}1 {first}\n{shared}2 {second}\n{base} {first}\n"),
    )
    .unwrap();
    repo.arc(&repo.root)
        .args([
            "history",
            "rewrite",
            "--map",
            map.to_str().unwrap(),
            "--reason",
            "two commits sharing a prefix",
        ])
        .assert()
        .success();

    // The full revision resolves; the shared prefix does not.
    let resolved = stdout(repo.arc(&repo.root).args(["history", "resolve", &base]));
    assert!(resolved.contains(&first[..8]), "{resolved}");
    let refused = repo
        .arc(&repo.root)
        .args(["history", "resolve", &shared])
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "an ambiguous abbreviation is not a resolution"
    );
    let complaint = String::from_utf8_lossy(&refused.stderr);
    assert!(
        complaint.contains(&format!("{shared}1")) && complaint.contains(&format!("{shared}2")),
        "the refusal names the candidates: {complaint}"
    );

    // Below the length at which a prefix names a commit it is not an
    // abbreviation of one, so it resolves to nothing rather than to a refusal.
    repo.arc(&repo.root)
        .args(["history", "resolve", &base[..4]])
        .assert()
        .code(2);
}

/// Where a rewrite writes down what it is about to do.
fn intent_path(repo: &Repo) -> PathBuf {
    repo.root.join(".git/arc/repository/rewrite-intent.json")
}

/// A history whose oldest commit is pinned by a retention ref and named by a
/// tag, so a rewrite of it has refs of three kinds to move at once.
fn repo_with_pinned_refs(repo: &Repo) -> String {
    let pinned = repo.head(&repo.root);
    repo.commit(&repo.root, "one.txt", "one\n", "feat: one");
    repo.commit(&repo.root, "two.txt", "two\n", "feat: two");
    git(
        &repo.root,
        &["update-ref", "refs/arc/keep/probe/ps-01", &pinned],
    );
    git(&repo.root, &["tag", "pinned", &pinned]);
    pinned
}

/// A rewrite interrupted before it moved anything leaves every ref where it
/// was and its map on disk, and the next run finishes it rather than starting
/// over.
///
/// Starting over is the defect: the commits would be signed again, gpg signs
/// differently every time, and the map would then claim a second successor
/// for every commit in range.
#[test]
fn a_rewrite_interrupted_before_its_refs_move_is_finished_by_the_next_run() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    let pinned = repo_with_pinned_refs(&repo);
    let before = git_out(&repo.root, &["log", "--format=%H", "master"]);

    let interrupted = arc_signing(&repo, &key, &repo.root)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .env("ARC_REWRITE_INTERRUPT", "intent")
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    assert_eq!(
        before,
        git_out(&repo.root, &["log", "--format=%H", "master"]),
        "an interruption before the transaction moves no ref"
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "refs/arc/keep/probe/ps-01"]),
        pinned,
        "nor any of the refs that move with it"
    );
    assert!(
        intent_path(&repo).exists(),
        "the map is on disk to be finished"
    );
    // Nothing is recorded yet, so the rewritten commit still resolves to
    // itself.
    repo.arc(&repo.root)
        .args(["history", "resolve", &pinned])
        .assert()
        .code(2);

    let finished = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
    ]));
    assert!(
        finished.contains("finishing the rewrite of master"),
        "{finished}"
    );
    assert_resumed(&repo, &pinned, &key);
}

/// A rewrite interrupted after its refs moved and before its map was
/// recorded leaves the refs where the rewrite put them, and the next run
/// records the map for those same commits.
#[test]
fn a_rewrite_interrupted_before_its_map_is_recorded_records_it_once() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    let pinned = repo_with_pinned_refs(&repo);
    let before = git_out(&repo.root, &["log", "--format=%H", "master"]);

    let interrupted = arc_signing(&repo, &key, &repo.root)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .env("ARC_REWRITE_INTERRUPT", "refs")
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    let after = git_out(&repo.root, &["log", "--format=%H", "master"]);
    assert_ne!(
        before, after,
        "the transaction committed before the interruption"
    );
    // Every ref moved together or none did, so the refs arc keeps are on the
    // same history as the branch.
    assert_ne!(
        git_out(&repo.root, &["rev-parse", "refs/arc/keep/probe/ps-01"]),
        pinned
    );
    assert!(intent_path(&repo).exists());
    // The refs moved and the map did not, which is the window in which the
    // ledger and Git disagree.
    repo.arc(&repo.root)
        .args(["history", "resolve", &pinned])
        .assert()
        .code(2);

    let finished = stdout(arc_signing(&repo, &key, &repo.root).args([
        "rewrite",
        "sign",
        "--key",
        &key.fingerprint,
    ]));
    assert!(
        finished.contains("finishing the rewrite of master"),
        "{finished}"
    );
    assert_eq!(
        after,
        git_out(&repo.root, &["log", "--format=%H", "master"]),
        "finishing the recording recreates no commit"
    );
    assert_resumed(&repo, &pinned, &key);
}

/// What a finished rewrite must look like however far the interrupted run
/// got: one recorded map, one successor per rewritten commit, every ref on
/// the rewritten history, and the whole history signed.
fn assert_resumed(repo: &Repo, pinned: &str, key: &Key) {
    assert!(
        !intent_path(repo).exists(),
        "a finished rewrite keeps no intent"
    );
    let events = repository_events(repo);
    assert_eq!(
        events.len(),
        1,
        "a rewrite finished in two runs is one recorded rewrite, not two"
    );
    let mapping = fs::read_to_string(&events[0]).unwrap();
    assert_eq!(
        mapping.matches(pinned).count(),
        1,
        "the recorded map names one successor for the rewritten commit: {mapping}"
    );

    let resolved = stdout(repo.arc(&repo.root).args(["history", "resolve", pinned]));
    let successor = resolved.split_whitespace().next_back().unwrap().to_string();
    assert_ne!(successor, pinned);
    // Every ref that named the rewritten commit names its successor, and the
    // successor is what the whole history is built on.
    for name in ["refs/arc/keep/probe/ps-01", "refs/tags/pinned"] {
        assert_eq!(
            git_out(&repo.root, &["rev-parse", name]),
            successor,
            "{name} was left on the replaced line"
        );
    }
    assert!(git_out(&repo.root, &["log", "--format=%H", "master"]).contains(&successor));
    let signatures = git_signing(&repo.root, key, &["log", "--format=%G? %GK", "master"]);
    for line in signatures.lines() {
        let (verification, signer) = line.split_once(' ').unwrap_or((line, ""));
        assert_eq!(
            verification, "G",
            "a finished rewrite leaves the whole history signed: {signatures}"
        );
        assert!(key.fingerprint.ends_with(signer), "{signatures}");
    }
    repo.arc(&repo.root).args(["doctor"]).assert().success();
}

/// A ref an unfinished rewrite is holding, moved by something else in the
/// meantime, stops the rewrite rather than being overwritten.
///
/// The intent says where each ref was and where it is going. A third value
/// means somebody decided where that ref points, and choosing between their
/// decision and the rewrite's is not arc's to make — so it says which ref and
/// leaves everything where it is.
#[test]
fn a_ref_moved_out_from_under_an_unfinished_rewrite_is_refused() {
    let repo = Repo::new();
    let Some(key) = signing_key(&repo) else {
        return;
    };
    let pinned = repo_with_pinned_refs(&repo);
    let before = git_out(&repo.root, &["log", "--format=%H", "master"]);

    arc_signing(&repo, &key, &repo.root)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .env("ARC_REWRITE_INTERRUPT", "intent")
        .output()
        .unwrap();
    let elsewhere = repo.head(&repo.root);
    git(
        &repo.root,
        &["update-ref", "refs/arc/keep/probe/ps-01", &elsewhere],
    );

    let refused = arc_signing(&repo, &key, &repo.root)
        .args(["rewrite", "sign", "--key", &key.fingerprint])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let complaint = String::from_utf8_lossy(&refused.stderr);
    assert!(
        complaint.contains("refs/arc/keep/probe/ps-01") && complaint.contains(&elsewhere[..8]),
        "the refusal names the ref and what it holds: {complaint}"
    );
    assert_eq!(
        before,
        git_out(&repo.root, &["log", "--format=%H", "master"]),
        "a refused transaction moves nothing"
    );
    assert_eq!(
        git_out(&repo.root, &["rev-parse", "refs/tags/pinned"]),
        pinned
    );
    assert!(
        intent_path(&repo).exists(),
        "the rewrite is still there to be finished once the ref is settled"
    );
    assert!(
        repository_events(&repo).is_empty(),
        "and nothing is recorded"
    );
}
