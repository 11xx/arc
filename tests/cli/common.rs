pub(crate) use assert_cmd::Command as AssertCommand;
pub(crate) use predicates::prelude::PredicateBooleanExt;
pub(crate) use sha2::{Digest, Sha256};
pub(crate) use std::fs;
pub(crate) use std::io::{BufRead, BufReader, Read};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
pub(crate) use std::thread;
pub(crate) use std::time::{Duration, Instant};
pub(crate) use tempfile::TempDir;

/// The executable every fixture launches, selected by the test runner or Cargo.
pub(crate) fn arc_binary() -> &'static str {
    static BINARY: OnceLock<String> = OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::var_os("ARC_TEST_BIN")
            .map(|value| value.into_string().expect("ARC_TEST_BIN is UTF-8"))
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_arc").to_owned())
    })
}

pub(crate) struct Repo {
    _tmp: TempDir,
    pub(crate) root: PathBuf,
    pub(crate) home: PathBuf,
}

impl Repo {
    pub(crate) fn new() -> Repo {
        Self::with_object_format("sha1")
    }

    /// A repository holding one commit on `master` that adds README.md,
    /// with the fixture identity in its local config, copied from the seed
    /// for `format`. Each fixture owns every file it holds, so nothing one
    /// fixture writes reaches the seed or another fixture.
    pub(crate) fn with_object_format(format: &str) -> Repo {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        seed(format).copy_to(&root);
        fs::create_dir_all(&home).unwrap();
        let config = home.join(".local/ai/arc");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("config.toml"),
            "[change]\ncheckout='worktree'\n",
        )
        .unwrap();
        Repo {
            _tmp: tmp,
            root,
            home,
        }
    }

    /// Where this fixture's arc writes: its ledger under the repository, and
    /// every other root under `home`. One prefix declares all of them, which
    /// is the same mechanism a caller sandboxing a real project gets.
    ///
    /// `HOME` is set as well because the harness running this suite may export
    /// a session store under its own home, and identity detection reads it.
    pub(crate) fn arc(&self, cwd: &Path) -> AssertCommand {
        self.fixture_env(AssertCommand::new(arc_binary()), cwd)
    }

    /// `arc`, run under an explicit umask, so a test about the permissions
    /// arc gives a file does not depend on the umask the suite inherited.
    pub(crate) fn arc_under_umask(&self, cwd: &Path, umask: &str) -> AssertCommand {
        let mut sh = AssertCommand::new("sh");
        sh.args([
            "-c",
            &format!("umask {umask} && exec \"$0\" \"$@\""),
            arc_binary(),
        ]);
        self.fixture_env(sh, cwd)
    }

    /// A shell command line in which `arc` is the binary under test, so a
    /// test can run a command exactly as arc printed it, quoting included.
    pub(crate) fn arc_shell(&self, cwd: &Path, line: &str) -> AssertCommand {
        let mut sh = AssertCommand::new("sh");
        sh.args([
            "-c",
            &format!("arc() {{ \"$ARC_UNDER_TEST\" \"$@\"; }}\n{line}"),
        ])
        .env("ARC_UNDER_TEST", arc_binary());
        self.fixture_env(sh, cwd)
    }

    pub(crate) fn fixture_env(&self, mut cmd: AssertCommand, cwd: &Path) -> AssertCommand {
        cmd.current_dir(cwd);
        let (sets, removes) = self.fixture_vars();
        cmd.envs(sets);
        for name in removes {
            cmd.env_remove(name);
        }
        cmd
    }

    /// `arc` as a child process under the fixture's environment, for a test
    /// that talks to a command while it runs. Every stream is piped.
    pub(crate) fn spawn_arc_piped(&self, cwd: &Path, args: &[&str]) -> Child {
        self.spawn_arc_piped_with_env(cwd, args, &[])
    }

    pub(crate) fn spawn_arc_piped_with_env(
        &self,
        cwd: &Path,
        args: &[&str],
        extra: &[(&str, &Path)],
    ) -> Child {
        let mut cmd = Command::new(arc_binary());
        cmd.current_dir(cwd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (sets, removes) = self.fixture_vars();
        cmd.envs(sets);
        for name in removes {
            cmd.env_remove(name);
        }
        cmd.envs(extra.iter().copied());
        cmd.spawn().unwrap()
    }

    /// What every fixture command's environment sets, and what it removes.
    fn fixture_vars(
        &self,
    ) -> (
        Vec<(&'static str, std::ffi::OsString)>,
        &'static [&'static str],
    ) {
        let sets = vec![
            ("HOME", self.home.as_os_str().to_os_string()),
            ("ARC_SANDBOX", self.home.as_os_str().to_os_string()),
            (
                "GIT_CONFIG_GLOBAL",
                self.home.join(".gitconfig").as_os_str().to_os_string(),
            ),
            (
                "XDG_CONFIG_HOME",
                self.home.join(".config").as_os_str().to_os_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            (
                "GIT_CEILING_DIRECTORIES",
                self._tmp.path().as_os_str().to_os_string(),
            ),
            ("ARC_ACTOR", "tester".into()),
            ("ARC_TEST_FIXTURE_IDENTITY", "1".into()),
            ("ARC_HARNESS", "test".into()),
            ("ARC_SESSION", "session-a".into()),
            (
                "ARC_AGENT_STATUS",
                self.home
                    .join(".no-agent-status")
                    .as_os_str()
                    .to_os_string(),
            ),
            (NO_EDITOR[0].0, NO_EDITOR[0].1.into()),
            (NO_EDITOR[1].0, NO_EDITOR[1].1.into()),
        ];
        const REMOVES: &[&str] = &[
            "ARC_ROLE",
            "ARC_MODEL",
            "ARC_SESSION_LINK",
            "ARC_ON_BEHALF_OF",
            "ARC_DATA_DIR",
            "ARC_DATA_ROOT",
            "ARC_WORKTREES_DIR",
            "ARC_AI_HOME",
            "ARC_JOURNAL_DIR",
            "AI_HOME",
            // The suite may itself run inside a harness. Whatever session
            // variable that harness exports must not reach the binary under
            // test, or `env` detects the runner instead of the fixture.
            "CLAUDE_SESSION_ID",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_BRIDGE_SESSION_ID",
            "CODEX_THREAD_ID",
            "OPENCODE_SESSION",
            "OPENCODE_TERMINAL",
            "PI_SESSION_ID",
            "PI_SESSION_FILE",
            "PI_MODEL",
            "PI_REASONING_LEVEL",
            // Session lookup honours each store override before `$HOME`, so a
            // host that exports one would otherwise have the fixture read its
            // real store. A test that needs one sets it explicitly.
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "PI_CODING_AGENT_SESSION_DIR",
            "PI_CODING_AGENT_DIR",
            // A price table the host names would price the fixture's figures.
            "TAPES_PRICES",
        ];
        (sets, REMOVES)
    }

    pub(crate) fn commit(&self, cwd: &Path, file: &str, content: &str, msg: &str) {
        fs::write(cwd.join(file), content).unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", msg]);
    }

    pub(crate) fn head(&self, cwd: &Path) -> String {
        git_out(cwd, &["rev-parse", "HEAD"])
    }

    /// Declare gates in the operator policy, which lives outside every tree:
    /// redeclaring one moves no change's tree, so recorded evidence keeps the
    /// tree it was recorded against.
    pub(crate) fn declare_gates_locally(&self, text: &str) {
        let dir = self.root.join(".git/arc");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("operator-policy.toml"), text).unwrap();
    }

    /// Commit `text` as `.arc/gates.toml` on master, the branch every change
    /// here merges into. Integration reads declarations from that branch, so
    /// this is how a fixture declares or redeclares a gate for changes already
    /// open, whichever branch the primary checkout stands on.
    pub(crate) fn redeclare_gates(&self, text: &str) {
        let on_master = git_out(&self.root, &["branch", "--show-current"]) == "master";
        let checkout = if on_master {
            self.root.clone()
        } else {
            let side = self.home.join("master-checkout");
            git(
                &self.root,
                &["worktree", "add", "-q", side.to_str().unwrap(), "master"],
            );
            side
        };
        fs::create_dir_all(checkout.join(".arc")).unwrap();
        fs::write(checkout.join(".arc/gates.toml"), text).unwrap();
        git(&checkout, &["add", ".arc/gates.toml"]);
        git(&checkout, &["commit", "-q", "-m", "test: declare gates"]);
        if !on_master {
            git(
                &self.root,
                &["worktree", "remove", "--force", checkout.to_str().unwrap()],
            );
        }
    }
}

/// What every fixture of one object format starts from, built once per test
/// process from Git's own commands. It holds no ledger and is never handed to
/// a test: fixtures copy it.
pub(crate) struct Seed {
    pub(crate) root: PathBuf,
    /// Git commands that building this seed ran.
    pub(crate) initializer_spawns: usize,
    hash_len: usize,
}

pub(crate) fn seed(format: &str) -> &'static Seed {
    static SHA1: OnceLock<Seed> = OnceLock::new();
    static SHA256: OnceLock<Seed> = OnceLock::new();
    match format {
        "sha1" => SHA1.get_or_init(|| Seed::build("sha1", 20)),
        "sha256" => SHA256.get_or_init(|| Seed::build("sha256", 32)),
        other => panic!("no fixture seed for object format {other:?}"),
    }
}

impl Seed {
    fn build(format: &str, hash_len: usize) -> Seed {
        let root = seed_scratch().join(format);
        fs::create_dir_all(&root).unwrap();
        let initializer_spawns = git_spawns_during(|| {
            git(
                &root,
                &[
                    "init",
                    "--template=",
                    "-b",
                    "master",
                    &format!("--object-format={format}"),
                ],
            );
            fs::create_dir_all(root.join(".git/hooks")).unwrap();
            fs::create_dir_all(root.join(".git/info")).unwrap();
            fs::write(root.join(".git/info/exclude"), b"").unwrap();
            git(&root, &["config", "user.name", "Tester"]);
            git(&root, &["config", "user.email", "tester@example.invalid"]);
            git(&root, &["config", "commit.gpgsign", "false"]);
            {
                use std::io::Write;
                fs::OpenOptions::new()
                    .append(true)
                    .open(root.join(".git/config"))
                    .unwrap()
                    .write_all(b"[maintenance]\n\tauto = false\n")
                    .unwrap();
            }
            fs::write(root.join("README.md"), "hello\n").unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-m", "init"]);
        })
        .0;
        Seed {
            root,
            initializer_spawns,
            hash_len,
        }
    }

    fn copy_to(&self, destination: &Path) {
        copy_tree(&self.root, destination);
        self.restat_index(destination);
    }

    /// Point the copied index's one entry, README.md, at the copy's stat data
    /// and reseal it, as `git update-index --refresh` would. Without this,
    /// plumbing that trusts stat data reports README.md modified.
    fn restat_index(&self, root: &Path) {
        use std::os::unix::fs::MetadataExt;
        let index_path = root.join(".git/index");
        let mut index = fs::read(&index_path).unwrap();
        let readme = fs::metadata(root.join("README.md")).unwrap();
        // The index header is 12 bytes; the first entry opens with its ctime,
        // mtime, device and inode, each a big-endian 32-bit field.
        let stat = [
            readme.ctime(),
            readme.ctime_nsec(),
            readme.mtime(),
            readme.mtime_nsec(),
            readme.dev() as i64,
            readme.ino() as i64,
        ];
        for (field, value) in index[12..36].chunks_exact_mut(4).zip(stat) {
            field.copy_from_slice(&(value as u32).to_be_bytes());
        }
        let body = index.len() - self.hash_len;
        let seal = match self.hash_len {
            20 => sha1::Sha1::digest(&index[..body]).to_vec(),
            _ => Sha256::digest(&index[..body]).to_vec(),
        };
        index[body..].copy_from_slice(&seal);
        fs::write(index_path, index).unwrap();
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// This process's seed directory, beside the fixtures in the system temporary
/// directory. A seed is a Git repository, and one left inside the checkout's
/// build directory is local content that keeps `arc integrate --cleanup` from
/// removing the checkout. Statics are never dropped, so each process removes
/// the directories its exited predecessors left.
fn seed_scratch() -> &'static Path {
    static SCRATCH: OnceLock<PathBuf> = OnceLock::new();
    SCRATCH.get_or_init(|| {
        // SAFETY: getuid has no preconditions and cannot fail.
        let user = unsafe { libc::getuid() };
        let parent = std::env::temp_dir().join(format!("arc-fixture-seeds-{user}"));
        fs::create_dir_all(&parent).unwrap();
        for entry in fs::read_dir(&parent).unwrap().flatten() {
            let owner = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok());
            if owner.is_some_and(|pid| !process_exists(pid)) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        let own = parent.join(std::process::id().to_string());
        let _ = fs::remove_dir_all(&own);
        fs::create_dir_all(&own).unwrap();
        own
    })
}

fn process_exists(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks whether `pid` exists.
    let signalled = unsafe { libc::kill(pid, 0) } == 0;
    signalled || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

thread_local! {
    static GIT_SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_git_spawn() {
    GIT_SPAWNS.with(|spawns| spawns.set(spawns.get() + 1));
}

/// `f`'s result and how many Git commands this suite's helpers ran on this
/// thread while it ran.
pub(crate) fn git_spawns_during<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = GIT_SPAWNS.with(|spawns| spawns.get());
    let value = f();
    (GIT_SPAWNS.with(|spawns| spawns.get()) - before, value)
}

/// Turn on the opt-in ambient identity detection this suite's harness
/// environment otherwise never reaches.
pub(crate) fn enable_identity_detection(repo: &Repo) {
    let config_dir = repo.home.join(".local/ai/arc");
    fs::create_dir_all(&config_dir).unwrap();
    let path = config_dir.join("config.toml");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    fs::write(path, format!("{existing}[identity]\ndetect = true\n")).unwrap();
}

pub(crate) fn codex_recording(repo: &Repo, session: &str, model: &str) -> PathBuf {
    let day = repo.home.join("codex-state/sessions/2026/07/24");
    fs::create_dir_all(&day).unwrap();
    let path = day.join(format!("rollout-2026-07-24T00-00-00-{session}.jsonl"));
    let rows = [
        serde_json::json!({"type":"session_meta","timestamp":"2026-07-24T00:00:00Z","payload":{"id":session}}),
        serde_json::json!({"type":"turn_context","timestamp":"2026-07-24T00:00:01Z","payload":{"model":model,"effort":"high"}}),
        serde_json::json!({"type":"response_item","timestamp":"2026-07-24T00:00:02Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"private full-id prompt"}]}}),
        serde_json::json!({"type":"response_item","timestamp":"2026-07-24T00:00:03Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"full-id answer"}]}}),
    ];
    fs::write(&path, rows.map(|row| row.to_string() + "\n").concat()).unwrap();
    path
}

/// A Pi recording in the fixture's store: the header, the thinking level in
/// effect, and one assistant turn naming the model. Returns the file, so a
/// caller can also name it as the live recording.
pub(crate) fn pi_recording(repo: &Repo, session: &str, model: &str, level: &str) -> PathBuf {
    let sessions = repo.home.join("pi-sessions/project");
    fs::create_dir_all(&sessions).unwrap();
    let path = sessions.join(format!("2026-07-18T12-07-52Z_{session}.jsonl"));
    fs::write(
        &path,
        format!(
            concat!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{session}\",",
                "\"timestamp\":\"2026-07-18T12:07:52Z\",\"cwd\":\"/fixture\"}}\n",
                "{{\"type\":\"thinking_level_change\",\"id\":\"thinking-1\",\"parentId\":null,",
                "\"timestamp\":\"2026-07-18T12:07:53Z\",\"thinkingLevel\":\"{level}\"}}\n",
                "{{\"type\":\"message\",\"id\":\"assistant-1\",\"parentId\":\"thinking-1\",",
                "\"timestamp\":\"2026-07-18T12:07:54Z\",\"message\":{{\"role\":\"assistant\",",
                "\"timestamp\":1767261604000,\"provider\":\"openai-codex\",\"model\":\"{model}\",",
                "\"content\":[{{\"type\":\"text\",\"text\":\"The recording's answer.\"}}]}}}}\n",
            ),
            session = session,
            model = model,
            level = level,
        ),
    )
    .unwrap();
    path
}

/// Git's editor, for every command this suite runs and every command the
/// binary under test runs beneath it. A fixture that reaches the operator's
/// editor stops the whole suite on a modal window it cannot answer, and
/// `GIT_EDITOR` in the environment outranks the `core.editor` a single
/// command sets for itself, so the environment is where this has to be said.
pub(crate) const NO_EDITOR: [(&str, &str); 2] =
    [("GIT_EDITOR", "true"), ("GIT_SEQUENCE_EDITOR", "true")];

/// Deliver the objects reachable from every source ref, including Arc's
/// evidence pins, under a namespace separate from the receiver's branches.
pub(crate) fn deliver_git_objects(source: &Repo, destination: &Repo) {
    git(
        &destination.root,
        &[
            "fetch",
            "--no-tags",
            source.root.to_str().unwrap(),
            "+refs/*:refs/delivered/*",
        ],
    );
}

pub(crate) fn git(cwd: &Path, args: &[&str]) {
    count_git_spawn();
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .envs(NO_EDITOR)
        .env("GIT_CONFIG_GLOBAL", cwd.join(".git/fixture-global-config"))
        .env("XDG_CONFIG_HOME", cwd.join(".git/fixture-config-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&st.stderr)
    );
    if args == ["config", "user.email", "tester@example.invalid"] && cwd.join(".git").is_dir() {
        fs::write(cwd.join(".git/arc-test-fixture"), "").unwrap();
    }
}

pub(crate) fn git_out(cwd: &Path, args: &[&str]) -> String {
    count_git_spawn();
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .envs(NO_EDITOR)
        .env("GIT_CONFIG_GLOBAL", cwd.join(".git/fixture-global-config"))
        .env("XDG_CONFIG_HOME", cwd.join(".git/fixture-config-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A PATH whose `git` refuses `replay`, as a Git that predates it does.
pub(crate) fn path_without_git_replay(repo: &Repo) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
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
            "#!/bin/sh\nif [ \"$1\" = replay ]; then\n  echo 'git: replay is not a git command' >&2\n  exit 1\nfi\nexec \"{}\" \"$@\"\n",
            real_git.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap()
}

/// Stdout of a command that must exit zero. Any other exit fails the test at
/// the caller, with the command, its status, and its stderr.
#[track_caller]
pub(crate) fn stdout(cmd: &mut AssertCommand) -> String {
    let out = cmd.output().unwrap();
    let program = Path::new(cmd.get_program()).file_name().unwrap();
    let args: Vec<_> = cmd.get_args().collect();
    assert!(
        out.status.success(),
        "{program:?} {args:?} exited {}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Stdout of a command whatever its exit status, for a call whose non-zero
/// exit is part of what the test observes.
pub(crate) fn stdout_any_status(cmd: &mut AssertCommand) -> String {
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub(crate) fn change_with_patchset(repo: &Repo, slug: &str) -> (String, PathBuf, String) {
    let out = stdout(repo.arc(&repo.root).args(["begin", slug]));
    let change_id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string();
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("test: add {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    let head = repo.head(&worktree);
    (change_id, worktree, head)
}

pub(crate) fn complete_change(repo: &Repo, slug: &str) {
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        &format!("{slug}\n"),
        &format!("feat: complete {slug}"),
    );
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&worktree)
        .args(["review", slug, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", slug])
        .assert()
        .success();
}

pub(crate) fn json_file_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

pub(crate) fn spawn_arc(repo: &Repo, cwd: &Path, args: &[&str]) -> Child {
    spawn_arc_with_session(repo, cwd, args, "session-a")
}

pub(crate) fn spawn_arc_with_session(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    session: &str,
) -> Child {
    arc_child(repo, cwd, args, session, Stdio::null())
}

/// `spawn_arc_with_session` with stderr kept for `child_stderr`, for a child
/// whose failure the test reports. The stderr pipe is read only after exit,
/// so the child must write less than a pipe buffer to it.
pub(crate) fn spawn_arc_keeping_stderr(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    session: &str,
) -> Child {
    arc_child(repo, cwd, args, session, Stdio::piped())
}

/// `spawn_arc` with extra environment, for a test-only hook the child reads.
pub(crate) fn spawn_arc_with_env(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &Path)],
) -> Child {
    arc_command(repo, cwd, args, "session-a", Stdio::null())
        .envs(envs.iter().copied())
        .spawn()
        .unwrap()
}

/// `spawn_arc_keeping_stderr` with extra environment.
pub(crate) fn spawn_arc_keeping_stderr_with_env(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Child {
    arc_command(repo, cwd, args, "session-a", Stdio::piped())
        .envs(envs.iter().copied())
        .spawn()
        .unwrap()
}

pub(crate) fn spawn_arc_without_actor(repo: &Repo, cwd: &Path, args: &[&str]) -> Child {
    arc_command(repo, cwd, args, "session-a", Stdio::piped())
        .env_remove("ARC_ACTOR")
        .spawn()
        .unwrap()
}

fn arc_child(repo: &Repo, cwd: &Path, args: &[&str], session: &str, stderr: Stdio) -> Child {
    arc_command(repo, cwd, args, session, stderr)
        .spawn()
        .unwrap()
}

fn arc_command(repo: &Repo, cwd: &Path, args: &[&str], session: &str, stderr: Stdio) -> Command {
    let binary = std::ffi::OsString::from(arc_binary());
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .env("HOME", &repo.home)
        .env("ARC_SANDBOX", &repo.home)
        .env("ARC_ACTOR", "tester")
        .env("ARC_TEST_FIXTURE_IDENTITY", "1")
        .env("ARC_HARNESS", "test")
        .env("ARC_SESSION", session)
        .envs(NO_EDITOR)
        .env_remove("ARC_ROLE")
        .env_remove("ARC_DATA_DIR")
        .env_remove("ARC_DATA_ROOT")
        .env_remove("ARC_WORKTREES_DIR")
        .env_remove("ARC_AI_HOME")
        .env_remove("ARC_JOURNAL_DIR")
        .env_remove("AI_HOME")
        .stdout(Stdio::piped())
        .stderr(stderr);
    command
}

pub(crate) fn refresh_bundle_checksum(bundle: &mut serde_json::Value) {
    let mut digest = Sha256::new();
    for event in bundle["events"].as_array().unwrap() {
        digest.update(serde_json::to_vec(event).unwrap());
        digest.update(b"\n");
    }
    bundle["events_sha256"] = serde_json::Value::String(hex::encode(digest.finalize()));
}

pub(crate) fn opened_change_id(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("begin output should contain a change id")
        .to_string()
}

pub(crate) fn event_dir(repo: &Repo, change_id: &str) -> PathBuf {
    repo.root
        .join(".git/arc/changes")
        .join(change_id)
        .join("events")
}

pub(crate) fn event_count(repo: &Repo, change_id: &str) -> usize {
    fs::read_dir(event_dir(repo, change_id)).unwrap().count()
}

pub(crate) fn rewrite_event(
    repo: &Repo,
    change_id: &str,
    event_type: &str,
    mut update: impl FnMut(&mut serde_json::Value),
) {
    let mut paths = fs::read_dir(event_dir(repo, change_id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let path = paths
        .into_iter()
        .rev()
        .find(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["event_type"] == event_type
        })
        .expect("event type should exist");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    update(&mut value);
    fs::write(path, json_file_bytes(&value)).unwrap();
}

pub(crate) fn age_event(repo: &Repo, change_id: &str, event_type: &str, seconds: i64) {
    rewrite_event(repo, change_id, event_type, |event| {
        event["created_at"] = serde_json::Value::String(
            (chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339(),
        );
    });
}

pub(crate) fn hold_transition_lock(repo: &Repo, change_id: &str) -> fs::File {
    hold_named_lock(repo, &format!("{change_id}.lock"))
}

pub(crate) fn hold_graph_lock(repo: &Repo) -> fs::File {
    hold_named_lock(repo, "graph.lock")
}

fn target_lock_stem(target: &str) -> String {
    format!("target-{}", hex::encode(Sha256::digest(target.as_bytes())))
}

pub(crate) fn hold_target_lock(repo: &Repo, target: &str) -> fs::File {
    hold_named_lock(repo, &format!("{}.lock", target_lock_stem(target)))
}

/// An arc paused where it first finds `target`'s lock held, so everything it
/// reads before taking the lock has been read. `release_lock_wait` resumes it.
#[cfg(debug_assertions)]
pub(crate) fn spawn_arc_paused_on_target_lock(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    target: &str,
) -> Child {
    spawn_arc_paused_on_lock(repo, cwd, args, &target_lock_stem(target), &[])
}

/// An arc paused where it first finds the lock file `<stem>.lock` held, with
/// extra environment. `release_lock_wait` resumes it.
#[cfg(debug_assertions)]
pub(crate) fn spawn_arc_paused_on_lock(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    stem: &str,
    envs: &[(&str, &str)],
) -> Child {
    spawn_arc_at_rendezvous(repo, cwd, args, &format!("{stem}-contended"), envs)
}

/// An arc paused at the test rendezvous `phase`, with extra environment.
/// `release_lock_wait` resumes it.
#[cfg(debug_assertions)]
pub(crate) fn spawn_arc_at_rendezvous(
    repo: &Repo,
    cwd: &Path,
    args: &[&str],
    phase: &str,
    envs: &[(&str, &str)],
) -> Child {
    let mut child = arc_command(repo, cwd, args, "session-a", Stdio::piped())
        .envs(envs.iter().copied())
        .env("ARC_TEST_IMPORT_RENDEZVOUS", phase)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let marker = format!("arc test: import {phase}\n");
    let mut stderr = BufReader::new(child.stderr.as_mut().unwrap());
    let mut seen = String::new();
    while !seen.ends_with(&marker) {
        assert_ne!(
            stderr.read_line(&mut seen).unwrap(),
            0,
            "arc exited before reaching {phase}: {seen}"
        );
    }
    child
}

#[cfg(debug_assertions)]
pub(crate) fn release_lock_wait(child: &mut Child) {
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"+").unwrap();
}

pub(crate) fn hold_named_lock(repo: &Repo, name: &str) -> fs::File {
    let dir = repo.root.join(".git/arc/locks");
    fs::create_dir_all(&dir).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(name))
        .unwrap();
    file.lock().unwrap();
    file
}

/// Still running after a pause. A loaded machine may not have reached the lock
/// yet, so a test that depends on what the child already read pauses it with
/// `spawn_arc_paused_on_target_lock` instead.
pub(crate) fn assert_waiting_on_transition_lock(children: &mut [&mut Child]) {
    thread::sleep(Duration::from_millis(250));
    for child in children {
        assert!(
            child.try_wait().unwrap().is_none(),
            "transition command bypassed the externally held product lock"
        );
    }
}

/// How long a test waits on an arc subprocess before calling it hung. Only a
/// process that never finishes reaches it, however loaded the machine is.
pub(crate) const HANG_BOUND: Duration = Duration::from_secs(120);

/// The child's exit status, killing and reaping it as hung after `HANG_BOUND`.
pub(crate) fn wait_for_exit(child: &mut Child) -> ExitStatus {
    if let Some(status) = child.try_wait().unwrap() {
        return status;
    }
    wait_for_exit_within(child, HANG_BOUND)
}

pub(crate) fn wait_for_exit_within(child: &mut Child, bound: Duration) -> ExitStatus {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    let deadline = Instant::now() + bound;
    // SAFETY: pidfd_open accepts the live child's PID and no flags.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
    assert!(fd >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
    // SAFETY: the successful syscall returned a fresh descriptor.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("arc subprocess did not exit within {bound:?}");
        }
        let mut event = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        // SAFETY: event is one initialized pollfd, valid for this call.
        let ready = unsafe { libc::poll(&mut event, 1, millis) };
        if ready > 0 {
            return child.wait().unwrap();
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::Interrupted,
                "poll: {error}"
            );
        }
    }
}

/// The `--timeout` of an `arc watch` a test expects to reach its condition.
/// The watch returns as soon as it does, so only one that never reaches it
/// runs this long.
pub(crate) fn reaching_watch_timeout() -> String {
    HANG_BOUND.as_secs().to_string()
}

/// `arc watch <args>`, with no `--timeout` of its own, spawned with the
/// marker it writes once it has evaluated its conditions and found none
/// reached.
pub(crate) fn spawn_watch(repo: &Repo, cwd: &Path, args: &[&str]) -> (Child, PathBuf) {
    let evaluated = repo.home.join("watch-evaluated");
    let _ = fs::remove_file(&evaluated);
    let child = spawn_arc_with_env(repo, cwd, args, &[("ARC_WATCH_EVALUATED", &evaluated)]);
    (child, evaluated)
}

/// Wait until the watch has evaluated its conditions unreached, so a state
/// change made afterwards is one it observes. A watch that exits first has
/// reached a condition it should not have.
pub(crate) fn await_watch_evaluation(child: &mut Child, evaluated: &Path) {
    let deadline = Instant::now() + HANG_BOUND;
    while !evaluated.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "watch exited {status} before it found its conditions unreached: {}",
                child_stdout(child)
            );
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("watch never evaluated its conditions within {HANG_BOUND:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// `arc watch <args>` evaluates its conditions, finds none reached, and keeps
/// waiting; the watch is then stopped. A `--timeout` short enough to keep a
/// test fast can expire on a loaded host before the watch reads anything,
/// so the evaluation marker, not a deadline, is what this waits on.
pub(crate) fn assert_watch_keeps_waiting(repo: &Repo, cwd: &Path, args: &[&str]) {
    let (mut child, evaluated) = spawn_watch(repo, cwd, args);
    await_watch_evaluation(&mut child, &evaluated);
    let early = child.try_wait().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(early.is_none(), "watch {args:?} returned after evaluating");
}

pub(crate) fn child_stdout(child: &mut Child) -> String {
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    output
}

pub(crate) fn child_stderr(child: &mut Child) -> String {
    let mut output = String::new();
    child
        .stderr
        .take()
        .expect("child spawned with spawn_arc_keeping_stderr")
        .read_to_string(&mut output)
        .unwrap();
    output
}

/// Run a fixture operation with an untracked file in the checkout.
pub(crate) fn with_uncommitted_worktree<T>(repo: &Repo, f: impl FnOnce() -> T) -> T {
    let path = repo.root.join(".arc-test-uncommitted");
    assert!(!path.exists(), "test fixture path already exists: {path:?}");
    fs::write(&path, b"temporary uncommitted fixture\n").unwrap();
    let result = f();
    fs::remove_file(path).unwrap();
    result
}

/// Open a ledger-only change by adopting a branch with no checkout.
pub(crate) fn begin_without_checkout(repo: &Repo, slug: &str, extra: &[&str]) -> String {
    let branch = format!("arc/{slug}");
    let base = extra
        .windows(2)
        .find(|pair| pair[0] == "--base")
        .map(|pair| pair[1])
        .unwrap_or("HEAD");
    git(&repo.root, &["branch", &branch, base]);
    let mut command = repo.arc(&repo.root);
    command.args(["begin", slug, "--adopt", &branch]);
    command.args(extra);
    let out = stdout(&mut command);
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

pub(crate) fn begin_change(repo: &Repo, slug: &str, blocked_by: Option<&str>) -> String {
    let extra = blocked_by
        .map(|blocker| vec!["--blocked-by", blocker])
        .unwrap_or_default();
    begin_without_checkout(repo, slug, &extra)
}

#[track_caller]
pub(crate) fn json_stdout(cmd: &mut AssertCommand) -> serde_json::Value {
    serde_json::from_str(&stdout(cmd)).unwrap()
}

/// `json_stdout` for a call whose non-zero exit is part of what the test
/// observes.
pub(crate) fn json_stdout_any_status(cmd: &mut AssertCommand) -> serde_json::Value {
    serde_json::from_str(&stdout_any_status(cmd)).unwrap()
}

/// Write one journal artifact and return the directory holding it and its
/// filename, which is how every artifact-subject command addresses it.
pub(crate) fn journal_artifact(
    repo: &Repo,
    topic: &str,
    kind: &str,
    body: &str,
) -> (PathBuf, String) {
    let source = repo.home.join(format!("{topic}-body.md"));
    fs::write(&source, body).unwrap();
    let printed = stdout(repo.arc(&repo.root).args([
        "journal",
        "note",
        topic,
        "--kind",
        kind,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    let path = PathBuf::from(printed.trim());
    let file = path.file_name().unwrap().to_string_lossy().to_string();
    (path.parent().unwrap().to_path_buf(), file)
}

/// Every typed journal event, in the order they were appended.
pub(crate) fn journal_event_log(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Drop the values that move with the wall clock before comparing two
/// invocations: `age_seconds` wherever it appears, `observed_at`, and the
/// prose `age:` line. Output that differs only in those is a fact about when
/// each command ran, not about the state it read.
pub(crate) fn without_clock(text: &str) -> String {
    let prose: String = text
        .lines()
        .filter(|line| !(line.starts_with("age: ") && line.ends_with(" old")))
        .map(|line| format!("{line}\n"))
        .collect();
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&prose) else {
        return prose;
    };
    strip_clock_fields(&mut value);
    serde_json::to_string(&value).unwrap()
}

pub(crate) fn strip_clock_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                match key.as_str() {
                    "age_seconds" => *entry = serde_json::Value::Number(0.into()),
                    "observed_at" => *entry = serde_json::Value::String(String::new()),
                    _ => strip_clock_fields(entry),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                strip_clock_fields(item);
            }
        }
        _ => {}
    }
}

/// Move every event in a journal directory `seconds` further into the past, so
/// the leases and lanes recorded there read as that much older.///
/// Shifting the whole log rather than one entry keeps the events in the order
/// they were appended, which is the order a reader replays them in. It lets a
/// test hold a lease long enough that no scheduling delay can end it, and then
/// end it exactly when the test says so, instead of racing a short TTL against
/// the wall clock.
pub(crate) fn age_journal_events(dir: &Path, seconds: i64) {
    let path = dir.join("events.jsonl");
    let aged = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
            let ts = chrono::DateTime::parse_from_rfc3339(event["ts"].as_str().unwrap()).unwrap()
                - chrono::Duration::seconds(seconds);
            event["ts"] = serde_json::Value::String(ts.to_rfc3339());
            serde_json::to_string(&event).unwrap()
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{aged}\n")).unwrap();
}

/// The claim IDs `claim-set` recorded on one artifact, oldest first.
pub(crate) fn artifact_claim_ids(dir: &Path, file: &str) -> Vec<String> {
    journal_event_log(dir)
        .into_iter()
        .filter(|event| event["event"] == "claim-set" && event["file"] == file)
        .map(|event| event["claim_id"].as_str().unwrap().to_string())
        .collect()
}

/// Run a command that executes a fixture script this suite wrote itself,
/// tolerating the kernel's text-file-busy refusal.
///
/// A fixture is executable the moment its bytes are written, but a sibling
/// test thread that forks while the writing descriptor is still open hands the
/// child an inherited copy of it. Until that child reaches its own exec, the
/// fixture's inode has an open writer, and any exec of it is refused. The
/// window is microseconds wide and closes on its own, so retry across it.
pub(crate) fn output_past_busy_text(command: &mut Command) -> std::process::Output {
    for _ in 0..40 {
        match command.output() {
            Err(err) if err.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(Duration::from_millis(25));
            }
            other => return other.unwrap(),
        }
    }
    panic!("fixture executable stayed busy");
}

pub(crate) fn spawn_arc_with_open_stdin(repo: &Repo, cwd: &Path, args: &[&str]) -> Child {
    arc_command(repo, cwd, args, "session-a", Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Read the topics owning the asserted contracts, without expanding the bootstrap.
pub fn instruction_topics(repo: &Repo, topics: &[&str]) -> String {
    topics
        .iter()
        .map(|topic| stdout(repo.arc(&repo.root).args(["guide", topic])))
        .collect()
}
