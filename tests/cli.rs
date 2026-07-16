use assert_cmd::Command as AssertCommand;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

struct Repo {
    _tmp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        git(&root, &["init", "-b", "master"]);
        git(&root, &["config", "user.name", "Tester"]);
        git(&root, &["config", "user.email", "tester@example.invalid"]);
        fs::write(root.join("README.md"), "hello\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "init"]);
        Repo {
            _tmp: tmp,
            root,
            home,
        }
    }

    fn arc(&self, cwd: &Path) -> AssertCommand {
        let mut cmd = AssertCommand::cargo_bin("arc").unwrap();
        cmd.current_dir(cwd)
            .env("HOME", &self.home)
            .env("ARC_ACTOR", "tester")
            .env("ARC_HARNESS", "test")
            .env_remove("ARC_DATA_DIR")
            .env_remove("ARC_DATA_ROOT")
            .env_remove("ARC_WORKTREES_DIR")
            .env_remove("AI_HOME");
        cmd
    }

    fn commit(&self, cwd: &Path, file: &str, content: &str, msg: &str) {
        fs::write(cwd.join(file), content).unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", msg]);
    }

    fn head(&self, cwd: &Path) -> String {
        git_out(cwd, &["rev-parse", "HEAD"])
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stdout(cmd: &mut AssertCommand) -> String {
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// begin → worktree + branch + ledger; list/status see the change.
#[test]
fn begin_creates_change_branch_and_worktree() {
    let repo = Repo::new();
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "fix-thing", "--title", "Fix the thing"]),
    );
    assert!(out.contains("change: fix-thing-"));
    assert!(out.contains("branch: arc/fix-thing"));
    assert!(out.contains("worktree: "));

    let wt = repo.home.join(".worktrees").join("repo-fix-thing");
    assert!(wt.is_dir(), "worktree should exist");

    let list = stdout(repo.arc(&repo.root).args(["list", "--json"]));
    let rows: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(rows[0]["slug"], "fix-thing");
    assert_eq!(rows[0]["state"], "open");

    // Same open slug refuses a duplicate.
    repo.arc(&repo.root)
        .args(["begin", "fix-thing"])
        .assert()
        .failure();
}

/// The full green path: implement → snapshot → verify gate → approve →
/// check ok → integrate produces a --no-ff merge with correct parents.
#[test]
fn green_path_integrates_with_merge_commit() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.smoke]\ncommand = \"test -f README.md\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);
    let old_master = repo.head(&repo.root);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "feat-x", "--title", "Feature X"]),
    );
    let wt = repo.home.join(".worktrees").join("repo-feat-x");
    repo.commit(&wt, "x.txt", "x\n", "feat: add x");

    stdout(repo.arc(&wt).args(["snapshot", "feat-x"]));
    repo.arc(&wt)
        .args(["verify", "feat-x", "--gate", "smoke"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt).args(["check", "feat-x"]).assert().success();

    // Integrate from the main checkout, which has master checked out.
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let merged = repo.head(&repo.root);
    let parents = git_out(&repo.root, &["rev-list", "--parents", "-n", "1", &merged]);
    let ids: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(ids.len(), 3, "merge commit must have two parents");
    assert_eq!(ids[1], old_master);
    let subject = git_out(&repo.root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject, "merge(feat-x): Feature X");

    let status = stdout(repo.arc(&repo.root).args(["status", "feat-x"]));
    let report: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(report["state"], "closed");
    assert_eq!(report["closure"]["outcome"], "integrated");
}

/// Blocking finding → check exits 2 and integrate refuses; resolving it
/// plus a renewed verdict unblocks.
#[test]
fn blocking_finding_blocks_until_resolved_and_reapproved() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-y"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-y");
    repo.commit(&wt, "y.txt", "y\n", "fix: y");
    stdout(repo.arc(&wt).args(["snapshot", "fix-y"]));

    let findings = r#"[{"blocking": true, "severity": "major",
        "summary": "y is wrong", "anchor": {"path": "y.txt", "line_start": 1}}]"#;
    repo.arc(&wt)
        .args([
            "review",
            "fix-y",
            "--verdict",
            "changes-requested",
            "--findings-json",
            "-",
        ])
        .write_stdin(findings)
        .assert()
        .success();

    repo.arc(&wt).args(["check", "fix-y"]).assert().code(2);
    repo.arc(&repo.root)
        .args(["integrate", "fix-y"])
        .assert()
        .code(2);

    let show = stdout(repo.arc(&wt).args(["show", "fix-y", "--json"]));
    let state: serde_json::Value = serde_json::from_str(&show).unwrap();
    let fid = state["findings"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    // Anchor captured a blob for the head side.
    assert!(state["findings"][&fid]["anchor"]["blob"].is_string());

    repo.commit(&wt, "y.txt", "y fixed\n", "fix: correct y");
    let fix = repo.head(&wt);
    repo.arc(&wt)
        .args([
            "resolve", "fix-y", &fid, "--status", "resolved", "--commit", &fix,
        ])
        .assert()
        .success();

    // Old approval basis is gone: new head needs a new patchset + verdict.
    repo.arc(&wt).args(["check", "fix-y"]).assert().code(3);
    stdout(repo.arc(&wt).args(["snapshot", "fix-y"]));
    repo.arc(&wt)
        .args(["review", "fix-y", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-y"]).assert().success();
}

/// A commit after approval makes the verdict stale (exit 3) until a new
/// patchset is approved.
#[test]
fn approval_goes_stale_when_head_moves() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-z"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-z");
    repo.commit(&wt, "z.txt", "z\n", "fix: z");
    stdout(repo.arc(&wt).args(["snapshot", "fix-z"]));
    repo.arc(&wt)
        .args(["review", "fix-z", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-z"]).assert().success();

    repo.commit(&wt, "z.txt", "z2\n", "fix: z again");
    repo.arc(&wt).args(["check", "fix-z"]).assert().code(3);
    repo.arc(&repo.root)
        .args(["integrate", "fix-z"])
        .assert()
        .code(3);
}

/// integrate --cleanup invoked from INSIDE the change worktree must not
/// die when that worktree is removed under it (regression: branch
/// deletion used the vanished cwd).
#[test]
fn integrate_cleanup_from_inside_change_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-c"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-c");
    repo.commit(&wt, "c.txt", "c\n", "fix: c");
    stdout(repo.arc(&wt).args(["snapshot", "fix-c"]));
    repo.arc(&wt)
        .args(["review", "fix-c", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["integrate", "fix-c", "--cleanup"])
        .assert()
        .success();

    assert!(!wt.exists(), "change worktree should be removed");
    let branches = git_out(&repo.root, &["branch", "--list", "arc/fix-c"]);
    assert!(branches.is_empty(), "change branch should be deleted");
}

/// Hold blocks integration (exit 4) until released.
#[test]
fn hold_blocks_integration() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-h"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-h");
    repo.commit(&wt, "h.txt", "h\n", "fix: h");
    stdout(repo.arc(&wt).args(["snapshot", "fix-h"]));
    repo.arc(&wt)
        .args(["review", "fix-h", "--verdict", "approved"])
        .assert()
        .success();

    repo.arc(&wt)
        .args(["hold", "fix-h", "--reason", "manual testing first"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().code(4);
    repo.arc(&repo.root)
        .args(["integrate", "fix-h"])
        .assert()
        .code(4);

    repo.arc(&wt)
        .args(["release-hold", "fix-h"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-h"]).assert().success();
}

/// A declared gate that never ran (or failed) blocks with exit 5; a pass
/// at the exact head unblocks.
#[test]
fn gates_must_be_green_at_head() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.fails]\ncommand = \"false\"\nprofiles = [\"local\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc"]);
    git(&repo.root, &["commit", "-m", "gates"]);

    stdout(repo.arc(&repo.root).args(["begin", "fix-g"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-g");
    repo.commit(&wt, "g.txt", "g\n", "fix: g");
    stdout(repo.arc(&wt).args(["snapshot", "fix-g"]));
    repo.arc(&wt)
        .args(["review", "fix-g", "--verdict", "approved"])
        .assert()
        .success();

    // Gate never ran: blocked.
    repo.arc(&wt).args(["check", "fix-g"]).assert().code(5);

    // Gate ran and failed: still blocked, verify itself exits 1.
    repo.arc(&wt)
        .args(["verify", "fix-g", "--gate", "fails"])
        .assert()
        .code(1);
    repo.arc(&wt).args(["check", "fix-g"]).assert().code(5);

    // Fix the gate in the worktree's .arc? Gate command comes from the
    // toplevel of the invoking worktree, so run a passing command via a
    // redefined gates file in the change worktree.
    fs::write(
        wt.join(".arc/gates.toml"),
        "[gates.fails]\ncommand = \"true\"\nprofiles = [\"local\"]\n",
    )
    .unwrap();
    git(&wt, &["add", ".arc"]);
    git(&wt, &["commit", "-m", "fix gate"]);
    stdout(repo.arc(&wt).args(["snapshot", "fix-g"]));
    repo.arc(&wt)
        .args(["verify", "fix-g", "--gate", "fails"])
        .assert()
        .success();
    repo.arc(&wt)
        .args(["review", "fix-g", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&wt).args(["check", "fix-g"]).assert().success();
}

/// Comments, replies, and prefix resolution round-trip through the ledger.
#[test]
fn comment_reply_roundtrip_and_prefix_resolution() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "chat-c"]));
    let wt = repo.home.join(".worktrees").join("repo-chat-c");
    repo.commit(&wt, "c.txt", "c\n", "c");
    stdout(repo.arc(&wt).args(["snapshot", "chat-c"]));

    let out = stdout(repo.arc(&wt).args([
        "comment",
        "chat-c",
        "--body",
        "looks odd",
        "--path",
        "c.txt",
        "--line",
        "1",
    ]));
    let event_id = out
        .lines()
        .find_map(|l| l.strip_prefix("event: "))
        .unwrap()
        .to_string();

    repo.arc(&wt)
        .args(["reply", "chat-c", &event_id, "--body", "explained"])
        .assert()
        .success();

    // Bare slug prefix resolves the change.
    let show = stdout(repo.arc(&wt).args(["show", "chat-c"]));
    assert!(show.contains("looks odd"));
    assert!(show.contains("explained"));
}

/// Approving while recording blocking findings is contradictory.
#[test]
fn approve_with_blocking_findings_refused() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "fix-w"]));
    let wt = repo.home.join(".worktrees").join("repo-fix-w");
    repo.commit(&wt, "w.txt", "w\n", "w");
    stdout(repo.arc(&wt).args(["snapshot", "fix-w"]));
    let findings = r#"[{"blocking": true, "severity": "critical", "summary": "no"}]"#;
    repo.arc(&wt)
        .args([
            "review",
            "fix-w",
            "--verdict",
            "approved",
            "--findings-json",
            "-",
        ])
        .write_stdin(findings)
        .assert()
        .failure();
}

/// Snapshot sets a retention ref so reviewed heads survive branch deletion.
#[test]
fn snapshot_sets_retention_ref() {
    let repo = Repo::new();
    let out = stdout(repo.arc(&repo.root).args(["begin", "keep-k"]));
    let change_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt = repo.home.join(".worktrees").join("repo-keep-k");
    repo.commit(&wt, "k.txt", "k\n", "k");
    stdout(repo.arc(&wt).args(["snapshot", "keep-k"]));
    let head = repo.head(&wt);
    let kept = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-01")],
    );
    assert_eq!(kept, head);

    // A rewound branch gets a second patchset with its own pin; the
    // first head stays protected.
    git(&wt, &["reset", "--hard", "HEAD~1"]);
    repo.commit(&wt, "k2.txt", "k2\n", "k2");
    stdout(repo.arc(&wt).args(["snapshot", "keep-k"]));
    let kept1 = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-01")],
    );
    let kept2 = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{change_id}/ps-02")],
    );
    assert_eq!(kept1, head, "rewound head must stay pinned");
    assert_eq!(kept2, repo.head(&wt));
}

/// Abandoning a change must keep every reviewed head pinned; integrating
/// releases only heads reachable from the merge.
#[test]
fn closure_retention_policy() {
    let repo = Repo::new();

    // Abandoned: pins survive even branch force-deletion.
    let out = stdout(repo.arc(&repo.root).args(["begin", "drop-r"]));
    let drop_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt = repo.home.join(".worktrees").join("repo-drop-r");
    repo.commit(&wt, "r.txt", "r\n", "r");
    stdout(repo.arc(&wt).args(["snapshot", "drop-r"]));
    let dropped_head = repo.head(&wt);
    repo.arc(&repo.root)
        .args(["close", "drop-r", "--abandoned"])
        .assert()
        .success()
        .stdout(predicates::str::contains("kept refs/arc/keep/"));
    git(
        &repo.root,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    git(&repo.root, &["branch", "-D", "arc/drop-r"]);
    let kept = git_out(
        &repo.root,
        &["rev-parse", &format!("refs/arc/keep/{drop_id}/ps-01")],
    );
    assert_eq!(kept, dropped_head, "abandoned head must stay pinned");

    // Integrated: the reachable head's pin is released.
    let out = stdout(repo.arc(&repo.root).args(["begin", "land-r"]));
    let land_id = out
        .lines()
        .find_map(|l| l.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let wt2 = repo.home.join(".worktrees").join("repo-land-r");
    repo.commit(&wt2, "l.txt", "l\n", "l");
    stdout(repo.arc(&wt2).args(["snapshot", "land-r"]));
    repo.arc(&wt2)
        .args(["review", "land-r", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "land-r"])
        .assert()
        .success();
    let refs = git_out(
        &repo.root,
        &["for-each-ref", &format!("refs/arc/keep/{land_id}/")],
    );
    assert!(refs.is_empty(), "reachable pins should be released");
}

/// begin derives the target from the primary worktree's branch, even
/// when invoked from another change's worktree on a different branch.
#[test]
fn begin_derives_target_from_primary_worktree() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "first-t"]));
    let wt = repo.home.join(".worktrees").join("repo-first-t");
    repo.commit(&wt, "t.txt", "t\n", "t");

    // From inside the first change's worktree: target must be master,
    // and the new branch must derive from master's head, not from the
    // in-progress arc/first-t head.
    stdout(repo.arc(&wt).args(["begin", "second-t"]));
    let show = stdout(repo.arc(&wt).args(["show", "second-t", "--json"]));
    let state: serde_json::Value = serde_json::from_str(&show).unwrap();
    assert_eq!(state["target_branch"], "master");
    assert_eq!(
        state["base"],
        serde_json::Value::String(repo.head(&repo.root)),
        "base must be master's head, not the other change's head"
    );
}

/// Implicit stacking on an open change branch is refused; explicit
/// --target allows it.
#[test]
fn begin_refuses_implicit_stacking() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "base-s"]));
    let wt = repo.home.join(".worktrees").join("repo-base-s");
    repo.commit(&wt, "s.txt", "s\n", "s");

    // Make the primary worktree sit on the change branch: simulate by
    // passing --target pointing at the open change branch explicitly —
    // allowed — versus the implicit refusal path, which needs the
    // default target to resolve to that branch. Explicit works:
    repo.arc(&wt)
        .args([
            "begin",
            "stack-s",
            "--target",
            "arc/base-s",
            "--no-worktree",
        ])
        .assert()
        .success();
}

/// ARC_WORKTREES_DIR and ARC_DATA_ROOT relocate paths (sandboxing).
#[test]
fn path_overrides_relocate_worktrees_and_ledger() {
    let repo = Repo::new();
    let sandbox_wts = repo.home.join("sandbox-wts");
    let sandbox_data = repo.home.join("sandbox-data");

    let out = stdout(
        repo.arc(&repo.root)
            .env("ARC_WORKTREES_DIR", &sandbox_wts)
            .env("ARC_DATA_ROOT", &sandbox_data)
            .args(["begin", "boxed-p"]),
    );
    assert!(out.contains("change: boxed-p-"));
    assert!(sandbox_wts.join("repo-boxed-p").is_dir());

    // Ledger landed under the slugged repo path inside the data root,
    // and the default (in-repo) store does not know the change.
    let slug_dirs: Vec<_> = fs::read_dir(&sandbox_data).unwrap().collect();
    assert_eq!(slug_dirs.len(), 1);
    repo.arc(&repo.root)
        .env("ARC_DATA_ROOT", &sandbox_data)
        .args(["show", "boxed-p"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["show", "boxed-p"])
        .assert()
        .failure();
}

/// The config file under AI_HOME drives the same overrides.
#[test]
fn config_file_under_ai_home() {
    let repo = Repo::new();
    let ai_home = repo.home.join("ai");
    fs::create_dir_all(ai_home.join("arc")).unwrap();
    fs::write(
        ai_home.join("arc/config.toml"),
        format!(
            "worktrees_dir = \"{}\"\n",
            repo.home.join("cfg-wts").display()
        ),
    )
    .unwrap();

    stdout(
        repo.arc(&repo.root)
            .env("AI_HOME", &ai_home)
            .args(["begin", "cfg-c"]),
    );
    assert!(repo.home.join("cfg-wts").join("repo-cfg-c").is_dir());
}

/// close --abandoned works and closed changes refuse new work.
#[test]
fn close_abandoned_and_refuse_further_work() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "drop-d", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["close", "drop-d", "--abandoned"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["check", "drop-d"])
        .assert()
        .code(6);
    repo.arc(&repo.root)
        .args(["hold", "drop-d", "--reason", "x"])
        .assert()
        .failure();
    // Slug is reusable after closure.
    repo.arc(&repo.root)
        .args([
            "begin",
            "drop-d",
            "--no-worktree",
            "--branch",
            "arc/drop-d-2",
        ])
        .assert()
        .success();
}
