//! `arc progress`: where each series of work stands, read on request.
//!
//! The list of items is the journal's (see `journal::series`). Everything
//! else a report shows is what the ledger has recorded when it is asked: a
//! linked item's stage, who holds its change, and when it was integrated. A
//! stage is a record, never a live judgement; `arc status` is the live
//! reading. Nothing here runs unless a caller asks for it.

use super::Ctx;
use crate::journal::series::{
    self, ItemLink, ItemState, ItemWait, Placement, RelatedSeries, Series, SinceStart,
};
use crate::journal::JournalEvent;
use crate::model::{Closure, Verdict};
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod view;

/// The schema `arc progress --json` prints.
const PROGRESS_SCHEMA: &str = "arc-progress/1";
/// The schema `arc workspace progress --json` prints.
const WORKSPACE_PROGRESS_SCHEMA: &str = "arc-workspace-progress/1";

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ProgressArgs {
    #[command(subcommand)]
    command: Option<ProgressCmd>,
    /// A series, by filename or topic. Omitted, every series of this project
    /// that is not retired
    series: Option<String>,
    /// Print the complete text, one fact per line and never painted,
    /// instead of the terminal view
    #[arg(long, conflicts_with = "json")]
    plain: bool,
    /// Emit structured JSON (arc-progress/1) instead of text
    #[arg(long)]
    json: bool,
}

/// The form a report is printed in.
#[derive(Clone, Copy)]
pub(crate) enum Form {
    /// For a person at a terminal: shortened, and painted when standard
    /// output is one.
    Terminal,
    /// Every recorded fact as line-oriented text.
    Plain,
    Json,
}

impl Form {
    pub(crate) fn of(plain: bool, json: bool) -> Self {
        match (json, plain) {
            (true, _) => Self::Json,
            (false, true) => Self::Plain,
            (false, false) => Self::Terminal,
        }
    }
}

#[derive(Subcommand)]
pub enum ProgressCmd {
    /// Start a series: write a `progress` artifact and declare its items
    ///
    /// Prints the series' filename, one line per declared item with its id,
    /// and last the `arc begin` call that links a change to an item as the
    /// change opens. Items declared here are the start that later edits are
    /// counted from. A series needs no plan; `--follows` records the plan it
    /// follows when there is one.
    Start {
        /// Kebab-case topic slug
        topic: String,
        /// The series' title
        #[arg(long)]
        title: String,
        /// The plan artifact this series follows, by its bare filename in
        /// this journal
        #[arg(long)]
        follows: Option<String>,
        /// An item's title; repeat once per item, in order
        #[arg(long = "item")]
        items: Vec<String>,
    },
    /// Add an item, at the end unless placed; prints its id
    Add {
        /// Series filename or topic
        series: String,
        /// The item's title, one line
        title: String,
        /// Place the new item directly before this item
        #[arg(long, conflicts_with = "after")]
        before: Option<String>,
        /// Place the new item directly after this item
        #[arg(long)]
        after: Option<String>,
    },
    /// Rename an item; its id stays
    Rename {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// The new title, one line
        title: String,
    },
    /// Move an item before or after another
    Move {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Put the item directly before this item
        #[arg(long, conflicts_with = "after", required_unless_present = "after")]
        before: Option<String>,
        /// Put the item directly after this item
        #[arg(long)]
        after: Option<String>,
    },
    /// Drop an item with a reason; it stays visible and leaves the count
    Drop {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Why the item no longer belongs to the series, one line
        #[arg(long)]
        reason: String,
    },
    /// Mark done an item no change carries
    ///
    /// A linked item's stage is read from its change and is never marked;
    /// unlink it first.
    Done {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// What finished it, one line
        #[arg(long)]
        note: Option<String>,
    },
    /// Reopen a done or dropped item, linked or not
    ///
    /// Undoes `drop` and `done` alike. A reopened item that is linked to a
    /// change reads its stage from that change again.
    Reopen {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Why it is open again, one line
        #[arg(long)]
        note: Option<String>,
    },
    /// Link an item to a change, of this project's ledger or another's
    ///
    /// The item's stage then follows the change, read from the ledger that
    /// holds it. An item already linked takes a new link only when its
    /// change closed without integrating, and only while its link is still
    /// the one this command read. `arc begin <slug> --item <series>:<item>`
    /// links at the moment a change of this project is opened.
    Link {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Change ID or unique prefix; `<project>::<change>` names a change
        /// of another registered project, by slug or label
        change: String,
    },
    /// Remove an item's link to its change
    Unlink {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
    },
    /// Say an item waits on a question of this journal
    ///
    /// The item shows the wait while the question is open and waits on a
    /// person, and nothing of it once the question is settled; its stage is
    /// unchanged either way. A new wait replaces the item's earlier one.
    Wait {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// The discussion holding the question, by its bare filename in this
        /// journal
        discussion: String,
        /// The question's id in that discussion
        #[arg(long)]
        question: String,
    },
    /// Remove the question an item was said to wait on
    Unwait {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
    },
    /// Split an item into several; the original shows as split and leaves the count
    Split {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// A new item's title; repeat once per item, at least twice
        #[arg(long = "into", required = true)]
        into: Vec<String>,
        /// Why the item is split, one line
        #[arg(long)]
        reason: String,
    },
    /// Relate a series to a series in another project
    ///
    /// The relation is recorded in this project's journal only. It is mutual
    /// once the same call is made in the other project, and the report says
    /// when it is one-sided.
    Relate {
        /// Series filename or topic in this project
        series: String,
        /// `<project>::<series>`: a registered project by slug or label, and
        /// a series of its journal by filename or topic
        target: String,
    },
    /// Remove a relation this project's series records
    ///
    /// Only this journal changes; the other project's relation, if it made
    /// one, stays until the same call is made there.
    Unrelate {
        /// Series filename or topic in this project
        series: String,
        /// `<project>::<series>` as the relation names it: the project by
        /// slug or label, the series by filename or topic
        target: String,
    },
    /// Retire a series as superseded by its conclusion
    ///
    /// Records `consumed` with outcome `superseded`, naming the conclusion
    /// as what resolved it. A retired series leaves the default listing,
    /// stays readable by name, and refuses further `arc progress` edits. The
    /// journal's own amendments still apply to it, as to every consumed
    /// artifact (see `arc guide progress`).
    Retire {
        /// Series filename or topic
        series: String,
        /// The conclusion artifact's bare filename in this journal; a path,
        /// another journal's reference or a symbolic link is refused
        #[arg(long)]
        by: String,
    },
}

fn placement<'a>(before: &'a Option<String>, after: &'a Option<String>) -> Placement<'a> {
    match (before.as_deref(), after.as_deref()) {
        (Some(item), _) => Placement::Before(item),
        (None, Some(item)) => Placement::After(item),
        (None, None) => Placement::End,
    }
}

pub fn run(ctx: &Ctx, args: ProgressArgs) -> Result<i32> {
    let Some(command) = args.command else {
        return report(ctx, args.series.as_deref(), Form::of(args.plain, args.json));
    };
    match command {
        ProgressCmd::Start {
            topic,
            title,
            follows,
            items,
        } => {
            let file = series::start(ctx, &topic, &title, follows.as_deref(), &items)?;
            println!("{file}");
            for (index, item) in items.iter().enumerate() {
                println!("  i{}  {}", index + 1, item.trim());
            }
            println!("a change joins as it opens: arc begin <slug> --item {topic}:<item>");
        }
        ProgressCmd::Add {
            series: name,
            title,
            before,
            after,
        } => {
            let id = series::add(ctx, &name, &title, placement(&before, &after))?;
            println!("added {id}: {}", title.trim());
        }
        ProgressCmd::Rename {
            series: name,
            item,
            title,
        } => {
            series::rename(ctx, &name, &item, &title)?;
            println!("renamed {item}: {}", title.trim());
        }
        ProgressCmd::Move {
            series: name,
            item,
            before,
            after,
        } => {
            series::move_item(ctx, &name, &item, placement(&before, &after))?;
            println!("moved {item}");
        }
        ProgressCmd::Drop {
            series: name,
            item,
            reason,
        } => {
            series::drop_item(ctx, &name, &item, &reason)?;
            println!("dropped {item}");
        }
        ProgressCmd::Done {
            series: name,
            item,
            note,
        } => {
            series::done(ctx, &name, &item, note.as_deref())?;
            println!("done {item}");
        }
        ProgressCmd::Reopen {
            series: name,
            item,
            note,
        } => {
            series::reopen(ctx, &name, &item, note.as_deref())?;
            println!("reopened {item}");
        }
        ProgressCmd::Link {
            series: name,
            item,
            change,
        } => {
            let target = preflight_item(ctx, &name, &item)?;
            let (link, shown) = resolve_link(ctx, &change)?;
            series::link(
                ctx,
                &target.series,
                &target.item,
                &link,
                target.expected.as_ref(),
                target.replaceable,
            )?;
            println!("linked {item} to {shown}");
        }
        ProgressCmd::Unlink { series: name, item } => {
            let link = series::unlink(ctx, &name, &item)?;
            println!("unlinked {item} from {link}");
        }
        ProgressCmd::Wait {
            series: name,
            item,
            discussion,
            question,
        } => {
            series::wait(ctx, &name, &item, &discussion, &question)?;
            println!("{item} waits on {question} of {discussion}");
        }
        ProgressCmd::Unwait { series: name, item } => {
            let wait = series::unwait(ctx, &name, &item)?;
            println!(
                "{item} waits no longer on {} of {}",
                wait.question, wait.file
            );
        }
        ProgressCmd::Split {
            series: name,
            item,
            into,
            reason,
        } => {
            let parts = series::split(ctx, &name, &item, &into, &reason)?;
            println!("split {item} into {}", parts.join(", "));
        }
        ProgressCmd::Relate {
            series: name,
            target,
        } => relate(ctx, &name, &target)?,
        ProgressCmd::Unrelate {
            series: name,
            target,
        } => {
            let relation = recorded_relation(ctx, &name, &target)?;
            series::unrelate(ctx, &name, relation.clone())?;
            println!("unrelated {}::{}", relation.project, relation.file);
        }
        ProgressCmd::Retire { series: name, by } => {
            let file = series::retire(ctx, &name, &by)?;
            println!("retired {file}: superseded by {by}");
        }
    }
    Ok(0)
}

/// An item `begin --item` or `progress link` will link, checked before
/// anything is written. `expected` is the link read here; the write compares
/// it under the journal's lock.
pub(crate) struct ItemTarget {
    series: String,
    item: String,
    expected: Option<ItemLink>,
    replaceable: bool,
}

/// Check an item for a link. Only the journal is read, and a ledger only
/// when the item already carries a link: the one holding that link's
/// change, through discovery that creates nothing, so a refused item leaves
/// no ledger behind.
fn preflight_item(ctx: &Ctx, name: &str, item: &str) -> Result<ItemTarget> {
    let (file, current) = series::linkable(ctx, name, item)?;
    let replaceable = match &current {
        None => true,
        Some(link) => {
            let state = match &link.project {
                None => Store::discover_existing_for_reading(&ctx.cwd)?
                    .and_then(|store| store.state(&link.change).ok()),
                Some(slug) => Registry::read()?
                    .get(slug)
                    .and_then(|project| project.ledger.state(&link.change).ok().flatten()),
            };
            state
                .and_then(|state| state.closure)
                .is_some_and(|closure| closure.outcome != Closure::Integrated)
        }
    };
    Ok(ItemTarget {
        series: file,
        item: item.to_string(),
        expected: current,
        replaceable,
    })
}

/// Check `<series>:<item>` for `begin --item` before the change is opened.
pub(crate) fn preflight_begin_item(ctx: &Ctx, spec: &str) -> Result<ItemTarget> {
    let Some((name, item)) = spec.rsplit_once(':') else {
        bail!("--item takes <series>:<item>, such as my-series:i3; got {spec:?}");
    };
    let target = preflight_item(ctx, name, item)?;
    if !target.replaceable {
        bail!(
            "item {item} of {} is linked to a change that is open or integrated; \
             `arc progress unlink` it first",
            target.series
        );
    }
    Ok(target)
}

/// Link the item a `begin --item` named to the change it opened.
pub(crate) fn link_opened(ctx: &Ctx, target: &ItemTarget, change_id: &str) -> Result<()> {
    let link = ItemLink {
        project: None,
        change: change_id.to_string(),
    };
    series::link(
        ctx,
        &target.series,
        &target.item,
        &link,
        target.expected.as_ref(),
        target.replaceable,
    )?;
    println!("item: {}:{}", target.series, target.item);
    Ok(())
}

/// The change a `link` names, and how to print it: a change of this
/// project's ledger, or `<project>::<change>` for one of another registered
/// project's, resolved in that project's ledger.
fn resolve_link(ctx: &Ctx, spec: &str) -> Result<(ItemLink, String)> {
    let Some((prefix, change)) = spec.split_once("::") else {
        let change = ctx.store()?.resolve_change(spec)?;
        return Ok((
            ItemLink {
                project: None,
                change: change.clone(),
            },
            change,
        ));
    };
    let here = crate::journal::resolve_dir(&ctx.cwd)?;
    let project = crate::journal::registered_project(prefix)?;
    if same_dir(&project.journal_dir, &here) {
        bail!("{prefix} is this project; name its change without a project");
    }
    let store = project
        .anchor
        .as_deref()
        .map(|anchor| Ledger::at(anchor).opened())
        .transpose()?
        .flatten()
        .with_context(|| format!("{} has no ledger to hold {change}", project.label()))?;
    let change = store.resolve_change(change)?;
    let shown = format!("{}::{change}", project.label());
    Ok((
        ItemLink {
            project: Some(project.slug),
            change,
        },
        shown,
    ))
}

fn relate(ctx: &Ctx, name: &str, target: &str) -> Result<()> {
    let Some((prefix, other)) = target.split_once("::") else {
        bail!("relate takes <project>::<series>, naming a registered project; got {target:?}");
    };
    let here = crate::journal::resolve_dir(&ctx.cwd)?;
    let project = crate::journal::registered_project(prefix)?;
    if same_dir(&project.journal_dir, &here) {
        bail!("{prefix} is this project; relate names a series in another project");
    }
    let remote = series::named_series(&project.journal_dir, other)
        .with_context(|| format!("cannot read a series {other:?} in {}", project.label()))?;
    let local = series::named_series(&here, name)?;
    let related = RelatedSeries {
        project: project.slug.clone(),
        file: remote.file.clone(),
    };
    let recorded = series::relate(ctx, name, related)?;
    let own_slug = own_slug(&crate::config::load()?, &here);
    let mutual = own_slug.as_deref().is_some_and(|slug| {
        remote
            .related
            .iter()
            .any(|back| back.project == slug && back.file == local.file)
    });
    println!(
        "{} {} -> {}::{} ({})",
        if recorded {
            "related"
        } else {
            "already related"
        },
        local.file,
        project.label(),
        remote.file,
        if mutual {
            "mutual".to_string()
        } else {
            format!(
                "one-sided until the same call is made in {}",
                project.label()
            )
        }
    );
    Ok(())
}

/// The relation a series records that `target` names. The project matches
/// the recorded slug, or else the slug the registry gives a project of that
/// label; the series matches the recorded filename or its topic. Neither the
/// other project's journal events nor its ledger is read, so a relation to a
/// project that is gone can still be removed by its slug.
fn recorded_relation(ctx: &Ctx, name: &str, target: &str) -> Result<RelatedSeries> {
    let Some((prefix, other)) = target.split_once("::") else {
        bail!("unrelate takes <project>::<series>; got {target:?}");
    };
    let here = crate::journal::resolve_dir(&ctx.cwd)?;
    let local = series::named_series(&here, name)?;
    let names_series = |relation: &&RelatedSeries| {
        relation.file == other
            || crate::journal::parse_artifact_name(&relation.file)
                .is_some_and(|(_, topic, _)| topic == other)
    };
    let mut matches: Vec<&RelatedSeries> = local
        .related
        .iter()
        .filter(|relation| relation.project == prefix)
        .filter(names_series)
        .collect();
    // A label is not what the relation records, so only then is the
    // registry asked which slug it names.
    if matches.is_empty() {
        if let Ok(project) = crate::journal::registered_project(prefix) {
            matches = local
                .related
                .iter()
                .filter(|relation| relation.project == project.slug)
                .filter(names_series)
                .collect();
        }
    }
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("{} does not relate to {target}", local.file),
        _ => bail!(
            "{target} names several relations of {}; name the series by file",
            local.file
        ),
    }
}

fn same_dir(left: &Path, right: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(left) == canonical(right)
}

/// This journal's registry slug: the name another project's relation
/// records it by.
fn own_slug(cfg: &crate::config::Config, dir: &Path) -> Option<String> {
    crate::registry::known_journals(cfg)
        .ok()?
        .into_iter()
        .find(|(_, journal)| same_dir(journal, dir))
        .map(|(slug, _)| slug)
}

// --- Reading -------------------------------------------------------------

/// One item as a report shows it.
#[derive(Serialize)]
struct ItemRow {
    id: String,
    title: String,
    /// One word. Unlinked: `open`, `done`, `dropped` or `split`. Linked,
    /// as the change's events record it: `opened`, `snapshotted`,
    /// `verified`, `reviewed`, `integrated`, or `closed` for a change closed
    /// without integrating; `unknown` when the ledger holding it has no such
    /// change, or cannot be read for a change of another project.
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<String>,
    /// For a change of another project's ledger, that project's label, as
    /// `arc workspace` names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    /// For a change of another project's ledger, that project's registry
    /// slug, which the link records.
    #[serde(skip_serializing_if = "Option::is_none")]
    project_slug: Option<String>,
    /// For a `reviewed` item, the verdict on the newest patchset:
    /// `approved`, `changes-requested` or `comment-only`.
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<&'static str>,
    /// Who holds a live claim on the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    held_by: Option<String>,
    /// Review rounds on the change that asked for changes.
    #[serde(skip_serializing_if = "is_zero")]
    changes_requested_rounds: usize,
    /// When the change was integrated, and the revision it integrated as.
    #[serde(skip_serializing_if = "Option::is_none")]
    integrated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    /// For a change in flight, the newest run the ledger records dispatched
    /// against it with no recorded end.
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch: Option<Dispatch>,
    /// The journal question the item waits on, while it is open and waits
    /// on a person.
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting: Option<Waiting>,
    /// The reason or note recorded with a done, dropped, split or reopened item.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    split_into: Vec<String>,
}

/// A run dispatched against a linked change, as the ledger records it: no
/// runner is asked whether it is alive.
#[derive(Serialize, Clone)]
struct Dispatch {
    /// The subject the dispatch was recorded for, else the route it went
    /// through.
    to: String,
    route: String,
    at: String,
    /// Whether a backend session is recorded as executing it.
    bound: bool,
}

/// An open question an item waits on.
#[derive(Serialize)]
struct Waiting {
    file: String,
    question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl ItemRow {
    /// The item as its series leaves it, before any ledger is read.
    fn of(item: &series::Item) -> Self {
        Self {
            id: item.id.clone(),
            title: item.title.clone(),
            stage: match item.state {
                ItemState::Open => "open",
                ItemState::Done => "done",
                ItemState::Dropped => "dropped",
                ItemState::Split => "split",
            },
            change: item.change.clone(),
            project: None,
            project_slug: None,
            verdict: None,
            held_by: None,
            changes_requested_rounds: 0,
            integrated_at: None,
            revision: None,
            dispatch: None,
            waiting: None,
            note: item.note.clone(),
            split_into: item.split_into.clone(),
        }
    }

    fn in_flight(&self) -> bool {
        self.change.is_some()
            && !matches!(
                self.stage,
                "integrated" | "closed" | "dropped" | "split" | "unknown"
            )
    }
}

/// The list counted as it stands.
#[derive(Serialize, Default)]
struct Counts {
    /// Items that stand: every item but the dropped and the split.
    items: usize,
    /// Integrated and done items, the share's numerator.
    complete: usize,
    integrated: usize,
    done: usize,
    in_flight: usize,
    /// Standing items no change carries and nobody marked done.
    open: usize,
    /// Linked items whose change closed without integrating.
    closed: usize,
    /// Standing items waiting on an open question, each also counted by its
    /// stage.
    #[serde(skip_serializing_if = "is_zero")]
    waiting: usize,
    /// `complete` over `items`, rounded down; absent with no items.
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<usize>,
    since_start: SinceStart,
}

#[derive(Serialize, Clone)]
struct Integration {
    item: String,
    at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

#[derive(Serialize)]
struct Relation {
    /// The other project's label, as `arc workspace` names it.
    project: String,
    /// The other project's registry slug, which the relation records.
    slug: String,
    file: String,
    /// Whether the other series relates back; absent when this command did
    /// not read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    mutual: Option<bool>,
}

#[derive(Serialize)]
pub(crate) struct SeriesReport {
    file: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    follows: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retired: Option<series::Retirement>,
    related: Vec<Relation>,
    items: Vec<ItemRow>,
    counts: Counts,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_integration: Option<Integration>,
}

/// A related series from another project, in compact form.
#[derive(Serialize)]
struct RelatedReport {
    project: String,
    slug: String,
    file: String,
    /// This project's series that relate to it.
    related_from: Vec<String>,
    /// Why the other journal could not be read; the remaining fields are
    /// then absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retired: Option<series::Retirement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<Counts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_integration: Option<Integration>,
    in_flight: Vec<ItemRow>,
}

#[derive(Serialize)]
struct WaitingQuestion {
    file: String,
    question: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    heading: Option<String>,
    asked_at: String,
}

#[derive(Serialize)]
struct ProgressReport {
    schema: &'static str,
    journal_dir: String,
    series: Vec<SeriesReport>,
    related: Vec<RelatedReport>,
    waiting_on_person: Vec<WaitingQuestion>,
}

/// Opens one project's ledger without creating anything; `None` when the
/// project has none.
type Opener = Box<dyn Fn() -> Result<Option<Store>>>;

/// One project's ledger as a report reads it: opened only when an item is
/// linked into it, and then read one linked change at a time. A stage is what
/// the ledger last recorded for the change; nothing here judges the change
/// live, runs Git, or reads gate declarations.
struct Ledger {
    label: String,
    opener: Opener,
    store: std::cell::OnceCell<std::result::Result<Option<Store>, String>>,
    /// Each change's newest dispatch with no recorded end, gathered once
    /// from the repository events the change's reduction already read.
    dispatches: std::cell::OnceCell<BTreeMap<String, Dispatch>>,
}

impl Ledger {
    /// The ledger of the checkout `ctx` stands in.
    fn local(ctx: &Ctx) -> Self {
        let cwd = ctx.cwd.clone();
        Self::new(&ctx.cwd, move || {
            if crate::gitio::toplevel(&cwd).is_err() {
                return Ok(None);
            }
            Store::discover_existing_for_reading(&cwd)
        })
    }

    /// The ledger of the project at `anchor`, wherever the configuration
    /// keeps it.
    fn at(anchor: &Path) -> Self {
        let path = anchor.to_path_buf();
        Self::new(anchor, move || {
            if !path.is_dir() {
                return Ok(None);
            }
            let cfg = crate::config::load()?;
            Ok(crate::registry::ledger_at(&cfg, &path)?
                .map(|root| Store::open_at(&root))
                .transpose()?
                .flatten())
        })
    }

    /// A project with no ledger to read.
    fn none(place: &Path) -> Self {
        Self::new(place, || Ok(None))
    }

    fn new(place: &Path, opener: impl Fn() -> Result<Option<Store>> + 'static) -> Self {
        Self {
            label: place.display().to_string(),
            opener: Box::new(opener),
            store: std::cell::OnceCell::new(),
            dispatches: std::cell::OnceCell::new(),
        }
    }

    /// The store itself, for a caller that writes nothing through this read.
    fn opened(self) -> Result<Option<Store>> {
        (self.opener)()
    }

    fn store(&self) -> Result<Option<&Store>> {
        match self
            .store
            .get_or_init(|| (self.opener)().map_err(|error| format!("{error:#}")))
        {
            Ok(store) => Ok(store.as_ref()),
            Err(error) => bail!("cannot read the ledger of {}: {error}", self.label),
        }
    }

    /// One change's state, from that change's events and the records every
    /// reduction reads. `None` when the ledger holds no such change; a ledger
    /// that cannot be opened, or a change that cannot be reduced, is an error.
    fn state(&self, change: &str) -> Result<Option<ChangeState>> {
        if crate::ids::validate_id_component(change).is_err() {
            return Ok(None);
        }
        let Some(store) = self.store()? else {
            return Ok(None);
        };
        if !store.holds_change(change)? {
            return Ok(None);
        }
        store
            .state(change)
            .map(Some)
            .with_context(|| format!("cannot read change {change} in {}", self.label))
    }

    /// The newest run dispatched against `change` that has no recorded end.
    fn open_dispatch(&self, change: &str) -> Result<Option<Dispatch>> {
        let Some(store) = self.store()? else {
            return Ok(None);
        };
        if self.dispatches.get().is_none() {
            let events = store.repository_events()?;
            let _ = self.dispatches.set(open_dispatches(&events));
        }
        Ok(self
            .dispatches
            .get()
            .and_then(|open| open.get(change))
            .cloned())
    }

    /// The item as this ledger leaves it. An unlinked item's stage is what
    /// the series recorded; a linked one's is what the change last recorded.
    fn row(&self, item: &series::Item, now: DateTime<Utc>) -> Result<ItemRow> {
        let mut row = ItemRow::of(item);
        let Some(change) = item.change.as_deref() else {
            return Ok(row);
        };
        if !item.stands() {
            return Ok(row);
        }
        let Some(state) = self.state(change)? else {
            row.stage = "unknown";
            return Ok(row);
        };
        row.changes_requested_rounds = state
            .verdicts
            .iter()
            .filter(|verdict| verdict.verdict == Verdict::ChangesRequested)
            .count();
        row.held_by = state.claim.as_ref().and_then(|claim| {
            crate::state::claim_timing_at(claim, now)
                .active
                .then(|| format!("{} via {}", claim.owner.actor, claim.owner.harness))
        });
        row.stage = recorded_stage(&state, &mut row);
        if row.in_flight() {
            row.dispatch = self.open_dispatch(change)?;
        }
        Ok(row)
    }
}

/// Each change's newest dispatch that no recorded end closes, in the order
/// the repository events record them.
fn open_dispatches(events: &[crate::model::Event]) -> BTreeMap<String, Dispatch> {
    let mut open = BTreeMap::new();
    for round in super::run::rounds(events) {
        let Some(super::run::RunSubject::Change { id }) = &round.subject else {
            continue;
        };
        let crate::model::Payload::RunDispatched { route, .. } = &round.dispatch.payload else {
            continue;
        };
        if round.ending.is_some() {
            continue;
        }
        open.insert(
            id.clone(),
            Dispatch {
                to: round
                    .dispatch
                    .on_behalf_of
                    .clone()
                    .unwrap_or_else(|| route.clone()),
                route: route.clone(),
                at: round
                    .dispatch
                    .created_at
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
                bound: round.binding.is_some(),
            },
        );
    }
    open
}

/// A registered project as a report reads it.
struct Registered {
    slug: String,
    label: String,
    journal_dir: PathBuf,
    /// Opened only when an item links into the project or a related series
    /// of it is read.
    ledger: Ledger,
}

/// The registered projects one report reads through: the registry, read once
/// and only when an item or a relation names another project. Each project's
/// ledger opens at most once, when something read needs it.
#[derive(Default)]
struct Registry {
    projects: Vec<Registered>,
}

impl Registry {
    fn of(projects: Vec<crate::registry::Project>) -> Self {
        Self {
            projects: projects
                .into_iter()
                .map(|project| Registered {
                    label: project.label(),
                    ledger: match project.anchor.as_deref() {
                        Some(anchor) => Ledger::at(anchor),
                        None => Ledger::none(&project.journal_dir),
                    },
                    slug: project.slug,
                    journal_dir: project.journal_dir,
                })
                .collect(),
        }
    }

    /// The registry as the journals record it. One that cannot be read
    /// leaves every other project unread rather than failing the report.
    fn read() -> Result<Self> {
        let cfg = crate::config::load()?;
        Ok(Self::of(
            crate::registry::journal_projects(&cfg).unwrap_or_default(),
        ))
    }

    fn get(&self, slug: &str) -> Option<&Registered> {
        self.projects.iter().find(|project| project.slug == slug)
    }

    /// The slug the registry gives the journal at `dir`.
    fn slug_of(&self, dir: &Path) -> Option<&str> {
        self.projects
            .iter()
            .find(|project| same_dir(&project.journal_dir, dir))
            .map(|project| project.slug.as_str())
    }

    fn label(&self, slug: &str) -> String {
        self.get(slug)
            .map_or_else(|| slug.to_string(), |project| project.label.clone())
    }
}

/// Whether reading `series` needs the registry: an item linked into another
/// project, or a relation to one.
fn needs_registry(series: &[Series]) -> bool {
    series.iter().any(|series| {
        !series.related.is_empty() || series.items.iter().any(|item| item.project.is_some())
    })
}

/// The questions of one journal that wait on a person, found from events
/// already read and only when something asks for them.
struct PersonQuestions<'a> {
    dir: &'a Path,
    events: &'a [JournalEvent],
    found: std::cell::OnceCell<Vec<crate::journal::OpenQuestion>>,
}

impl<'a> PersonQuestions<'a> {
    fn new(dir: &'a Path, events: &'a [JournalEvent]) -> Self {
        Self {
            dir,
            events,
            found: std::cell::OnceCell::new(),
        }
    }

    fn all(&self) -> Result<&[crate::journal::OpenQuestion]> {
        if self.found.get().is_none() {
            let found = series::person_questions_in(self.dir, self.events)?;
            let _ = self.found.set(found);
        }
        Ok(self.found.get().map(Vec::as_slice).unwrap_or_default())
    }

    /// The wait as it stands: the question, while it is open and waits on a
    /// person.
    fn waiting(&self, wait: &ItemWait) -> Result<Option<Waiting>> {
        Ok(self
            .all()?
            .iter()
            .find(|open| open.file == wait.file && open.question == wait.question)
            .map(|open| Waiting {
                file: open.file.clone(),
                question: open.question.clone(),
                heading: open.heading.clone(),
            }))
    }
}

/// What one series' items are read through: the ledger of the project whose
/// series it is, the registry for an item linked into another project, and
/// the questions of its own journal.
struct Reader<'a> {
    home: &'a Ledger,
    home_slug: Option<&'a str>,
    registry: &'a Registry,
    questions: &'a PersonQuestions<'a>,
    now: DateTime<Utc>,
}

impl Reader<'_> {
    /// One item, read from the ledger that holds its change. A change of
    /// another project whose ledger is not registered or cannot be read is
    /// `unknown` and fails nothing.
    fn row(&self, item: &series::Item) -> Result<ItemRow> {
        let foreign = item
            .project
            .as_deref()
            .filter(|slug| Some(*slug) != self.home_slug);
        let mut row = match foreign {
            None => self.home.row(item, self.now)?,
            Some(slug) => match self
                .registry
                .get(slug)
                .map(|project| project.ledger.row(item, self.now))
            {
                Some(Ok(row)) => row,
                _ => {
                    let mut row = ItemRow::of(item);
                    if item.stands() {
                        row.stage = "unknown";
                    }
                    row
                }
            },
        };
        if let Some(slug) = &item.project {
            row.project = Some(self.registry.label(slug));
            row.project_slug = Some(slug.clone());
        }
        if item.stands() {
            if let Some(wait) = &item.waits_on {
                row.waiting = self.questions.waiting(wait)?;
            }
        }
        Ok(row)
    }

    fn rows(&self, series: &Series) -> Result<Vec<ItemRow>> {
        series.items.iter().map(|item| self.row(item)).collect()
    }
}

/// The furthest stage the change's own events record. Integration and
/// closure come first; then a verdict on the newest patchset; then a
/// recorded pass at it; then the patchset itself.
fn recorded_stage(state: &ChangeState, row: &mut ItemRow) -> &'static str {
    if let Some(closure) = &state.closure {
        if closure.outcome != Closure::Integrated {
            return "closed";
        }
        row.integrated_at = Some(
            closure
                .created_at
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        );
        row.revision = closure
            .landing
            .as_ref()
            .map(|landing| landing.target_after.clone())
            .or_else(|| closure.integrated_commit.clone());
        return "integrated";
    }
    let Some(patchset) = state.latest_patchset() else {
        return "opened";
    };
    if let Some(verdict) = state
        .latest_verdict()
        .filter(|verdict| verdict.patchset_id == patchset.id)
    {
        row.verdict = Some(match verdict.verdict {
            Verdict::Approved => "approved",
            Verdict::ChangesRequested => "changes-requested",
            Verdict::CommentOnly => "comment-only",
        });
        return "reviewed";
    }
    if verified_at(state, &patchset.head) {
        "verified"
    } else {
        "snapshotted"
    }
}

/// Whether the ledger records a passing verification of `head`: at least one
/// gate has a run recorded at it, and every gate's most recent run there is a
/// pass. A run that was not admitted ran nothing and is passed over; a
/// failure, or a bypass, recorded after a pass takes it back.
fn verified_at(state: &ChangeState, head: &str) -> bool {
    let mut latest: BTreeMap<&str, crate::model::VerifyResult> = BTreeMap::new();
    for entry in &state.verifications {
        let Some(gate) = entry.gate.as_deref() else {
            continue;
        };
        // The revision the run was recorded at: a pass carried to `head` by
        // a recorded rewrite is not a run at `head`.
        if entry.recorded_revision != head
            || entry.probe.is_some()
            || entry.result == crate::model::VerifyResult::NotAdmitted
        {
            continue;
        }
        latest.insert(gate, entry.result);
    }
    !latest.is_empty()
        && latest
            .values()
            .all(|result| *result == crate::model::VerifyResult::Pass)
}

fn counts(series: &Series, rows: &[ItemRow]) -> Counts {
    let mut counts = Counts {
        since_start: series.since_start(),
        ..Counts::default()
    };
    for (item, row) in series.items.iter().zip(rows) {
        if !item.stands() {
            continue;
        }
        counts.items += 1;
        if row.waiting.is_some() {
            counts.waiting += 1;
        }
        match row.stage {
            "integrated" => counts.integrated += 1,
            "done" => counts.done += 1,
            "closed" => counts.closed += 1,
            "open" => counts.open += 1,
            _ if row.in_flight() => counts.in_flight += 1,
            _ => counts.open += 1,
        }
    }
    counts.complete = counts.integrated + counts.done;
    counts.percent = (counts.items > 0).then(|| counts.complete * 100 / counts.items);
    counts
}

fn last_integration(rows: &[ItemRow]) -> Option<Integration> {
    rows.iter()
        .filter_map(|row| {
            row.integrated_at.as_ref().map(|at| Integration {
                item: row.id.clone(),
                at: at.clone(),
                revision: row.revision.clone(),
            })
        })
        .max_by(|left, right| left.at.cmp(&right.at))
}

fn series_report(
    series: &Series,
    reader: &Reader,
    relations: Vec<Relation>,
) -> Result<SeriesReport> {
    let rows = reader.rows(series)?;
    Ok(SeriesReport {
        file: series.file.clone(),
        title: series.title.clone(),
        follows: series.follows.clone(),
        started_at: series.started_at.clone(),
        retired: series.retired.clone(),
        related: relations,
        counts: counts(series, &rows),
        last_integration: last_integration(&rows),
        items: rows,
    })
}

/// One related series, read once from its own journal.
struct RelatedRead {
    slug: String,
    label: String,
    result: Result<(Series, Vec<JournalEvent>, PathBuf), String>,
}

fn read_related(registry: &Registry, relation: &RelatedSeries) -> RelatedRead {
    let project = registry.get(&relation.project);
    let result = (|| -> Result<_> {
        let dir = project
            .map(|project| project.journal_dir.clone())
            .with_context(|| format!("no journal is registered as {}", relation.project))?;
        let events = crate::journal::read_events(&dir)?;
        let series = series::replay(&events, &relation.file)
            .with_context(|| format!("{} holds no series {}", relation.project, relation.file))?;
        Ok((series, events, dir))
    })()
    .map_err(|error| format!("{error:#}").replace(['\n', '\r'], " "));
    RelatedRead {
        slug: relation.project.clone(),
        label: registry.label(&relation.project),
        result,
    }
}

fn related_report(
    registry: &Registry,
    read: RelatedRead,
    file: &str,
    from: Vec<String>,
    now: DateTime<Utc>,
) -> RelatedReport {
    let mut report = RelatedReport {
        project: read.label,
        slug: read.slug,
        file: file.to_string(),
        related_from: from,
        unavailable: None,
        title: None,
        retired: None,
        counts: None,
        last_integration: None,
        in_flight: Vec::new(),
    };
    match read.result {
        Err(reason) => report.unavailable = Some(reason),
        Ok((series, events, dir)) => {
            let none = Ledger::none(&dir);
            let home = registry
                .get(&report.slug)
                .map_or(&none, |project| &project.ledger);
            let questions = PersonQuestions::new(&dir, &events);
            let reader = Reader {
                home,
                home_slug: Some(&report.slug),
                registry,
                questions: &questions,
                now,
            };
            // A related series is read on the way, never relied on: a ledger
            // that cannot be read makes it unavailable, not the report.
            let rows = match reader.rows(&series) {
                Ok(rows) => rows,
                Err(error) => {
                    report.unavailable = Some(format!("{error:#}").replace(['\n', '\r'], " "));
                    return report;
                }
            };
            report.title = Some(series.title.clone());
            report.retired = series.retired.clone();
            report.counts = Some(counts(&series, &rows));
            report.last_integration = last_integration(&rows);
            report.in_flight = rows.into_iter().filter(ItemRow::in_flight).collect();
        }
    }
    report
}

/// Build the report for this project: one series by name, or every active
/// one, with the related series they name.
fn build(ctx: &Ctx, name: Option<&str>) -> Result<ProgressReport> {
    let now = Utc::now();
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    let events = crate::journal::read_events(&dir)?;
    let chosen = match name {
        Some(name) => vec![series::named_in(&events, &dir, name)?],
        None => series::active_in(&events),
    };
    let ledger = Ledger::local(ctx);
    let registry = if needs_registry(&chosen) {
        Registry::read()?
    } else {
        Registry::default()
    };
    let own = registry.slug_of(&dir).map(str::to_string);
    let questions = PersonQuestions::new(&dir, &events);
    let reader = Reader {
        home: &ledger,
        home_slug: own.as_deref(),
        registry: &registry,
        questions: &questions,
        now,
    };
    let mut reads: Vec<(RelatedSeries, RelatedRead, Vec<String>)> = Vec::new();
    let mut reports = Vec::new();
    for series in &chosen {
        let mut relations = Vec::new();
        for relation in &series.related {
            let index = match reads.iter().position(|(known, _, _)| known == relation) {
                Some(index) => index,
                None => {
                    reads.push((
                        relation.clone(),
                        read_related(&registry, relation),
                        Vec::new(),
                    ));
                    reads.len() - 1
                }
            };
            let (_, read, from) = &mut reads[index];
            from.push(series.file.clone());
            let mutual = read.result.as_ref().ok().map(|(other, _, _)| {
                own.as_deref().is_some_and(|slug| {
                    other
                        .related
                        .iter()
                        .any(|back| back.project == slug && back.file == series.file)
                })
            });
            relations.push(Relation {
                project: read.label.clone(),
                slug: relation.project.clone(),
                file: relation.file.clone(),
                mutual,
            });
        }
        reports.push(series_report(series, &reader, relations)?);
    }
    let related = reads
        .into_iter()
        .map(|(relation, read, from)| related_report(&registry, read, &relation.file, from, now))
        .collect();
    let waiting_on_person = questions
        .all()?
        .iter()
        .map(|question| WaitingQuestion {
            file: question.file.clone(),
            question: question.question.clone(),
            heading: question.heading.clone(),
            asked_at: question.asked_at.clone(),
        })
        .collect();
    Ok(ProgressReport {
        schema: PROGRESS_SCHEMA,
        journal_dir: dir.display().to_string(),
        series: reports,
        related,
        waiting_on_person,
    })
}

fn report(ctx: &Ctx, name: Option<&str>, form: Form) -> Result<i32> {
    let report = build(ctx, name)?;
    match form {
        Form::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        Form::Plain => render_report(&report),
        Form::Terminal => {
            let mut view = view::View::new(crate::terminal::Look::of_stdout(), Utc::now());
            view.project(&report);
            println!("{}", view.finish());
        }
    }
    Ok(0)
}

fn render_report(report: &ProgressReport) {
    if report.series.is_empty() {
        println!("no active progress series; start one with `arc progress start <topic> --title <title>`");
    }
    for (index, series) in report.series.iter().enumerate() {
        if index > 0 {
            println!();
        }
        render_series(series);
    }
    for related in &report.related {
        println!();
        render_related(related);
    }
    if !report.waiting_on_person.is_empty() {
        println!();
        println!("waiting on a person ({}):", report.waiting_on_person.len());
        for question in &report.waiting_on_person {
            println!(
                "  {} {}{}",
                question.file,
                question.question,
                question
                    .heading
                    .as_deref()
                    .map(|heading| format!(": {heading}"))
                    .unwrap_or_default()
            );
        }
    }
}

fn render_series(series: &SeriesReport) {
    println!("== {} ({})", series.title, series.file);
    if !series.related.is_empty() {
        let named: Vec<String> = series
            .related
            .iter()
            .map(|relation| {
                format!(
                    "{}::{} ({})",
                    relation.project,
                    relation.file,
                    match relation.mutual {
                        Some(true) => "mutual",
                        Some(false) => "one-sided",
                        None => "unread",
                    }
                )
            })
            .collect();
        println!("related: {}", named.join(", "));
    }
    if let Some(plan) = &series.follows {
        println!("follows: {plan}");
    }
    if let Some(retired) = &series.retired {
        println!(
            "retired {}{}",
            retired.at,
            retired
                .by
                .as_deref()
                .map(|by| format!(" by {by}"))
                .unwrap_or_default()
        );
    }
    let width = series
        .items
        .iter()
        .map(|row| row.stage.len())
        .max()
        .unwrap_or(0);
    for row in &series.items {
        render_row(row, width);
    }
    render_counts(&series.counts, true);
    render_last_integration(series.last_integration.as_ref());
}

fn render_row(row: &ItemRow, width: usize) {
    let mut detail = Vec::new();
    if let Some(change) = &row.change {
        match (&row.project, &row.project_slug) {
            (Some(project), Some(slug)) => {
                detail.push(format!("change {project}::{change} (project {slug})"))
            }
            _ => detail.push(format!("change {change}")),
        }
    }
    if row.changes_requested_rounds > 0 {
        detail.push(format!(
            "{} round{} asked for changes",
            row.changes_requested_rounds,
            if row.changes_requested_rounds == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if let Some(holder) = &row.held_by {
        detail.push(format!("held by {holder}"));
    }
    if let Some(verdict) = row.verdict {
        detail.push(verdict.to_string());
    }
    if let Some(at) = &row.integrated_at {
        detail.push(format!(
            "integrated {at}{}",
            row.revision
                .as_deref()
                .map(|revision| format!(" at {}", &revision[..revision.len().min(12)]))
                .unwrap_or_default()
        ));
    }
    if let Some(dispatch) = &row.dispatch {
        detail.push(format!(
            "dispatched to {} at {}{}{}",
            dispatch.to,
            dispatch.at,
            if dispatch.to == dispatch.route {
                String::new()
            } else {
                format!(" via route {}", dispatch.route)
            },
            if dispatch.bound { "" } else { ", unbound" }
        ));
    }
    if let Some(waiting) = &row.waiting {
        detail.push(format!(
            "waiting on {} {}{}",
            waiting.file,
            waiting.question,
            waiting
                .heading
                .as_deref()
                .map(|heading| format!(": {heading}"))
                .unwrap_or_default()
        ));
    }
    if !row.split_into.is_empty() {
        detail.push(format!("into {}", row.split_into.join(", ")));
    }
    if let Some(note) = &row.note {
        detail.push(note.clone());
    }
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("  [{}]", detail.join("; "))
    };
    println!(
        "  {:<4} {:<width$}  {}{detail}",
        row.id, row.stage, row.title
    );
}

fn render_counts(counts: &Counts, since: bool) {
    let mut line = format!(
        "  {} of {} complete ({} integrated, {} done)",
        counts.complete, counts.items, counts.integrated, counts.done
    );
    if let Some(percent) = counts.percent {
        line.push_str(&format!(", {percent}% by count of items"));
    }
    line.push_str(&format!("; {} in flight", counts.in_flight));
    if counts.waiting > 0 {
        line.push_str(&format!("; {} waiting", counts.waiting));
    }
    line.push_str(&format!("; {} open", counts.open));
    if counts.closed > 0 {
        line.push_str(&format!("; {} closed without integrating", counts.closed));
    }
    println!("{line}");
    if since {
        let start = counts.since_start;
        println!(
            "  {} items; {} declared at the start; since then {} added, {} dropped, {} split",
            counts.items, start.declared, start.added, start.dropped, start.split
        );
    }
}

fn render_last_integration(landing: Option<&Integration>) {
    match landing {
        Some(landing) => println!(
            "  last integrated: {} at {}{}",
            landing.item,
            landing.at,
            landing
                .revision
                .as_deref()
                .map(|revision| format!(", {}", &revision[..revision.len().min(12)]))
                .unwrap_or_default()
        ),
        None => println!("  last integrated: none"),
    }
}

fn render_related(related: &RelatedReport) {
    let Some(counts) = &related.counts else {
        println!(
            "related {}::{}: unavailable ({})",
            related.project,
            related.file,
            related.unavailable.as_deref().unwrap_or("unread")
        );
        return;
    };
    println!(
        "-- {}: {} ({}){}",
        related.project,
        related.title.as_deref().unwrap_or_default(),
        related.file,
        if related.retired.is_some() {
            ", retired"
        } else {
            ""
        }
    );
    render_counts(counts, false);
    render_last_integration(related.last_integration.as_ref());
    let width = related
        .in_flight
        .iter()
        .map(|row| row.stage.len())
        .max()
        .unwrap_or(0);
    for row in &related.in_flight {
        render_row(row, width);
    }
}

// --- Workspace -----------------------------------------------------------

#[derive(Serialize)]
struct ProjectProgress {
    project: String,
    slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor: Option<String>,
    series: Vec<SeriesReport>,
}

#[derive(Serialize)]
struct ProgressFailure {
    project: String,
    reason: String,
}

/// One project's active series as the workspace reports them: each in full
/// from the project's own journal and ledger, its relations named without
/// reading the related series, so whether one is mutual is left absent.
pub(crate) fn project_series(
    journal_dir: &Path,
    anchor: Option<&Path>,
    now: DateTime<Utc>,
) -> Result<Vec<SeriesReport>> {
    series_of(journal_dir, anchor, None, now)
}

/// [`project_series`] through `shared`, the registry the caller already
/// read with this project's slug in it. Without it the registry is read, and
/// only when an item or a relation names another project.
fn series_of(
    journal_dir: &Path,
    anchor: Option<&Path>,
    shared: Option<(&Registry, &str)>,
    now: DateTime<Utc>,
) -> Result<Vec<SeriesReport>> {
    let events = crate::journal::read_events(journal_dir)?;
    let active = series::active_in(&events);
    if active.is_empty() {
        return Ok(Vec::new());
    }
    let read;
    let (registry, home_slug) = match shared {
        Some((registry, slug)) => (registry, Some(slug.to_string())),
        None => {
            read = if needs_registry(&active) {
                Registry::read()?
            } else {
                Registry::default()
            };
            let slug = read.slug_of(journal_dir).map(str::to_string);
            (&read, slug)
        }
    };
    let own;
    let home = match home_slug.as_deref().and_then(|slug| registry.get(slug)) {
        Some(project) => &project.ledger,
        None => {
            own = match anchor {
                Some(anchor) => Ledger::at(anchor),
                None => Ledger::none(journal_dir),
            };
            &own
        }
    };
    let questions = PersonQuestions::new(journal_dir, &events);
    let reader = Reader {
        home,
        home_slug: home_slug.as_deref(),
        registry,
        questions: &questions,
        now,
    };
    active
        .iter()
        .map(|series| {
            let relations = series
                .related
                .iter()
                .map(|relation| Relation {
                    project: registry.label(&relation.project),
                    slug: relation.project.clone(),
                    file: relation.file.clone(),
                    mutual: None,
                })
                .collect();
            series_report(series, &reader, relations)
        })
        .collect()
}

/// Fill each relation's `mutual` from the series this command read: `true`
/// when the related series relates back, `false` when it was read and does
/// not, and absent when it was not read here.
fn settle_mutual(projects: &mut [ProjectProgress]) {
    let read: std::collections::BTreeSet<(String, String)> = projects
        .iter()
        .flat_map(|project| {
            project
                .series
                .iter()
                .map(|series| (project.slug.clone(), series.file.clone()))
        })
        .collect();
    let relates: std::collections::BTreeSet<(String, String, String, String)> = projects
        .iter()
        .flat_map(|project| {
            project.series.iter().flat_map(move |series| {
                series.related.iter().map(move |relation| {
                    (
                        project.slug.clone(),
                        series.file.clone(),
                        relation.slug.clone(),
                        relation.file.clone(),
                    )
                })
            })
        })
        .collect();
    for project in projects.iter_mut() {
        for series in &mut project.series {
            for relation in &mut series.related {
                relation.mutual = read
                    .contains(&(relation.slug.clone(), relation.file.clone()))
                    .then(|| {
                        relates.contains(&(
                            relation.slug.clone(),
                            relation.file.clone(),
                            project.slug.clone(),
                            series.file.clone(),
                        ))
                    });
            }
        }
    }
}

/// Whether any item of `series`, as [`project_series`] reports them, follows
/// a change of one of `projects`, named by registry slug.
pub(crate) fn links_into(
    series: &serde_json::Value,
    projects: &std::collections::BTreeSet<&str>,
) -> bool {
    !projects.is_empty()
        && series.as_array().into_iter().flatten().any(|series| {
            series["items"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| {
                    item["project_slug"]
                        .as_str()
                        .is_some_and(|slug| projects.contains(slug))
                })
        })
}

/// `arc workspace progress`: every registered project's active series.
pub(crate) fn workspace(
    includes: impl Fn(Option<&Path>) -> bool,
    scope: impl Serialize,
    form: Form,
) -> Result<i32> {
    let now = Utc::now();
    let cfg = crate::config::load()?;
    let mut projects = Vec::new();
    let mut failures = Vec::new();
    let registered = crate::registry::journal_projects(&cfg)?;
    let registry = Registry::of(registered.clone());
    for project in registered {
        if project.retired.is_some() || !includes(project.anchor.as_deref()) {
            continue;
        }
        let read = series_of(
            &project.journal_dir,
            project.anchor.as_deref(),
            Some((&registry, &project.slug)),
            now,
        );
        match read {
            Ok(series) if series.is_empty() => {}
            Ok(series) => projects.push(ProjectProgress {
                project: project.label(),
                slug: project.slug.clone(),
                anchor: project
                    .anchor
                    .as_ref()
                    .map(|path| path.display().to_string()),
                series,
            }),
            Err(error) => failures.push(ProgressFailure {
                project: project.label(),
                reason: format!("{error:#}"),
            }),
        }
    }
    settle_mutual(&mut projects);
    match form {
        Form::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": WORKSPACE_PROGRESS_SCHEMA,
                "scope": scope,
                "projects": projects,
                "failures": failures,
            }))?
        ),
        Form::Plain => {
            if projects.is_empty() {
                println!("no active progress series");
            }
            for (index, project) in projects.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                println!("{} ({}):", project.project, project.series.len());
                for series in &project.series {
                    render_series(series);
                }
            }
        }
        Form::Terminal => {
            let mut view = view::View::new(crate::terminal::Look::of_stdout(), now);
            view.workspace(&projects);
            println!("{}", view.finish());
        }
    }
    for failure in &failures {
        eprintln!(
            "{}: unavailable ({})",
            failure.project,
            failure.reason.replace(['\n', '\r'], " ")
        );
    }
    Ok(if failures.is_empty() { 0 } else { 16 })
}
