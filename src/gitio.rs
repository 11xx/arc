use crate::commands::fork::is_fork_branch;
use anyhow::{bail, Context, Result};
use std::cell::Cell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

thread_local! {
    static COMMAND_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Scope Git subprocesses on this thread to one absolute deadline. Watch uses
/// this so its typed timeout can kill and reap a probe instead of orphaning it.
pub fn with_deadline<T>(deadline: Option<Instant>, f: impl FnOnce() -> Result<T>) -> Result<T> {
    COMMAND_DEADLINE.with(|slot| {
        let previous = slot.replace(deadline);
        let result = f();
        slot.set(previous);
        result
    })
}

pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Run Git and forward its raw output through this process's standard streams.
///
/// Forwarding preserves Git's diff rendering while keeping command output
/// visible to callers that capture Arc itself, including the CLI test harness.
pub fn git_inherit(cwd: &Path, args: &[String]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if !output.status.success() {
        bail!("git {} failed with {}", args.join(" "), output.status);
    }
    Ok(())
}

fn command_output(command: &mut Command) -> Result<Output> {
    let deadline = COMMAND_DEADLINE.get();
    let Some(deadline) = deadline else {
        return Ok(command.output()?);
    };

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("git probe timed out");
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

/// The common Git directory shared by every worktree of one repository.
pub fn common_dir(cwd: &Path) -> Result<PathBuf> {
    let raw = git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(raw);
    let abs = if p.is_absolute() { p } else { cwd.join(p) };
    Ok(std::fs::canonicalize(&abs).unwrap_or(abs))
}

pub fn toplevel(cwd: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(cwd, &["rev-parse", "--show-toplevel"])?))
}

/// Resolve a path under the Git directory, honoring `core.hooksPath` and
/// worktree layout the way Git itself does (e.g. `git_path("hooks")`).
pub fn git_path(cwd: &Path, name: &str) -> Result<PathBuf> {
    let raw = git(cwd, &["rev-parse", "--git-path", name])?;
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

pub fn rev_parse(cwd: &Path, rev: &str) -> Result<String> {
    git(
        cwd,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
}

pub fn head(cwd: &Path) -> Result<String> {
    rev_parse(cwd, "HEAD")
}

pub fn latest_tag(cwd: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Resolve HEAD when the repository has one. An unborn repository is a
/// normal probe condition, while other Git failures remain errors.
pub fn head_if_present(cwd: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if out.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ));
    }
    if out.status.code() == Some(1) {
        return Ok(None);
    }
    bail!(
        "git rev-parse --verify -q HEAD failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

pub fn branch_head(cwd: &Path, branch: &str) -> Result<String> {
    rev_parse(cwd, &format!("refs/heads/{branch}"))
}

pub fn current_branch(cwd: &Path) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .current_dir(cwd)
        .output()?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// Repository-relative paths a change touched between two revisions.
pub fn changed_paths(cwd: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let out = git(cwd, &["diff", "--name-only", &format!("{base}...{head}")])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn merge_base(cwd: &Path, a: &str, b: &str) -> Result<String> {
    git(cwd, &["merge-base", a, b])
}

/// What merging one revision into another would produce, computed without a
/// worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Git could not reconcile the text and a person has to.
    pub conflicts: bool,
    /// The tree a clean merge writes. `None` on a conflict, where there is no
    /// single result to name.
    ///
    /// This is the content the merge would ship, and it belongs to neither
    /// side: a change that is behind its target carries a tree no commit on
    /// either branch holds, and so one nothing has been run against.
    pub tree: Option<String>,
}

pub fn merge_outcome(cwd: &Path, target_rev: &str, head_rev: &str) -> Result<MergeOutcome> {
    let out = Command::new("git")
        .args([
            "merge-tree",
            "--write-tree",
            "--no-messages",
            target_rev,
            head_rev,
        ])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git merge-tree")?;
    match out.status.code() {
        // The tree id is the first line of stdout; anything after it describes
        // conflicts, and a clean merge has none to describe.
        Some(0) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let tree = stdout
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .context("git merge-tree reported a clean merge without naming a tree")?
                .to_string();
            Ok(MergeOutcome {
                conflicts: false,
                tree: Some(tree),
            })
        }
        Some(1) => Ok(MergeOutcome {
            conflicts: true,
            tree: None,
        }),
        code => bail!(
            "git merge-tree failed with exit code {}: {}",
            code.map_or_else(|| "signal".to_string(), |code| code.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// Write a commit holding `tree` under the given parents, touching no ref.
///
/// It exists so a tree belonging to no branch can be checked out, named by
/// evidence, and pinned. The parents record which two sides produced it, so a
/// reader can see what it is a merge of.
pub fn commit_tree_with_parents(
    cwd: &Path,
    tree: &str,
    parents: &[&str],
    message: &str,
) -> Result<String> {
    let mut args = vec!["commit-tree", tree];
    for parent in parents {
        args.push("-p");
        args.push(parent);
    }
    args.push("-m");
    args.push(message);
    git(cwd, &args)
}

/// Who did something to a commit, and when, exactly as the commit object
/// spells it.
///
/// The date keeps Git's own `<seconds> <±hhmm>` spelling rather than a parsed
/// timestamp. A rewrite that reformatted it would move the commit's offset to
/// whatever the rewriting machine happened to be set to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStamp {
    pub name: String,
    pub email: String,
    pub date: String,
}

impl CommitStamp {
    /// The date in the form `git commit-tree` reads from the environment.
    fn git_date(&self) -> String {
        format!("@{}", self.date)
    }

    /// The identity as a commit or tag object header value spells it.
    fn git_line(&self) -> String {
        format!("{} <{}> {}", self.name, self.email, self.date)
    }

    fn parse(line: &str, what: &str) -> Result<Self> {
        let open = line
            .find('<')
            .with_context(|| format!("commit {what} {line:?} names no email"))?;
        let close = line[open..]
            .find('>')
            .map(|offset| open + offset)
            .with_context(|| format!("commit {what} {line:?} names no email"))?;
        Ok(Self {
            name: line[..open].trim_end().to_string(),
            email: line[open + 1..close].to_string(),
            date: line[close + 1..].trim().to_string(),
        })
    }
}

/// A commit object as the database holds it: everything a rewrite has to
/// carry forward unchanged.
///
/// The message is bytes. A commit message is not required to be UTF-8 — that
/// is what the `encoding` header is for — and a rewrite that decoded and
/// re-encoded one would change the object it claims to have only re-signed.
#[derive(Debug, Clone)]
pub struct RawCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: CommitStamp,
    pub committer: CommitStamp,
    pub encoding: Option<String>,
    /// Every header this module does not interpret, as the raw block it
    /// occupies: its `field value` line and each continuation line under it,
    /// newline-separated and without a trailing newline. `mergetag`, which a
    /// merge of a signed tag carries, is the common one.
    ///
    /// The signature headers are absent by construction: a recreated commit
    /// is signed afresh or not at all, so carrying the old `gpgsig` forward
    /// would attest to an object that no longer exists.
    pub extra_headers: Vec<String>,
    pub message: Vec<u8>,
}

impl RawCommit {
    /// The field name of each uninterpreted header, for a caller that has to
    /// say which headers a commit carries.
    pub fn extra_header_fields(&self) -> Vec<String> {
        self.extra_headers
            .iter()
            .map(|block| {
                block
                    .split([' ', '\n'])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

/// Read one commit object whole.
pub fn read_commit(cwd: &Path, rev: &str) -> Result<RawCommit> {
    let raw = git_bytes(cwd, &["cat-file", "commit", rev])?;
    parse_commit_object(&raw)
        .with_context(|| format!("cannot read commit {rev} in {}", cwd.display()))
}

/// Split a commit object into its headers and its message.
///
/// A header value may continue onto following lines, each indented by one
/// space; that is how a signature and a `mergetag` are stored. The message
/// begins after the first empty line and is taken verbatim to the end of the
/// object.
///
/// Every header is kept: the ones this module writes from named fields, and
/// the rest verbatim in the order the object holds them, so a commit is
/// recreated with the headers it had rather than with the ones arc knows
/// about. The exceptions are the signature headers, which describe an object
/// that a recreation replaces.
fn parse_commit_object(raw: &[u8]) -> Result<RawCommit> {
    let split = raw
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .context("commit object has no message")?;
    let (headers, message) = (&raw[..split], &raw[split + 2..]);
    let headers = std::str::from_utf8(headers).context("commit headers are not UTF-8")?;
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = None;
    let mut committer = None;
    let mut encoding = None;
    let mut extra_headers: Vec<String> = Vec::new();
    // Whether the header being read is one to carry verbatim, so its
    // continuation lines travel with it.
    let mut carrying = false;
    for line in headers.lines() {
        if line.starts_with(' ') {
            if carrying {
                let block = extra_headers
                    .last_mut()
                    .expect("a carried header to extend");
                block.push('\n');
                block.push_str(line);
            }
            continue;
        }
        let (field, value) = line.split_once(' ').unwrap_or((line, ""));
        carrying = false;
        match field {
            "tree" => tree = Some(value.to_string()),
            "parent" => parents.push(value.to_string()),
            "author" => author = Some(CommitStamp::parse(value, "author")?),
            "committer" => committer = Some(CommitStamp::parse(value, "committer")?),
            "encoding" => encoding = Some(value.to_string()),
            "gpgsig" | "gpgsig-sha256" => {}
            _ => {
                extra_headers.push(line.to_string());
                carrying = true;
            }
        }
    }
    Ok(RawCommit {
        tree: tree.context("commit object names no tree")?,
        parents,
        author: author.context("commit object names no author")?,
        committer: committer.context("commit object names no committer")?,
        encoding,
        extra_headers,
        message: message.to_vec(),
    })
}

/// Write a commit holding `tree` under the given parents, carrying the given
/// author, committer, dates, encoding and message bytes, and signed when a
/// key is asked for. Touches no ref.
///
/// Recreating a commit this way changes its signature and its parents and
/// nothing else, so a commit rebuilt with the same inputs and no signature
/// has the same object id it started with.
///
/// `commit-tree` writes the headers it has flags for and no others, so a
/// commit carrying a header outside that set is refused here and belongs to
/// [`write_commit_object`], which assembles the object itself. Refusing keeps
/// the guarantee above true: a path that silently omitted a header would
/// produce a commit that is not the one it claims to have only re-signed.
pub fn commit_tree_as(
    cwd: &Path,
    commit: &RawCommit,
    sign: Option<Option<&str>>,
) -> Result<String> {
    let RawCommit {
        tree,
        parents,
        author,
        committer,
        encoding,
        extra_headers,
        message,
    } = commit;
    if !extra_headers.is_empty() {
        bail!(
            "git commit-tree cannot write the {} header carried by the commit holding tree {}",
            commit.extra_header_fields().join(" and the "),
            tree
        );
    }
    let mut args: Vec<String> = Vec::new();
    // The encoding header is written from configuration, so it travels as
    // configuration rather than as a flag `commit-tree` does not have.
    if let Some(encoding) = encoding {
        args.push("-c".into());
        args.push(format!("i18n.commitEncoding={encoding}"));
    }
    args.push("commit-tree".into());
    args.push(tree.clone());
    for parent in parents {
        args.push("-p".into());
        args.push(parent.clone());
    }
    if let Some(key) = sign {
        args.push(match key {
            Some(key) => format!("-S{key}"),
            None => "-S".into(),
        });
    }
    let mut command = Command::new("git");
    command
        .args(&args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", &author.name)
        .env("GIT_AUTHOR_EMAIL", &author.email)
        .env("GIT_AUTHOR_DATE", author.git_date())
        .env("GIT_COMMITTER_NAME", &committer.name)
        .env("GIT_COMMITTER_EMAIL", &committer.email)
        .env("GIT_COMMITTER_DATE", committer.git_date())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("cannot run git commit-tree")?;
    let mut stdin = child.stdin.take().context("git commit-tree has no stdin")?;
    let message = message.to_vec();
    // The message goes out on its own thread for the same reason every other
    // piped Git call here does: a message larger than the pipe buffer
    // deadlocks a writer that is not being read from.
    let writer = thread::spawn(move || stdin.write_all(&message));
    let out = child.wait_with_output().context("git commit-tree failed")?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("the commit-tree writer thread panicked"))??;
    if !out.status.success() {
        bail!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Write the same commit [`commit_tree_as`] would, by assembling the object
/// text and hashing it, so every header the commit carries survives.
///
/// The header order is Git's own — tree, parents, author, committer,
/// encoding, then the carried headers in the order they were read, then the
/// signature last — which is why a commit with nothing to carry comes out
/// with the object id `commit-tree` would have given it.
pub fn write_commit_object(
    cwd: &Path,
    commit: &RawCommit,
    sign: Option<Option<&str>>,
) -> Result<String> {
    let unsigned = commit_object_text(commit, None);
    let object = match sign {
        Some(key) => {
            let signature = openpgp_sign(cwd, &unsigned, key)?;
            commit_object_text(commit, Some(&signature))
        }
        None => unsigned,
    };
    hash_object(cwd, "commit", &object)
}

/// One commit object, exactly as the database holds it.
fn commit_object_text(commit: &RawCommit, signature: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut header = |field: &str, value: &str| {
        out.extend_from_slice(field.as_bytes());
        out.push(b' ');
        out.extend_from_slice(value.as_bytes());
        out.push(b'\n');
    };
    header("tree", &commit.tree);
    for parent in &commit.parents {
        header("parent", parent);
    }
    header("author", &commit.author.git_line());
    header("committer", &commit.committer.git_line());
    if let Some(encoding) = &commit.encoding {
        header("encoding", encoding);
    }
    for block in &commit.extra_headers {
        out.extend_from_slice(block.as_bytes());
        out.push(b'\n');
    }
    if let Some(signature) = signature {
        out.extend_from_slice(&folded_header("gpgsig", signature));
    }
    out.push(b'\n');
    out.extend_from_slice(&commit.message);
    out
}

/// A header whose value spans lines, written the way Git stores one: the
/// first line after the field name, every later line indented by one space.
fn folded_header(field: &str, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, line) in value
        .strip_suffix(b"\n")
        .unwrap_or(value)
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if index == 0 {
            out.extend_from_slice(field.as_bytes());
        }
        out.push(b' ');
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// An annotated tag object as the database holds it.
///
/// The message is bytes for the same reason a commit's is, and the headers
/// under the tag name — the tagger line, and an `encoding` where there is one
/// — travel verbatim, so re-pointing a tag carries its authorship and date
/// rather than restating them.
#[derive(Debug, Clone)]
pub struct RawTag {
    pub object: String,
    pub kind: String,
    pub name: String,
    pub headers: Vec<String>,
    pub message: Vec<u8>,
    /// Whether the original carried a signature over its own text.
    pub signed: bool,
}

/// Read one annotated tag object whole.
pub fn read_tag(cwd: &Path, rev: &str) -> Result<RawTag> {
    let raw = git_bytes(cwd, &["cat-file", "tag", rev])?;
    parse_tag_object(&raw).with_context(|| format!("cannot read tag {rev} in {}", cwd.display()))
}

const PGP_SIGNATURE: &[u8] = b"-----BEGIN PGP SIGNATURE-----";

fn parse_tag_object(raw: &[u8]) -> Result<RawTag> {
    let split = raw
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .context("tag object has no message")?;
    let (headers, body) = (&raw[..split], &raw[split + 2..]);
    let headers = std::str::from_utf8(headers).context("tag headers are not UTF-8")?;
    let mut object = None;
    let mut kind = None;
    let mut name = None;
    let mut carried: Vec<String> = Vec::new();
    for line in headers.lines() {
        if line.starts_with(' ') {
            if let Some(block) = carried.last_mut() {
                block.push('\n');
                block.push_str(line);
            }
            continue;
        }
        let (field, value) = line.split_once(' ').unwrap_or((line, ""));
        match field {
            "object" => object = Some(value.to_string()),
            "type" => kind = Some(value.to_string()),
            "tag" => name = Some(value.to_string()),
            _ => carried.push(line.to_string()),
        }
    }
    // A tag's signature is appended to its message rather than held in a
    // header, so the message is everything up to where the signature starts.
    let signature = body
        .windows(PGP_SIGNATURE.len())
        .position(|window| window == PGP_SIGNATURE);
    Ok(RawTag {
        object: object.context("tag object names no object")?,
        kind: kind.context("tag object names no type")?,
        name: name.context("tag object names no tag")?,
        headers: carried,
        message: body[..signature.unwrap_or(body.len())].to_vec(),
        signed: signature.is_some(),
    })
}

/// Write an annotated tag object, signed when a key is asked for. Touches no
/// ref.
pub fn write_tag_object(cwd: &Path, tag: &RawTag, sign: Option<Option<&str>>) -> Result<String> {
    let mut object = tag_object_text(tag);
    if let Some(key) = sign {
        let signature = openpgp_sign(cwd, &object, key)?;
        object.extend_from_slice(&signature);
    }
    hash_object(cwd, "tag", &object)
}

/// One tag object up to its signature, which is what a tag signature covers.
fn tag_object_text(tag: &RawTag) -> Vec<u8> {
    let mut out = Vec::new();
    for (field, value) in [
        ("object", &tag.object),
        ("type", &tag.kind),
        ("tag", &tag.name),
    ] {
        out.extend_from_slice(format!("{field} {value}\n").as_bytes());
    }
    for block in &tag.headers {
        out.extend_from_slice(block.as_bytes());
        out.push(b'\n');
    }
    out.push(b'\n');
    out.extend_from_slice(&tag.message);
    out
}

/// Store an object of the given type and answer with its name.
fn hash_object(cwd: &Path, kind: &str, object: &[u8]) -> Result<String> {
    let args = ["hash-object", "-t", kind, "-w", "--stdin"].map(String::from);
    let out = piped_output("git", &args, object, |command| {
        command.current_dir(cwd);
    })?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// An armored detached signature over `payload`, made by the OpenPGP program
/// and key Git itself would use.
///
/// `--key` names the key when the caller has one; otherwise it is
/// `user.signingkey`, and where that is unset the signing program's own
/// default key. A repository configured for a signature format this cannot
/// produce is refused rather than signed with the wrong one.
fn openpgp_sign(cwd: &Path, payload: &[u8], key: Option<&str>) -> Result<Vec<u8>> {
    let format = config_value(cwd, "gpg.format")?.unwrap_or_else(|| "openpgp".to_string());
    if format != "openpgp" {
        bail!(
            "gpg.format is {format}; carrying a commit's other headers through a rewrite \
             assembles the object here, and only an openpgp signature can be made over it"
        );
    }
    let program = config_value(cwd, "gpg.program")?.unwrap_or_else(|| "gpg".to_string());
    let key = match key {
        Some(key) => Some(key.to_string()),
        None => config_value(cwd, "user.signingkey")?,
    };
    let mut args: Vec<String> = vec![
        "--status-fd=2".into(),
        "--detach-sign".into(),
        "--armor".into(),
    ];
    if let Some(key) = &key {
        args.push("--local-user".into());
        args.push(key.clone());
    }
    let signature = piped_output(&program, &args, payload, |command| {
        command.current_dir(cwd);
    })
    .with_context(|| format!("cannot sign with {program}"))?;
    if !signature.starts_with(PGP_SIGNATURE) {
        bail!("{program} produced no armored signature");
    }
    Ok(signature)
}

/// One Git configuration value, or nothing where it is unset.
fn config_value(cwd: &Path, key: &str) -> Result<Option<String>> {
    let mut command = Command::new("git");
    command.args(["config", "--get", key]).current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to read git config in {}", cwd.display()))?;
    if !out.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

/// Run a program with `input` on its standard input, answering with its
/// standard output.
///
/// The input goes out on its own thread: an input larger than the pipe buffer
/// deadlocks a writer nobody is reading from.
fn piped_output(
    program: &str,
    args: &[String],
    input: &[u8],
    setup: impl FnOnce(&mut Command),
) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    setup(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let mut stdin = child
        .stdin
        .take()
        .with_context(|| format!("{program} has no stdin"))?;
    let input = input.to_vec();
    let writer = thread::spawn(move || stdin.write_all(&input));
    let out = child
        .wait_with_output()
        .with_context(|| format!("{program} failed"))?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("the {program} writer thread panicked"))??;
    if !out.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Git's raw output, for the callers that read objects rather than text.
fn git_bytes(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    let out = command_output(&mut command)
        .with_context(|| format!("failed to run git in {}", cwd.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// How each commit reachable from `rev` is signed, newest first: the commit,
/// Git's one-letter verification result, and the key that signed it.
///
/// The verification letter and the key are what Git itself reports, including
/// `N` for no signature and an empty key where there is none to name.
pub fn signature_survey(cwd: &Path, rev: &str) -> Result<Vec<(String, String, String)>> {
    let out = git(cwd, &["log", "--format=%H%x00%G?%x00%GK", rev])?;
    out.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split('\0');
            let commit = fields.next().unwrap_or_default().to_string();
            let verification = fields.next().unwrap_or_default().to_string();
            let key = fields.next().unwrap_or_default().to_string();
            if commit.is_empty() {
                bail!("git log returned a row naming no commit");
            }
            Ok((commit, verification, key))
        })
        .collect()
}

/// Every commit from `from` through `rev`, parents before children.
///
/// `from` is included, and its own ancestry is not: excluding the parents of
/// `from` is what makes the range start there rather than at the root, and it
/// is spelled that way so a root commit — which has no parents to exclude —
/// needs no special case.
pub fn commits_from(cwd: &Path, from: &str, rev: &str) -> Result<Vec<String>> {
    let out = git(
        cwd,
        &[
            "rev-list",
            "--topo-order",
            "--reverse",
            rev,
            &format!("^{from}^@"),
        ],
    )?;
    Ok(out.lines().map(str::to_string).collect())
}

/// One local branch or tag.
#[derive(Debug, Clone)]
pub struct LocalRef {
    pub name: String,
    /// What the ref names, which for an annotated tag is the tag object.
    pub object: String,
    /// The object type, which separates a tag pointing straight at a commit
    /// from an annotated tag object, which names a commit but is not one.
    pub kind: String,
    /// The commit the ref leads to: the object itself where that is already a
    /// commit, and what an annotated tag peels to otherwise. A rewrite maps
    /// commits, so this is the id to look up.
    pub commit: String,
}

/// Every local branch and tag.
pub fn local_refs(cwd: &Path) -> Result<Vec<LocalRef>> {
    let out = git(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)",
            "refs/heads/",
            "refs/tags/",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let name = fields.next()?.to_string();
            let object = fields.next()?.to_string();
            let kind = fields.next()?.to_string();
            let peeled = fields.next().unwrap_or_default();
            let commit = if peeled.is_empty() {
                object.clone()
            } else {
                peeled.to_string()
            };
            Some(LocalRef {
                name,
                object,
                kind,
                commit,
            })
        })
        .collect())
}

pub fn blob_oid(cwd: &Path, rev: &str, path: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--verify", &format!("{rev}:{path}")]).ok()
}

/// The tree a commit points at, for comparing what was committed with what was
/// actually there.
pub fn commit_tree(cwd: &Path, rev: &str) -> Result<String> {
    git(cwd, &["rev-parse", &format!("{rev}^{{tree}}")])
}

/// The tree a checkout would have to reproduce to match this worktree:
/// tracked content with its staged and unstaged edits, plus files Git is not
/// yet tracking. Ignored files and the contents of submodules are outside it,
/// exactly as they are outside a commit.
///
/// Evidence recorded against a commit describes a tree no checkout of that
/// commit reproduces whenever the worktree was dirty, which is the ordinary
/// shape of agent execution. This writes the tree into the object database and
/// the caller keeps a ref to it, so the evidence names something that stays
/// resolvable; the scratch index means the real one is untouched.
pub fn worktree_tree(cwd: &Path) -> Result<String> {
    // Inside the Git directory rather than the system temp dir: the index has
    // to be on the same filesystem as the object store it feeds, and this one
    // is already private to the repository.
    let index_path = git_path(cwd, "arc-scratch-index")?.with_extension(crate::ids::new_event_id());
    let index = index_path.display().to_string();

    // From the top of the worktree, so `add -A` means the whole worktree
    // rather than the subtree a caller happened to run from.
    let top = toplevel(cwd)?;
    let run = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&top)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .with_context(|| format!("cannot run git {}", args.join(" ")))?;
        if !output.status.success() {
            anyhow::bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let result = (|| {
        // An unborn HEAD has no tree to read; the add below still captures
        // everything present.
        let _ = run(&["read-tree", "HEAD"]);
        run(&["add", "-A"])?;
        run(&["write-tree"])
    })();
    let _ = std::fs::remove_file(&index_path);
    result
}

pub fn is_clean(cwd: &Path) -> Result<bool> {
    Ok(git(cwd, &["status", "--porcelain"])?.is_empty())
}

/// Tracked and untracked dirt, recorded separately so a waiver reason is
/// self-evident at the moment of waiving and the frequency premise becomes
/// measurable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dirt {
    pub tracked: bool,
    pub untracked: bool,
}

/// What kind of dirt a worktree carries.
///
/// One bool cannot say which, so the claim that this wedges overwhelmingly on
/// untracked-only dirt could not be checked. `git status --porcelain` already
/// exempts ignored paths, so what it reports as untracked is versionable
/// content nobody added — most often the forgotten `git add` the build may
/// already be reading.
pub fn dirt(cwd: &Path) -> Result<Dirt> {
    let status = git(cwd, &["status", "--porcelain"])?;
    let mut dirt = Dirt::default();
    for line in status.lines().filter(|line| !line.trim().is_empty()) {
        if line.starts_with("??") {
            dirt.untracked = true;
        } else {
            dirt.tracked = true;
        }
    }
    Ok(dirt)
}

pub fn branch_exists(cwd: &Path, branch: &str) -> bool {
    git(
        cwd,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

pub fn create_branch(cwd: &Path, branch: &str, base: &str) -> Result<()> {
    git(cwd, &["branch", branch, base])?;
    Ok(())
}

pub fn checkout(cwd: &Path, branch: &str) -> Result<()> {
    git(cwd, &["checkout", branch])?;
    Ok(())
}

pub fn add_worktree(cwd: &Path, path: &Path, branch: &str) -> Result<()> {
    git(
        cwd,
        &[
            "worktree",
            "add",
            path.to_str().context("non-UTF8 worktree path")?,
            branch,
        ],
    )?;
    Ok(())
}

/// Create `branch` at HEAD (or `start`) and check it out in a new worktree,
/// as one Git call. The `-b` must precede the path: `worktree add <path>
/// <new-branch>` treats the branch as an existing commit-ish and fails with
/// `invalid reference`, so a branch cannot be minted positionally.
pub fn add_worktree_new_branch(cwd: &Path, path: &Path, branch: &str, start: &str) -> Result<()> {
    git(
        cwd,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().context("non-UTF8 worktree path")?,
            start,
        ],
    )?;
    Ok(())
}

/// Remove a worktree. Fails loudly when Git refuses — uncommitted or
/// untracked files make it refuse — so a fork worktree holding work arc
/// cannot see is never destroyed silently. The operator's `--force` is the
/// decision that gets past this, not a flag arc reaches for itself.
pub fn remove_worktree(cwd: &Path, path: &Path, force: bool) -> Result<()> {
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if force {
        args.push("--force".to_string());
    }
    args.push(path.to_str().context("non-UTF8 worktree path")?.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git(cwd, &refs)?;
    Ok(())
}

/// How many commits `branch` carries that `base` does not. Either side
/// failing to resolve is an error, not a zero a reader would sum.
///
/// Discover the branch a repository presents as its integration branch.
///
/// Git lists the primary worktree first, so its checked-out branch is the
/// strongest local signal. When that worktree is detached, `origin/HEAD` is
/// used when it names a non-fork branch. `None` means no safe branch was
/// discoverable; callers must not substitute a literal branch name, because
/// it could resolve to an unrelated history.
pub fn default_branch(cwd: &Path) -> Option<String> {
    // `origin/HEAD` is a property of the repository, so it is asked first: the
    // primary worktree's branch is whatever someone happens to have checked
    // out, and a fork measured against that changes its answer when an
    // unrelated checkout moves.
    let declared = git(
        cwd,
        &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|branch| branch.strip_prefix("origin/").map(str::to_string))
    .filter(|branch| !branch.is_empty() && !is_working_branch(branch));
    if declared.is_some() {
        return declared;
    }

    // Without a declared default the primary checkout is the best remaining
    // evidence, but only when it holds an integration branch. A branch arc
    // itself creates is work in progress and never something a fork
    // integrates into, so it yields no base rather than a misleading one.
    primary_worktree_branch(cwd)
        .ok()
        .flatten()
        .filter(|branch| !branch.is_empty())
        .filter(|branch| !is_working_branch(branch))
}

/// Whether a branch is one arc creates to carry work: a change branch or a
/// fork. Neither is ever an integration target.
fn is_working_branch(branch: &str) -> bool {
    is_fork_branch(branch) || branch.starts_with("arc/")
}

pub fn ahead_count(cwd: &Path, base: &str, branch: &str) -> Result<usize> {
    let out = git(cwd, &["rev-list", "--count", &format!("{base}..{branch}")])?;
    out.trim()
        .parse::<usize>()
        .with_context(|| format!("cannot parse rev-list count {out:?}"))
}

/// One entry from `git worktree list --porcelain`. A detached worktree has no
/// branch association; its path is still useful when another durable record
/// names that exact checkout. Git marks an entry `prunable` after its path
/// disappears, so consumers can distinguish a live checkout from stale
/// administrative state.
#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub prunable: bool,
}

/// Read Git's current worktree inventory, preserving the order Git reports.
pub fn worktree_inventory(cwd: &Path) -> Result<Vec<WorktreeEntry>> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;
    let mut current_prunable = false;
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(path) = current_path.replace(PathBuf::from(path)) {
                entries.push(WorktreeEntry {
                    path,
                    branch: current_branch.take(),
                    prunable: current_prunable,
                });
            }
            current_branch = None;
            current_prunable = false;
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current_branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if line == "detached" {
            current_branch = None;
        } else if line == "prunable" || line.starts_with("prunable ") {
            current_prunable = true;
        }
    }
    if let Some(path) = current_path {
        entries.push(WorktreeEntry {
            path,
            branch: current_branch,
            prunable: current_prunable,
        });
    }
    Ok(entries)
}

/// The live worktree (if any) that has `branch` checked out.
pub fn worktree_for_branch(cwd: &Path, branch: &str) -> Result<Option<PathBuf>> {
    let inventory = worktree_inventory(cwd)?;
    Ok(inventory
        .iter()
        .find(|entry| {
            !entry.prunable && entry.path.exists() && entry.branch.as_deref() == Some(branch)
        })
        .map(|entry| entry.path.clone()))
}

/// The primary worktree path (the first entry from `git worktree list`),
/// including when its HEAD is detached.
pub fn primary_worktree(cwd: &Path) -> Result<PathBuf> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    out.lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .context("git worktree list did not contain a primary worktree")
}

pub fn update_ref(cwd: &Path, name: &str, value: &str) -> Result<()> {
    git(cwd, &["update-ref", name, value])?;
    Ok(())
}

pub fn delete_ref(cwd: &Path, name: &str) -> Result<()> {
    git(cwd, &["update-ref", "-d", name])?;
    Ok(())
}

/// One retention ref per patchset: reviewed heads must stay reachable
/// individually, including across branch rewinds.
pub fn retention_ref(change_id: &str, patchset_id: &str) -> String {
    format!("refs/arc/keep/{change_id}/{patchset_id}")
}

pub fn retention_prefix(change_id: &str) -> String {
    format!("refs/arc/keep/{change_id}/")
}

/// The ref that keeps a recorded tree reachable. A tree named only in the
/// ledger is a string; Git does not read JSON, and a garbage collection would
/// take the evidence away while the claim to it remained.
pub fn tree_retention_ref(change_id: &str, event_id: &str) -> String {
    format!("refs/arc/tree/{change_id}/{event_id}")
}

pub fn tree_retention_prefix(change_id: &str) -> String {
    format!("refs/arc/tree/{change_id}/")
}

/// The ref that keeps a synthesized merge reachable while evidence cites it.
///
/// The commit is on no branch, so nothing else holds it; naming it by its own
/// id makes re-evaluating the same merge idempotent rather than accumulating a
/// ref per run.
pub fn merge_retention_ref(change_id: &str, commit: &str) -> String {
    format!("refs/arc/tree/{change_id}/merge-{commit}")
}

/// Every object reachable from a revision.
///
/// One walk answers for every pin at once. A tree recorded as evidence may be
/// any commit's tree in the integrated history, not only the tip's, so
/// comparing against the tip alone would keep pins for trees Git is already
/// holding.
pub fn reachable_objects(cwd: &Path, rev: &str) -> Result<std::collections::HashSet<String>> {
    let listing = git(cwd, &["rev-list", "--objects", rev])?;
    Ok(listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

/// All refs under a prefix as (refname, object id) pairs.
pub fn list_refs(cwd: &Path, prefix: &str) -> Result<Vec<(String, String)>> {
    let out = git(
        cwd,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect())
}

/// Which of these revisions this repository cannot resolve.
///
/// One `cat-file --batch-check` process answers for every revision at once,
/// because a ledger of any age holds thousands and a check that costs a
/// process each stops being run.
pub fn missing_objects<'a>(
    cwd: &Path,
    revisions: impl Iterator<Item = &'a str>,
) -> Result<Vec<String>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // A revision containing a newline would become two queries and shift every
    // later answer, so those are refused rather than silently misaligned.
    let revisions: Vec<&str> = revisions
        .filter(|revision| !revision.contains(['\n', '\r']))
        .collect();
    if revisions.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run git cat-file")?;
    let query = revisions.join("\n") + "\n";
    let mut stdin = child.stdin.take().context("git cat-file has no stdin")?;
    // The query goes out on its own thread so this one can drain stdout while
    // it is still being written. `cat-file` blocks once its answer pipe fills,
    // and a caller that only starts reading after the last query byte is sent
    // deadlocks against it — both processes asleep writing to a full pipe.
    let writer = std::thread::spawn(move || stdin.write_all(query.as_bytes()));
    let output = child.wait_with_output().context("git cat-file failed")?;
    let wrote = writer
        .join()
        .map_err(|_| anyhow::anyhow!("the git cat-file writer thread panicked"))?;
    // A git that died early makes the write fail with a broken pipe, and its
    // own stderr says more about why than that symptom does. The write failure
    // is worth reporting only when git itself was fine.
    if output.status.success() {
        wrote.context("cannot write to git cat-file")?;
    }
    if !output.status.success() {
        // Reporting nothing missing because the probe failed would turn a
        // broken repository into a clean bill of health.
        anyhow::bail!(
            "git cat-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let answers: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if answers.len() != revisions.len() {
        anyhow::bail!(
            "git cat-file answered {} of {} revisions; refusing to guess which",
            answers.len(),
            revisions.len()
        );
    }

    // One answer line per query line, in order. A resolvable revision answers
    // with its object; anything else names why it could not be resolved.
    let mut missing = Vec::new();
    for (revision, answer) in revisions.iter().zip(&answers) {
        if answer.ends_with(" missing") || answer.ends_with(" ambiguous") {
            missing.push((*revision).to_string());
        }
    }
    Ok(missing)
}

pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let out = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git merge-base")?;
    Ok(out.status.success())
}

/// Whether a rebase is stopped part-way in this worktree, waiting for a person.
///
/// Git records that state as a directory beside the index, and `--git-path`
/// resolves it for the worktree rather than for the repository's common dir,
/// so a linked worktree answers about itself.
pub fn rebase_in_progress(cwd: &Path) -> Result<bool> {
    for state_dir in ["rebase-merge", "rebase-apply"] {
        let path = git(cwd, &["rev-parse", "--git-path", state_dir])?;
        if cwd.join(path).exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Paths Git could not reconcile, as the index reports them.
pub fn unmerged_paths(cwd: &Path) -> Result<Vec<String>> {
    Ok(git(cwd, &["diff", "--name-only", "--diff-filter=U"])?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// How a rebase ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseOutcome {
    /// Every commit replayed; the branch is on the new base.
    Replayed,
    /// Git stopped on a conflict and left the rebase in progress for a person
    /// to resolve. Nothing is aborted: the partial replay is their work.
    Stopped,
}

/// Replay the worktree's branch onto `onto`.
///
/// A rebase that stops on a conflict is an outcome, not a failure: it leaves
/// state a person continues from. Anything else — an unknown revision, a
/// refusal — is an error, and leaves no rebase in progress to distinguish it.
pub fn rebase(cwd: &Path, onto: &str) -> Result<RebaseOutcome> {
    let Err(refusal) = git(cwd, &["rebase", onto]) else {
        return Ok(RebaseOutcome::Replayed);
    };
    if rebase_in_progress(cwd)? {
        return Ok(RebaseOutcome::Stopped);
    }
    Err(refusal)
}

pub fn commit_count(cwd: &Path, base: &str, head: &str) -> Result<usize> {
    git(cwd, &["rev-list", "--count", &format!("{base}..{head}")])?
        .parse()
        .context("git rev-list --count returned a non-numeric commit count")
}

/// The branch checked out in the primary worktree (the main checkout,
/// always first in `git worktree list`). None when it is detached.
pub fn primary_worktree_branch(cwd: &Path) -> Result<Option<String>> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    let mut in_first = false;
    for line in out.lines() {
        if line.starts_with("worktree ") {
            if in_first {
                break; // reached the second worktree
            }
            in_first = true;
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            return Ok(Some(b.to_string()));
        }
    }
    Ok(None)
}

pub fn commit_parents(cwd: &Path, rev: &str) -> Result<Vec<String>> {
    let out = git(cwd, &["rev-list", "--parents", "-n", "1", rev])?;
    let mut ids = out.split_whitespace().map(str::to_string);
    ids.next();
    Ok(ids.collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIdentity {
    pub author_name: String,
    pub author_email: String,
    pub committer_name: String,
    pub committer_email: String,
}

pub fn commit_identity(cwd: &Path, rev: &str) -> Result<CommitIdentity> {
    let out = git(
        cwd,
        &["show", "-s", "--format=%an%x00%ae%x00%cn%x00%ce", rev],
    )?;
    let mut fields = out.split('\0');
    let identity = CommitIdentity {
        author_name: fields
            .next()
            .context("git omitted author name")?
            .to_string(),
        author_email: fields
            .next()
            .context("git omitted author email")?
            .to_string(),
        committer_name: fields
            .next()
            .context("git omitted committer name")?
            .to_string(),
        committer_email: fields
            .next()
            .context("git omitted committer email")?
            .to_string(),
    };
    if fields.next().is_some() {
        bail!("git returned unexpected commit identity fields");
    }
    Ok(identity)
}

pub fn commit_exists(cwd: &Path, oid: &str) -> Result<bool> {
    let object = format!("{oid}^{{commit}}");
    let out = Command::new("git")
        .args(["cat-file", "-e", "--", &object])
        .current_dir(cwd)
        .output()
        .context("failed to spawn git cat-file")?;
    Ok(out.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assembling a commit object here has to agree with `commit-tree` on
    /// every byte, or the header order, the identity lines or the message
    /// boundary are wrong and a carried header would come with a silent
    /// change to something else.
    #[test]
    fn a_hand_built_commit_is_the_one_commit_tree_writes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "-b", "master"],
            vec!["config", "user.name", "Tester"],
            vec!["config", "user.email", "tester@example.invalid"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git(&root, &args).unwrap();
        }
        std::fs::write(root.join("one.txt"), "one\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        // A message with an interior blank line and trailing whitespace, so
        // the boundary between the headers and the message is checked rather
        // than assumed.
        git(
            &root,
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "feat: one  \n\nbody\n",
            ],
        )
        .unwrap();

        let head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let commit = read_commit(&root, &head).unwrap();
        assert!(commit.extra_headers.is_empty());
        assert_eq!(write_commit_object(&root, &commit, None).unwrap(), head);
        assert_eq!(commit_tree_as(&root, &commit, None).unwrap(), head);
    }

    /// A header outside the set `commit-tree` has flags for is carried, and
    /// carrying it is the only difference: recreating the commit unsigned
    /// reproduces its object id.
    #[test]
    fn an_uninterpreted_header_is_carried_verbatim() {
        let raw = b"tree 0000000000000000000000000000000000000000\n\
                    parent 1111111111111111111111111111111111111111\n\
                    author A <a@example.invalid> 1700000000 +0000\n\
                    committer C <c@example.invalid> 1700000001 +0000\n\
                    mergetag object 2222222222222222222222222222222222222222\n\
                    \x20type commit\n\
                    \x20tag v1\n\
                    \x20\n\
                    \x20a release\n\
                    gpgsig -----BEGIN PGP SIGNATURE-----\n\
                    \x20\n\
                    \x20abc\n\
                    \x20-----END PGP SIGNATURE-----\n\
                    \nthe message\n";
        let commit = parse_commit_object(raw).unwrap();
        assert_eq!(commit.extra_header_fields(), vec!["mergetag".to_string()]);
        assert_eq!(
            commit.extra_headers[0],
            "mergetag object 2222222222222222222222222222222222222222\n type commit\n tag v1\n \n a release"
        );
        assert_eq!(commit.message, b"the message\n");
        // The signature covered the object it was read from, so a recreation
        // carries everything but that.
        let rebuilt = commit_object_text(&commit, None);
        assert_eq!(
            String::from_utf8_lossy(&rebuilt),
            String::from_utf8_lossy(raw).replace(
                "gpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n",
                ""
            )
        );
        // Signing writes the header Git writes: the first line after the
        // field name, the rest indented by one space.
        let signed = commit_object_text(&commit, Some(b"-----BEGIN PGP SIGNATURE-----\n\nabc\n"));
        assert!(String::from_utf8_lossy(&signed)
            .contains("gpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n\nthe message\n"));
    }

    #[test]
    fn deadline_kills_and_reaps_a_slow_probe() {
        let started = Instant::now();
        with_deadline(Some(started + Duration::from_millis(50)), || {
            let mut command = Command::new("sleep");
            command.arg("5");
            let error = command_output(&mut command).unwrap_err();
            assert!(error.to_string().contains("timed out"));
            Ok(())
        })
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A query larger than the pipe buffers on both sides must still answer.
    ///
    /// Sending the whole query before reading any of it deadlocks: `cat-file`
    /// sleeps writing answers into a full pipe while the caller sleeps writing
    /// queries into another one. Twenty thousand revisions puts both sides well
    /// past the 64 KiB a pipe holds, so this fails by timing out rather than by
    /// hanging the suite.
    #[test]
    fn a_query_larger_than_the_pipe_buffer_still_answers() {
        let revisions: Vec<String> = (0..20_000).map(|n| format!("{n:040x}")).collect();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let answered = missing_objects(Path::new("."), revisions.iter().map(String::as_str))
                .map(|missing| missing.len());
            let _ = sender.send(answered);
        });
        let answered = receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("missing_objects deadlocked against git cat-file")
            .expect("missing_objects failed");
        // Every one of them is fabricated, so every one is unresolvable.
        assert_eq!(answered, 20_000);
    }
}
