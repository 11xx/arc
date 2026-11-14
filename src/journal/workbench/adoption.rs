//! What adoption may move, decided before anything moves.
//!
//! Adoption renames material somebody made into the journal. The material
//! is opened once, without following a link, while the journal's transition
//! lock is held; every check below reads that opened object, and the move
//! confirms it is still the one at the material's path. Nothing here reads
//! a file's content except the first line of an index arc may have written.
//!
//! These guards stop mistakes and accidents: a wrong path, a link, a
//! repository, a journal, another arc writer, which the journal's locks
//! serialize, and a file that appears while arc works. They do not defeat a
//! process that deliberately swaps directory entries during the call: no
//! rename on this platform binds the identity of the entry it moves, and
//! Git is asked about the material by its path. A swap between the last
//! check and the rename is detected after the move, which then stops as
//! moved and not recorded.

use super::fsat::{Dir, Stat};
use super::{walk, Measure, DIR, INDEX};
use crate::commands::Ctx;
use crate::config;
use anyhow::{bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The material an adoption names, opened once.
pub(super) struct Source {
    /// Absolute: the directory above the material resolved, the material's
    /// own name kept, so neither a link nor a trailing slash is followed in
    /// its place.
    pub(super) path: PathBuf,
    parent_path: PathBuf,
    pub(super) parent: Dir,
    /// The directory above the material as it stood when opened.
    pub(super) parent_stat: Stat,
    pub(super) name: OsString,
    pub(super) stat: Stat,
    /// The material itself, opened, when it is a directory.
    pub(super) dir: Option<Dir>,
    /// The path as the adoption's record states it.
    pub(super) recorded: String,
}

impl Source {
    pub(super) fn is_dir(&self) -> bool {
        self.dir.is_some()
    }
}

/// Open the material an adoption names, refusing what it cannot be: no
/// material, a link, anything but a file or a directory, or a path the
/// record cannot state on one line of text.
pub(super) fn resolve(ctx: &Ctx, raw: &str) -> Result<Source> {
    let requested = config::expand_tilde(raw)?;
    let requested = if requested.is_absolute() {
        requested
    } else {
        ctx.cwd.join(requested)
    };
    // Rebuilding the path from its components drops a trailing slash or `.`,
    // which would otherwise make the lookup follow a link to a directory. A
    // path ending in `..` names a directory, never a link, and resolves to
    // it.
    let requested = if requested.ends_with("..") {
        std::fs::canonicalize(&requested).with_context(|| format!("cannot resolve {raw}"))?
    } else {
        requested
    };
    let components: Vec<Component<'_>> = requested.components().collect();
    let name = match components.last() {
        Some(Component::Normal(name)) => name.to_os_string(),
        _ => bail!("{raw} is a filesystem root"),
    };
    let parent_path: PathBuf = components[..components.len() - 1].iter().collect();
    let parent_path = std::fs::canonicalize(&parent_path)
        .with_context(|| format!("cannot resolve {}", parent_path.display()))?;
    let path = parent_path.join(&name);
    let parent = Dir::open(&parent_path)
        .with_context(|| format!("cannot open {}", parent_path.display()))?;
    let parent_stat = parent.stat()?;
    let Some(stat) = parent
        .entry(&name)
        .with_context(|| format!("cannot read {}", path.display()))?
    else {
        bail!("{} does not exist", path.display())
    };
    if stat.is_link() {
        bail!(
            "{} is a symbolic link; adopt the material it points to",
            path.display()
        );
    }
    if !stat.is_dir() && !stat.is_file() {
        bail!(
            "{} is neither a regular file nor a directory",
            path.display()
        );
    }
    let dir = if stat.is_dir() {
        let dir = parent
            .open_dir(&name)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if !dir.stat()?.same(&stat) {
            bail!(
                "{} changed while it was opened; nothing moved",
                path.display()
            );
        }
        Some(dir)
    } else {
        None
    };
    let recorded = match path.to_str() {
        Some(text) if !text.chars().any(char::is_control) => text.to_string(),
        _ => bail!(
            "{:?} cannot be recorded: an adoption records where the material was as one line of text; rename it first",
            path
        ),
    };
    Ok(Source {
        path,
        parent_path,
        parent,
        parent_stat,
        name,
        stat,
        dir,
        recorded,
    })
}

/// Refuse unless the material's path still leads, through the directory
/// opened above it, to the object every check read. Called immediately
/// before the move.
pub(super) fn confirm(source: &Source) -> Result<()> {
    let parent_now = std::fs::metadata(&source.parent_path).ok();
    let entry = source.parent.entry(&source.name)?;
    let parent_same = parent_now.is_some_and(|metadata| {
        metadata.dev() == source.parent_stat.dev && metadata.ino() == source.parent_stat.ino
    });
    if !parent_same || !entry.is_some_and(|entry| entry.same(&source.stat)) {
        bail!(
            "{} changed after it was checked; nothing moved",
            source.path.display()
        );
    }
    Ok(())
}

fn present(dir: &Dir, name: &str) -> bool {
    dir.entry(OsStr::new(name))
        .is_ok_and(|entry| entry.is_some())
}

/// What makes `dir` a Git repository by its entries: a `.git` entry of any
/// kind, which a working tree holds, or `HEAD`, `objects` and `refs`
/// together, which every repository directory holds, bare or not.
fn repository(dir: &Dir) -> Option<&'static str> {
    if present(dir, ".git") {
        return Some("a `.git` entry");
    }
    repository_directory(dir)
}

fn repository_directory(dir: &Dir) -> Option<&'static str> {
    (present(dir, "HEAD") && present(dir, "objects") && present(dir, "refs"))
        .then_some("HEAD, objects and refs")
}

/// What makes `dir` a journal by its entries. Every journal arc has bound
/// holds `bindings.jsonl`, every journal arc has written holds its locks in
/// `.locks/`, and a journal older than both holds `events.jsonl` beside
/// artifacts. A session log named `events.jsonl` holds none of the others.
/// A journal's cold archive is the sibling named `<journal>-archive`, read
/// through `parent` when `dir` is its entry `name`.
fn journal(dir: &Dir, parent: Option<(&Dir, &OsStr)>) -> Result<Option<String>> {
    if let Some(marker) = journal_entries(dir)? {
        return Ok(Some(marker.to_string()));
    }
    let Some((parent, name)) = parent else {
        return Ok(None);
    };
    let stem = name
        .as_bytes()
        .strip_suffix(b"-archive")
        .filter(|stem| !stem.is_empty());
    if let Some(stem) = stem {
        if let Ok(sibling) = parent.open_dir(OsStr::from_bytes(stem)) {
            if journal_entries(&sibling)?.is_some() {
                return Ok(Some(format!(
                    "the cold archive of the journal {}",
                    OsStr::from_bytes(stem).to_string_lossy()
                )));
            }
        }
    }
    Ok(None)
}

fn journal_entries(dir: &Dir) -> Result<Option<&'static str>> {
    if present(dir, "bindings.jsonl") {
        return Ok(Some("bindings.jsonl"));
    }
    if let Ok(locks) = dir.open_dir(OsStr::new(".locks")) {
        if present(&locks, "events.lock") || present(&locks, "transition.lock") {
            return Ok(Some("journal locks"));
        }
    }
    if present(dir, "events.jsonl") {
        for name in dir.names()? {
            let name = name?;
            let artifact = name.to_str().and_then(crate::journal::parse_artifact_name);
            if artifact.is_some_and(|(_, _, kind)| super::super::known_kind(&kind)) {
                return Ok(Some("an event log beside journal artifacts"));
            }
        }
    }
    Ok(None)
}

/// Refuse a directory that is a repository or a journal. `at` is where it
/// lies and `root` the material holding it.
fn refuse_kept(dir: &Dir, parent: Option<(&Dir, &OsStr)>, at: &Path, root: &Path) -> Result<()> {
    let holds = |what: String| {
        if at == root {
            format!("{} is {what}", root.display())
        } else {
            format!("{} holds {what} at {}", root.display(), at.display())
        }
    };
    if let Some(marker) = repository(dir) {
        bail!(
            "{}; adopt material no repository holds",
            holds(format!("a Git repository ({marker})"))
        );
    }
    if let Some(marker) = journal(dir, parent)? {
        bail!(
            "{}; a journal's material is adopted from its own project",
            holds(format!("a journal ({marker})"))
        );
    }
    Ok(())
}

/// Refuse the material by where it lies and what it holds of the places
/// arc and the caller stand on. `dir` is this journal as arc names it and
/// `hot` the same journal resolved.
pub(super) fn refuse_location(
    ctx: &Ctx,
    dir: &Path,
    hot: &Path,
    source: &Source,
    in_place: bool,
) -> Result<()> {
    let path = &source.path;
    // Where this journal itself lives is not the material's to answer for.
    let own = path.strip_prefix(hot).unwrap_or(path);
    if let Some(named) = own.components().find(|component| {
        let name = component.as_os_str();
        name == ".git" || name == ".arc"
    }) {
        bail!(
            "{} is inside a `{}` directory, which belongs to a repository or to arc",
            path.display(),
            named.as_os_str().to_string_lossy()
        );
    }
    let mut stands = vec![ctx.cwd.clone()];
    stands.extend(std::env::current_dir().ok());
    for cwd in stands {
        if std::fs::canonicalize(&cwd).is_ok_and(|cwd| cwd.starts_with(path)) {
            bail!(
                "{} is the current directory or holds it; adopt material from outside where you stand",
                path.display()
            );
        }
    }
    let mut homes = vec![config::expand_tilde("~")?];
    homes.extend(config::real_home().ok());
    for home in homes {
        if std::fs::canonicalize(&home).is_ok_and(|home| home.starts_with(path)) {
            bail!(
                "{} is the home directory or holds it; adopt a directory inside it",
                path.display()
            );
        }
    }
    if source.stat.dev != source.parent_stat.dev {
        bail!(
            "{} is a mount point; adopt material inside it",
            path.display()
        );
    }
    let cfg = config::load()?;
    let archive = super::super::archive_dir(dir);
    let mut journals = vec![config::global_journal_dir()?];
    journals.extend(
        crate::registry::known_journals(&cfg)?
            .into_iter()
            .map(|(_, journal)| journal),
    );
    let mut kept: Vec<(PathBuf, String)> = vec![(archive.clone(), "this journal's archive".into())];
    for journal in journals {
        let resolved = super::super::resolve_existing_prefix(&journal)?;
        if resolved == hot {
            continue;
        }
        kept.push((
            resolved.clone(),
            format!("the journal at {}", resolved.display()),
        ));
        kept.push((
            super::super::archive_dir(&resolved),
            format!("the archive of the journal at {}", resolved.display()),
        ));
    }
    for (root, what) in &kept {
        let root = super::super::resolve_existing_prefix(root)?;
        if path.starts_with(&root) {
            bail!(
                "{} lies inside {what}; adopt it from that journal's own project",
                path.display()
            );
        }
        if root.starts_with(path) {
            bail!("{} holds {what}; adopt material inside it", path.display());
        }
    }
    if hot.starts_with(path) {
        bail!(
            "{} holds this journal; adopt material inside it, such as one entry of a loose directory",
            path.display()
        );
    }
    if let Ok(inside) = path.strip_prefix(hot) {
        let first = inside
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        if first == DIR && !in_place {
            bail!(
                "{} is already inside this journal's workbenches",
                path.display()
            );
        }
        if first != DIR
            && (first.starts_with('.')
                || first == super::super::EXCHANGE_DIR
                || (inside.components().count() == 1 && !source.is_dir()))
        {
            bail!(
                "{} is part of the journal arc keeps; adopt material arc does not own, such as a directory under it",
                path.display()
            );
        }
    } else {
        let mut stores = vec![(cfg.ai_home.clone(), "arc's home")];
        if let Some(data_root) = &cfg.data_root {
            stores.push((data_root.clone(), "arc's data root"));
        }
        for (root, what) in stores {
            let root = super::super::resolve_existing_prefix(&root)?;
            if path.starts_with(&root) {
                bail!(
                    "{} is inside {} ({what}); adopt material from outside it",
                    path.display(),
                    root.display()
                );
            }
            if root.starts_with(path) {
                bail!(
                    "{} holds {} ({what}); adopt material inside it",
                    path.display(),
                    root.display()
                );
            }
        }
    }
    for ancestor in source.parent_path.ancestors() {
        if ancestor == hot {
            continue;
        }
        let Ok(opened) = Dir::open(ancestor) else {
            continue;
        };
        let above = ancestor.parent().and_then(|above| Dir::open(above).ok());
        let parent = above.as_ref().zip(ancestor.file_name());
        if let Some(marker) = journal(&opened, parent)? {
            bail!(
                "{} lies inside the journal at {} ({marker}); adopt it from that journal's own project",
                path.display(),
                ancestor.display()
            );
        }
        if let Some(marker) = repository_directory(&opened) {
            bail!(
                "{} lies inside the Git repository at {} ({marker})",
                path.display(),
                ancestor.display()
            );
        }
    }
    Ok(())
}

/// Refuse material a Git repository tracks: moving it would delete files
/// from that repository's checkout. Names are matched literally, and no
/// variable of the caller's environment steers which repository Git finds;
/// anything but a clear answer refuses.
pub(super) fn refuse_tracked(source: &Source) -> Result<()> {
    let (probe, spec) = if source.is_dir() {
        (source.path.clone(), OsString::from("."))
    } else {
        (source.parent_path.clone(), source.name.clone())
    };
    let git = |args: &[&OsStr]| -> Result<std::process::Output> {
        let mut command = crate::gitio::git_command();
        let fixed: Vec<OsString> = command
            .get_envs()
            .map(|(name, _)| name.to_os_string())
            .collect();
        for (name, _) in std::env::vars_os() {
            if name.as_bytes().starts_with(b"GIT_") && !fixed.contains(&name) {
                command.env_remove(&name);
            }
        }
        command
            .current_dir(&probe)
            .env("LC_ALL", "C")
            .arg("--literal-pathspecs")
            .args(args);
        command
            .output()
            .with_context(|| format!("cannot run git in {}", probe.display()))
    };
    let toplevel = git(&["rev-parse".as_ref(), "--show-toplevel".as_ref()])?;
    if !toplevel.status.success() {
        let stderr = String::from_utf8_lossy(&toplevel.stderr);
        // Git's own answer that no repository holds the probe: its fatal
        // exit status and a diagnostic that begins with exactly this. The
        // same words inside any other error, such as a malformed
        // configuration value, are no answer.
        if toplevel.status.code() == Some(128) && stderr.starts_with("fatal: not a git repository")
        {
            return Ok(());
        }
        bail!(
            "cannot tell whether a repository tracks {}: {}",
            source.path.display(),
            stderr.trim()
        );
    }
    let tracked = git(&[
        "ls-files".as_ref(),
        "-z".as_ref(),
        "--".as_ref(),
        spec.as_os_str(),
    ])?;
    if !tracked.status.success() {
        bail!(
            "cannot tell whether a repository tracks {}: {}",
            source.path.display(),
            String::from_utf8_lossy(&tracked.stderr).trim()
        );
    }
    if !tracked.stdout.is_empty() {
        bail!(
            "{} is tracked by the Git repository at {}; adopt material no repository tracks",
            source.path.display(),
            String::from_utf8_lossy(&toplevel.stdout).trim()
        );
    }
    Ok(())
}

/// `path` with `.` and `..` resolved by name, without touching the
/// filesystem.
fn lexical(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// Walk the opened material, refusing what the move would break or what is
/// not the caller's to move, and measure it. An index already at its top
/// is refused unless the material is a directory adopted where it stands
/// and the index is one arc wrote.
pub(super) fn survey(source: &Source, in_place: bool) -> Result<Measure> {
    if source.name == INDEX {
        bail!(
            "{} is named like the index arc writes in every workbench; rename it before adopting",
            source.path.display()
        );
    }
    let Some(root) = &source.dir else {
        let mut measure = Measure {
            files: 1,
            bytes: source.stat.size,
            newest: None,
            partial: false,
        };
        measure.touch(&source.stat);
        return Ok(measure);
    };
    let root_path = &source.path;
    if root.entry(OsStr::new(INDEX))?.is_some() && !(in_place && super::is_arc_index(root)) {
        bail!(
            "{} is not an index arc wrote, and arc never overwrites, moves or renames it: move or rename it, then adopt {} again",
            root_path.join(INDEX).display(),
            root_path.display()
        );
    }
    refuse_kept(
        root,
        Some((&source.parent, &source.name)),
        root_path,
        root_path,
    )?;
    let device = source.stat.dev;
    walk(root, root_path, None, |path, parent, name, stat| {
        if stat.dev != device {
            bail!(
                "{} holds a mount point at {}; adopt material on one filesystem",
                root_path.display(),
                path.display()
            );
        }
        if stat.is_dir() {
            let child = parent
                .open_dir(name)
                .with_context(|| format!("cannot open {}", path.display()))?;
            refuse_kept(&child, Some((parent, name)), path, root_path)?;
        }
        if stat.is_link() {
            let target = parent
                .read_link(name)
                .with_context(|| format!("cannot read link {}", path.display()))?;
            let holder = path.parent().unwrap_or(root_path);
            if target.is_relative() && !lexical(&holder.join(&target)).starts_with(root_path) {
                bail!(
                    "{} is a relative link leading out of {}, which would point elsewhere once moved",
                    path.display(),
                    root_path.display()
                );
            }
        }
        Ok(())
    })
}
