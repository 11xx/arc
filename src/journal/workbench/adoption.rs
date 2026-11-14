//! What adoption may move, decided before anything moves.
//!
//! Adoption renames material somebody made into the journal. Everything that
//! can refuse it is settled here from the material's path and directory
//! entries: what the path names, where it lies, what it holds, and whether
//! the record of it can be written. Nothing here reads a file's content.

use super::{walk, Measure, DIR, INDEX};
use crate::commands::Ctx;
use crate::config;
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The material an adoption names, resolved once.
pub(super) struct Source {
    /// Absolute, with every directory above the material resolved and the
    /// material's own name kept, so neither a link nor a trailing slash is
    /// followed in its place. Every check and the move itself use this path.
    pub(super) path: PathBuf,
    pub(super) name: OsString,
    pub(super) is_dir: bool,
    pub(super) metadata: std::fs::Metadata,
    /// The path as the adoption's record states it.
    pub(super) recorded: String,
}

/// Resolve the path an adoption names, refusing what it cannot be: no
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
    let parent: PathBuf = components[..components.len() - 1].iter().collect();
    let parent = std::fs::canonicalize(&parent)
        .with_context(|| format!("cannot resolve {}", parent.display()))?;
    let path = parent.join(&name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("{} does not exist", path.display())
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "{} is a symbolic link; adopt the material it points to",
            path.display()
        );
    }
    if !metadata.is_dir() && !metadata.is_file() {
        bail!(
            "{} is neither a regular file nor a directory",
            path.display()
        );
    }
    let recorded = match path.to_str() {
        Some(text) if !text.chars().any(char::is_control) => text.to_string(),
        _ => bail!(
            "{:?} cannot be recorded: an adoption records where the material was as one line of text; rename it first",
            path
        ),
    };
    Ok(Source {
        is_dir: metadata.is_dir(),
        path,
        name,
        metadata,
        recorded,
    })
}

/// Whether `path` names an entry, a link or anything else, without
/// following it.
fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// What makes `dir` a Git repository by its entries: a `.git` entry of any
/// kind, which a working tree holds, or `HEAD`, `objects` and `refs`
/// together, which every repository directory holds, bare or not.
fn repository(dir: &Path) -> Option<&'static str> {
    if present(&dir.join(".git")) {
        return Some("a `.git` entry");
    }
    repository_directory(dir)
}

fn repository_directory(dir: &Path) -> Option<&'static str> {
    (present(&dir.join("HEAD")) && present(&dir.join("objects")) && present(&dir.join("refs")))
        .then_some("HEAD, objects and refs")
}

/// What makes `dir` a journal by its entries. Every journal arc has bound
/// holds `bindings.jsonl`, every journal arc has written holds its locks in
/// `.locks/`, and a journal older than both holds `events.jsonl` beside
/// artifacts. A session log named `events.jsonl` holds none of the others.
/// A journal's cold archive is the sibling named `<journal>-archive`.
fn journal(dir: &Path) -> Result<Option<String>> {
    if let Some(marker) = journal_entries(dir)? {
        return Ok(Some(marker.to_string()));
    }
    let sibling = dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix("-archive"))
        .zip(dir.parent())
        .map(|(stem, parent)| parent.join(stem));
    if let Some(sibling) = sibling {
        if journal_entries(&sibling)?.is_some() {
            return Ok(Some(format!(
                "the cold archive of the journal at {}",
                sibling.display()
            )));
        }
    }
    Ok(None)
}

fn journal_entries(dir: &Path) -> Result<Option<&'static str>> {
    if present(&dir.join("bindings.jsonl")) {
        return Ok(Some("bindings.jsonl"));
    }
    let locks = dir.join(".locks");
    if present(&locks.join("events.lock")) || present(&locks.join("transition.lock")) {
        return Ok(Some("journal locks"));
    }
    if present(&dir.join("events.jsonl")) {
        let entries =
            std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
            let name = entry.file_name();
            let artifact = name.to_str().and_then(crate::journal::parse_artifact_name);
            if artifact.is_some_and(|(_, _, kind)| super::super::known_kind(&kind)) {
                return Ok(Some("an event log beside journal artifacts"));
            }
        }
    }
    Ok(None)
}

/// Refuse a directory that is a repository or a journal.
fn refuse_kept(dir: &Path, root: &Path) -> Result<()> {
    let holds = |what: String| {
        if dir == root {
            format!("{} is {what}", root.display())
        } else {
            format!("{} holds {what} at {}", root.display(), dir.display())
        }
    };
    if let Some(marker) = repository(dir) {
        bail!(
            "{}; adopt material no repository holds",
            holds(format!("a Git repository ({marker})"))
        );
    }
    if let Some(marker) = journal(dir)? {
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
    let parent = path.parent().context("adopted material has a parent")?;
    let parent_dev = std::fs::metadata(parent)
        .with_context(|| format!("cannot read {}", parent.display()))?
        .dev();
    if source.metadata.dev() != parent_dev {
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
                || (inside.components().count() == 1 && !source.is_dir))
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
    for ancestor in parent.ancestors() {
        if ancestor == hot {
            continue;
        }
        if let Some(marker) = journal(ancestor)? {
            bail!(
                "{} lies inside the journal at {} ({marker}); adopt it from that journal's own project",
                path.display(),
                ancestor.display()
            );
        }
        if let Some(marker) = repository_directory(ancestor) {
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
/// from that repository's checkout. Names are matched literally, so no
/// pathspec magic in a file's name changes what is looked up.
pub(super) fn refuse_tracked(source: &Source) -> Result<()> {
    let (probe, spec) = if source.is_dir {
        (source.path.clone(), OsString::from("."))
    } else {
        (
            source
                .path
                .parent()
                .context("adopted material has a parent")?
                .to_path_buf(),
            source.name.clone(),
        )
    };
    let git = |args: &[&std::ffi::OsStr]| -> Result<std::process::Output> {
        let mut command = crate::gitio::git_command();
        command
            .current_dir(&probe)
            .env("LC_ALL", "C")
            .arg("--literal-pathspecs")
            .args(args);
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            command.env_remove(name);
        }
        crate::gitio::bounded_output(&mut command)
            .with_context(|| format!("cannot run git in {}", probe.display()))
    };
    let toplevel = git(&["rev-parse".as_ref(), "--show-toplevel".as_ref()])?;
    if !toplevel.status.success() {
        let stderr = String::from_utf8_lossy(&toplevel.stderr);
        if stderr.contains("not a git repository") {
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

/// Walk the material, refusing what the move would break or what is not
/// the caller's to move, and measure it. Arc writes an index only into a
/// recorded workbench, so an index already present is never arc's.
pub(super) fn survey(source: &Source) -> Result<Measure> {
    if source.name == INDEX {
        bail!(
            "{} is named like the index arc writes in every workbench; rename it before adopting",
            source.path.display()
        );
    }
    if !source.is_dir {
        let mut measure = Measure {
            files: 1,
            bytes: source.metadata.len(),
            newest: None,
            partial: false,
        };
        measure.touch(&source.metadata);
        return Ok(measure);
    }
    let root = &source.path;
    if present(&root.join(INDEX)) {
        bail!(
            "{} already holds {INDEX}, the index arc writes in every workbench; rename it before adopting",
            root.display()
        );
    }
    refuse_kept(root, root)?;
    let device = source.metadata.dev();
    walk(root, None, |path, metadata| {
        if metadata.dev() != device {
            bail!(
                "{} holds a mount point at {}; adopt material on one filesystem",
                root.display(),
                path.display()
            );
        }
        if metadata.is_dir() {
            refuse_kept(path, root)?;
        }
        if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(path)
                .with_context(|| format!("cannot read link {}", path.display()))?;
            let parent = path.parent().unwrap_or(root);
            if target.is_relative() && !lexical(&parent.join(&target)).starts_with(root) {
                bail!(
                    "{} is a relative link leading out of {}, which would point elsewhere once moved",
                    path.display(),
                    root.display()
                );
            }
        }
        Ok(())
    })
}
