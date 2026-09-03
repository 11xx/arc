//! A disposable copy of one project, under a sandbox prefix.
//!
//! A sandbox prefix stands in for the home directory, so pointing `ARC_SANDBOX`
//! at an empty prefix gives arc a fresh set of roots and no history. What that
//! alone does not give is a *copy*: to rehearse something destructive — a
//! history rewrite, a bulk debt discharge, a schema migration — the rehearsal
//! has to start from the state the real run would start from.
//!
//! So a clone carries the four things a project's answers are made of: the
//! repository, the ledger, the journal, and the configuration. The registry
//! needs nothing of its own, being derived from the journal root.
//!
//! Everything a clone writes is under the prefix. The source is read, never
//! written, and the copy has no remote pointing back at it, so nothing done in
//! a sandbox can travel home by accident.

use super::Ctx;
use crate::config;
use crate::gitio;
use crate::journal;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Names a prefix as arc's to remove. `discard` deletes a whole directory
/// tree, so it acts only where arc itself recorded what the tree is.
const MARKER: &str = ".arc-sandbox.json";

const MARKER_SCHEMA: &str = "arc-sandbox/1";

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    schema: String,
    created_at: String,
    /// The project this sandbox was copied from, as it was then.
    source_repository: String,
    source_ledger: String,
    source_journal: String,
    /// The copy, inside the prefix.
    repository: String,
    ledger: String,
    journal: String,
}

#[derive(Debug, Serialize)]
struct CloneReport {
    schema: &'static str,
    prefix: String,
    repository: String,
    ledger: String,
    journal: String,
    config: String,
    revision: String,
}

pub fn clone(ctx: &Ctx, prefix: &Path, json: bool) -> Result<i32> {
    let prefix = absolute(prefix)?;
    // The source is resolved as a whole project, not as the checkout the
    // caller happens to stand in: a linked worktree holds one branch, while
    // the ledger, the journal, and every other branch belong to the project.
    let source_repository = gitio::primary_worktree(&ctx.cwd)
        .context("arc sandbox clone copies a project, so it runs inside a Git repository")?;
    let source_ledger = Store::resolve_root(&ctx.cwd)?;
    let source_journal = journal::resolve_dir(&ctx.cwd)?;

    let name = source_repository
        .file_name()
        .context("cannot determine the repository's name")?
        .to_owned();
    let repository = prefix.join(&name);
    refuse_unless_available(&prefix, &repository)?;

    let ai_home = config::ai_home_under(&prefix);
    let journal = ai_home
        .join("journals")
        .join(config::path_slug(&repository));
    let config_path = ai_home.join("arc").join("config.toml");

    fs::create_dir_all(&prefix).with_context(|| format!("cannot create {}", prefix.display()))?;
    clone_repository(&source_repository, &repository)?;
    let ledger = repository.join(".git").join("arc");
    if source_ledger.is_dir() {
        copy_tree(&source_ledger, &ledger)?;
    }
    if source_journal.is_dir() {
        copy_tree(&source_journal, &journal)?;
        // The copy is addressed by its own project's slugged path, and the
        // binding is what states which project that is. Left saying the
        // source, the copy would be read as the source's journal living in an
        // unexpected place.
        journal::record_binding(
            &journal,
            "cloned",
            &repository,
            &source_repository,
            ctx.harness.clone(),
            ctx.session.clone(),
        )?;
    }
    write_config(ctx, &config_path)?;

    let revision = gitio::head_if_present(&repository)?.unwrap_or_default();
    write_marker(
        &prefix,
        &Marker {
            schema: MARKER_SCHEMA.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            source_repository: display(&source_repository),
            source_ledger: display(&source_ledger),
            source_journal: display(&source_journal),
            repository: display(&repository),
            ledger: display(&ledger),
            journal: display(&journal),
        },
    )?;

    let report = CloneReport {
        schema: "arc-sandbox-clone/1",
        prefix: display(&prefix),
        repository: display(&repository),
        ledger: display(&ledger),
        journal: display(&journal),
        config: display(&config_path),
        revision,
    };
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("sandbox: {}", report.prefix);
        println!("repository: {}", report.repository);
        println!("ledger: {}", report.ledger);
        println!("journal: {}", report.journal);
        println!("config: {}", report.config);
        println!(
            "work in it: ARC_SANDBOX={} arc catchup, from {}",
            report.prefix, report.repository
        );
        // Everything derived from the ledger answers the same in the copy.
        // Checkouts are the exception, and saying so here is cheaper than
        // letting the first `arc catchup` in a sandbox look like a fault.
        println!(
            "note: an open change's recorded checkout is the source's; the copy \
             stands on {} and has none until one is made here",
            if report.revision.is_empty() {
                "no revision"
            } else {
                &report.revision
            }
        );
    }
    Ok(0)
}

#[derive(Debug, Serialize)]
struct DiffReport {
    schema: &'static str,
    prefix: String,
    ledger_events: SetDiff,
    journal_events: SetDiff,
    refs: SetDiff,
}

impl DiffReport {
    fn identical(&self) -> bool {
        self.ledger_events.identical() && self.journal_events.identical() && self.refs.identical()
    }
}

/// What one side holds that the other does not. Both directions are reported:
/// a sandbox that lost an event says something as loudly as one that gained it.
#[derive(Debug, Serialize)]
struct SetDiff {
    only_in_sandbox: Vec<String>,
    only_in_source: Vec<String>,
}

impl SetDiff {
    fn between(sandbox: BTreeSet<String>, source: BTreeSet<String>) -> SetDiff {
        SetDiff {
            only_in_sandbox: sandbox.difference(&source).cloned().collect(),
            only_in_source: source.difference(&sandbox).cloned().collect(),
        }
    }

    fn identical(&self) -> bool {
        self.only_in_sandbox.is_empty() && self.only_in_source.is_empty()
    }
}

pub fn diff(_ctx: &Ctx, prefix: &Path, json: bool) -> Result<i32> {
    let prefix = absolute(prefix)?;
    let marker = read_marker(&prefix)?;

    let report = DiffReport {
        schema: "arc-sandbox-diff/1",
        prefix: display(&prefix),
        ledger_events: SetDiff::between(
            ledger_events(Path::new(&marker.ledger))?,
            ledger_events(Path::new(&marker.source_ledger))?,
        ),
        journal_events: SetDiff::between(
            journal_events(Path::new(&marker.journal))?,
            journal_events(Path::new(&marker.source_journal))?,
        ),
        refs: SetDiff::between(
            refs(Path::new(&marker.repository))?,
            refs(Path::new(&marker.source_repository))?,
        ),
    };
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else if report.identical() {
        println!("identical: ledger events, journal events, and refs all match the source");
    } else {
        render("ledger events", &report.ledger_events);
        render("journal events", &report.journal_events);
        render("refs", &report.refs);
    }
    Ok(0)
}

fn render(label: &str, diff: &SetDiff) {
    if diff.identical() {
        return;
    }
    println!("{label}:");
    for item in &diff.only_in_sandbox {
        println!("  + {item}");
    }
    for item in &diff.only_in_source {
        println!("  - {item}");
    }
}

pub fn discard(ctx: &Ctx, prefix: &Path) -> Result<i32> {
    let prefix = absolute(prefix)?;
    // Reading the marker first is what makes this safe to run: an arbitrary
    // directory is refused before anything is removed.
    read_marker(&prefix)?;
    if ctx.cwd.starts_with(&prefix) {
        bail!(
            "cannot discard {} from inside it; run this from outside the sandbox",
            prefix.display()
        );
    }
    fs::remove_dir_all(&prefix).with_context(|| format!("cannot remove {}", prefix.display()))?;
    println!("discarded: {}", prefix.display());
    Ok(0)
}

/// Copy a repository so the copy holds every ref the source does and stands on
/// the same revision, with no remote pointing home.
///
/// A mirror clone is the only clone that reproduces refs exactly — an ordinary
/// one turns branches into remote-tracking refs, which would leave every
/// change's branch missing from the copy's own namespace. It produces a bare
/// repository, so the copy is turned back into a checkout afterwards.
///
/// Objects are copied rather than hardlinked: a sandbox exists to be treated
/// roughly, and sharing inodes with the source is the one way roughness could
/// reach it.
fn clone_repository(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .context("sandbox repository has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let git_dir = destination.join(".git");
    gitio::git(
        parent,
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            "--mirror",
            &source.to_string_lossy(),
            &git_dir.to_string_lossy(),
        ],
    )
    .with_context(|| format!("cannot clone {}", source.display()))?;
    gitio::git(destination, &["config", "core.bare", "false"])?;
    // A sandbox that can push is not a sandbox.
    gitio::git(
        destination,
        &["config", "--remove-section", "remote.origin"],
    )?;
    // A mirror clone leaves no worktree and no index; HEAD already names the
    // source's branch, so a hard reset is what populates the checkout.
    gitio::git(destination, &["reset", "--quiet", "--hard"])?;
    Ok(())
}

/// Write the sandbox's configuration: the behaviour the source project chose,
/// and no path settings at all.
///
/// A path a configuration states is a path outside the prefix, which is the one
/// thing a sandbox must not inherit. The prefix supplies every root instead, so
/// the copy's layout is derived from where it is rather than from where the
/// source was.
fn write_config(ctx: &Ctx, path: &Path) -> Result<()> {
    let _ = ctx;
    let source = config::load()?;
    let parent = path.parent().context("config path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let body = format!(
        "[journal]\nauto_log = {}\n\n[identity]\ndetect = {}\n\n[provenance]\ngit_identity = {:?}\n",
        source.journal_auto_log,
        source.identity_detect,
        source.provenance_git_identity.as_str(),
    );
    fs::write(path, body).with_context(|| format!("cannot write {}", path.display()))
}

/// Every ledger event, addressed the way the store addresses it: change (or
/// the repository scope) and event file. The path is the identity — two
/// ledgers holding the same event file hold the same event.
fn ledger_events(root: &Path) -> Result<BTreeSet<String>> {
    let mut events = BTreeSet::new();
    for (scope, dir) in scopes(root)? {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", dir.display()))
            }
        };
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                events.insert(format!("{scope}/{name}"));
            }
        }
    }
    Ok(events)
}

/// The event directories a ledger has: one per change, plus the repository's own.
fn scopes(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut scopes = vec![(
        Store::REPOSITORY_SCOPE.to_string(),
        root.join("repository").join("events"),
    )];
    let changes = root.join("changes");
    match fs::read_dir(&changes) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                scopes.push((name, entry.path().join("events")));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", changes.display()))
        }
    }
    Ok(scopes)
}

/// Every journal event, as the line that records it. A journal event carries
/// no identifier of its own, and the line is what a reader replays.
fn journal_events(dir: &Path) -> Result<BTreeSet<String>> {
    let path = dir.join("events.jsonl");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect())
}

fn refs(repository: &Path) -> Result<BTreeSet<String>> {
    Ok(gitio::git(
        repository,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )?
    .lines()
    .filter(|line| !line.trim().is_empty())
    .map(str::to_owned)
    .collect())
}

fn absolute(prefix: &Path) -> Result<PathBuf> {
    let expanded = config::expand_tilde(&prefix.to_string_lossy())?;
    if !expanded.is_absolute() {
        bail!(
            "a sandbox prefix must be an absolute path, got {}",
            prefix.display()
        );
    }
    Ok(expanded)
}

/// A prefix is available when arc can build a whole sandbox in it: either it
/// does not exist, or it is an empty directory. An existing sandbox is refused
/// rather than merged into, because two projects' copies in one prefix share a
/// journal root and cannot both be the source of an answer.
fn refuse_unless_available(prefix: &Path, repository: &Path) -> Result<()> {
    if prefix.join(MARKER).is_file() {
        bail!(
            "{} already holds a sandbox; compare it with `arc sandbox diff` or remove it with \
             `arc sandbox discard`",
            prefix.display()
        );
    }
    if repository.exists() {
        bail!("{} already exists", repository.display());
    }
    match fs::read_dir(prefix) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                bail!(
                    "{} is not empty; a sandbox prefix is a whole directory arc owns",
                    prefix.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("cannot read {}", prefix.display())),
    }
}

fn write_marker(prefix: &Path, marker: &Marker) -> Result<()> {
    let path = prefix.join(MARKER);
    let mut body = serde_json::to_vec_pretty(marker)?;
    body.push(b'\n');
    fs::write(&path, body).with_context(|| format!("cannot write {}", path.display()))
}

fn read_marker(prefix: &Path) -> Result<Marker> {
    let path = prefix.join(MARKER);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "{} is not a sandbox arc made: no {MARKER}",
            prefix.display()
        )
    })?;
    let marker: Marker =
        serde_json::from_slice(&bytes).with_context(|| format!("malformed {}", path.display()))?;
    if marker.schema != MARKER_SCHEMA {
        bail!(
            "{} records an unreadable sandbox format {:?}",
            path.display(),
            marker.schema
        );
    }
    Ok(marker)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("cannot create {}", destination.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("cannot read {}", source.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        // Follows what the entry points at rather than reproducing the link:
        // a copy holding a symlink into the source is a copy that is not one.
        let kind = fs::metadata(&from)
            .with_context(|| format!("cannot stat {}", from.display()))?
            .file_type();
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to)
                .with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn display(path: &Path) -> String {
    path.display().to_string()
}
