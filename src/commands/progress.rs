//! `arc progress`: where each series of work stands, read on request.
//!
//! The list of items is the journal's (see `journal::series`). Everything
//! else a report shows is what the ledger has recorded when it is asked: a
//! linked item's stage, who holds its change, and when it was integrated. A
//! stage is a record, never a live judgement; `arc status` is the live
//! reading. Nothing here runs unless a caller asks for it.

use super::Ctx;
use crate::journal::series::{self, ItemLink, ItemSource, Placement, RelatedSeries, ResolvedBy};
use crate::model::Closure;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) mod reader;
mod view;

use reader::{build, own_slug, same_dir, series_of, settle_mutual, Ledger, Registry};
pub(crate) use reader::{
    links_into, project_series, Counts, Integration, ItemRow, ProgressFailure, ProgressReport,
    ProjectProgress, RelatedReport, SeriesReport, WaitingQuestion,
};

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
        /// Bind a declared item to its originating journal artifact
        /// (`<item>=<file>` such as `i1=2026...todo.md`, or `<file>` when one
        /// item is declared; cross-project as `<project>::<file>`)
        #[arg(long = "from")]
        from: Vec<String>,
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
        /// Originating journal artifact (`todo`, `feature-request`, `plan`, or
        /// `handoff`), bare filename in this journal or `<project>::<file>`
        #[arg(long)]
        from: Option<String>,
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
    /// unlink it first. Optional `--by` cites the resolving conclusion or
    /// decision artifact (bare filename in this journal) or recorded
    /// checkpoint name.
    Done {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Resolving conclusion or decision filename of this journal, or a
        /// recorded checkpoint name
        #[arg(long)]
        by: Option<String>,
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
    /// Bind an item to its originating journal artifact (`todo`,
    /// `feature-request`, `plan`, or `handoff`)
    Source {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Bare artifact filename of this journal, or `<project>::<file>` for
        /// another registered project
        artifact: String,
    },
    /// Remove an item's originating journal artifact link
    Unsource {
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
            from,
        } => {
            let sources = parse_start_sources(&from, items.len())?;
            let file = series::start(ctx, &topic, &title, follows.as_deref(), &items, &sources)?;
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
            from,
        } => {
            let id = series::add(
                ctx,
                &name,
                &title,
                placement(&before, &after),
                from.as_deref(),
            )?;
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
            by,
            note,
        } => {
            let resolved_by = by
                .as_deref()
                .map(|target| resolve_done_by(ctx, target))
                .transpose()?;
            series::done(ctx, &name, &item, resolved_by.clone(), note.as_deref())?;
            match resolved_by {
                Some(by) => println!("done {item} by {by}"),
                None => println!("done {item}"),
            }
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
        ProgressCmd::Source {
            series: name,
            item,
            artifact,
        } => {
            let source = series::set_source(ctx, &name, &item, &artifact)?;
            println!("sourced {item} from {source}");
        }
        ProgressCmd::Unsource { series: name, item } => {
            let source = series::clear_source(ctx, &name, &item)?;
            println!("unsourced {item} from {source}");
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

fn parse_start_sources(specs: &[String], item_count: usize) -> Result<BTreeMap<usize, String>> {
    let mut mapped = BTreeMap::new();
    for spec in specs {
        let (index, artifact) = if let Some((id, file)) = spec.split_once('=') {
            let number: usize = id
                .strip_prefix('i')
                .and_then(|digits| digits.parse().ok())
                .filter(|&n| n >= 1)
                .with_context(|| {
                    format!("--from takes <item>=<file> (such as i1=2026...todo.md); got {spec:?}")
                })?;
            (number - 1, file.to_string())
        } else if item_count == 1 {
            (0, spec.clone())
        } else {
            bail!(
                "--from takes <item>=<file> when start declares more than one item; got {spec:?}"
            );
        };
        if mapped.insert(index, artifact).is_some() {
            bail!("--from names item i{} more than once", index + 1);
        }
    }
    Ok(mapped)
}

fn resolve_done_by(ctx: &Ctx, target: &str) -> Result<ResolvedBy> {
    let dir = crate::journal::resolve_dir(&ctx.cwd)?;
    if let Some(artifact) = series::resolve_journal_resolution(&dir, target, "--by")? {
        return Ok(artifact);
    }
    let store = Store::discover_existing_for_reading(&ctx.cwd)?
        .with_context(|| format!("--by {target:?} is neither a conclusion/decision filename nor a checkpoint (no ledger here)"))?;
    let checkpoint = super::checkpoint::find(&store, target).with_context(|| {
        format!(
            "--by takes a bare conclusion or decision filename of this journal, or a recorded checkpoint name; {target:?} matched neither"
        )
    })?;
    Ok(ResolvedBy {
        kind: "checkpoint".to_string(),
        target: checkpoint.name,
    })
}

/// An item `begin --item` or `progress link` will link, checked before
/// anything is written. `expected` is the link read here; the write compares
/// it under the journal's lock.
pub(crate) struct ItemTarget {
    series: String,
    item: String,
    expected: Option<ItemLink>,
    source: Option<ItemSource>,
    replaceable: bool,
}

impl ItemTarget {
    pub(crate) fn source_spec(&self) -> Option<&ItemSource> {
        self.source.as_ref()
    }
}

/// Format an item's recorded `--from` source as a `--from-journal` reference
/// (`<file>` for this journal, or `<journal-dir>::<file>` for a registered
/// foreign project; `None` when a foreign project is no longer registered).
pub(crate) fn item_source_journal_ref(source: &ItemSource) -> Result<Option<String>> {
    let Some(slug) = source.project.as_deref() else {
        return Ok(Some(source.file.clone()));
    };
    let registry = Registry::read()?;
    Ok(registry
        .get(slug)
        .map(|project| format!("{}::{}", project.journal_dir.display(), source.file)))
}

/// Check an item for a link. Only the journal is read, and a ledger only
/// when the item already carries a link: the one holding that link's
/// change, through discovery that creates nothing, so a refused item leaves
/// no ledger behind.
fn preflight_item(ctx: &Ctx, name: &str, item: &str) -> Result<ItemTarget> {
    let (file, current, source) = series::linkable(ctx, name, item)?;
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
        source,
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

// --- Reporting -----------------------------------------------------------

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
    if let Some(claim) = &row.claimed {
        detail.push(format!(
            "claimed by {} at {} via {}{}",
            claim.owner,
            claim.at,
            claim.via,
            claim
                .lane
                .as_deref()
                .map(|lane| format!(" ({lane})"))
                .unwrap_or_default()
        ));
    }
    if let Some(by) = &row.resolved_by {
        detail.push(format!("by {by}"));
    }
    if let Some(source) = &row.source {
        let named = match (&source.project, &source.project_slug) {
            (Some(project), Some(slug)) => {
                format!("{project}::{} (project {slug})", source.file)
            }
            _ => source.file.clone(),
        };
        let drift = if source.drift {
            ", drift: source still open"
        } else {
            ""
        };
        detail.push(format!("from {named} ({}{drift})", source.status));
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
    if counts.claimed > 0 {
        line.push_str(&format!("; {} claimed", counts.claimed));
    }
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
