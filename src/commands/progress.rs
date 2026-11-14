//! `arc progress`: where each series of work stands, read on request.
//!
//! The list of items is the journal's (see `journal::series`). Everything
//! else a report shows is read from the ledger at the moment it is asked
//! for: a linked item's stage, who holds its change, and when it landed.
//! Nothing here runs unless a caller asks; no other command prints it.

use super::Ctx;
use crate::journal::series::{self, ItemState, Placement, RelatedSeries, Series, SinceStart};
use crate::model::{Closure, Verdict};
use crate::state::ChangeState;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// Emit structured JSON (arc-progress/1) instead of text
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
pub enum ProgressCmd {
    /// Start a series: write a `progress` artifact and declare its items
    ///
    /// Prints the series' filename, then one line per declared item with its
    /// id. Items declared here are the start that later edits are counted
    /// from. A series needs no plan; `--follows` records the plan it
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
    /// Link an item to a change of this project's ledger
    ///
    /// The item's stage then follows the change. An item already linked
    /// takes a new link only when its change closed without integrating, and
    /// only while its link is still the one this command read.
    /// `arc begin <slug> --item <series>:<item>` links at the moment a change
    /// is opened.
    Link {
        /// Series filename or topic
        series: String,
        /// Item id, such as i3
        item: String,
        /// Change ID or unique prefix
        change: String,
    },
    /// Remove an item's link to its change
    Unlink {
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
    /// stays readable by name, and refuses further writes.
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
        return report(ctx, args.series.as_deref(), args.json);
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
            let change = ctx.store()?.resolve_change(&change)?;
            series::link(
                ctx,
                &target.series,
                &target.item,
                &change,
                target.expected.as_deref(),
                target.replaceable,
            )?;
            println!("linked {item} to {change}");
        }
        ProgressCmd::Unlink { series: name, item } => {
            let change = series::unlink(ctx, &name, &item)?;
            println!("unlinked {item} from {change}");
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
    expected: Option<String>,
    replaceable: bool,
}

/// Check an item for a link. Only the journal is read, and the ledger only
/// when the item already carries a link, through discovery that creates
/// nothing: a refused item leaves no ledger behind.
fn preflight_item(ctx: &Ctx, name: &str, item: &str) -> Result<ItemTarget> {
    let (file, current) = series::linkable(ctx, name, item)?;
    let replaceable = match current.as_deref() {
        None => true,
        Some(change) => Store::discover_existing_for_reading(&ctx.cwd)?
            .and_then(|store| store.state(change).ok())
            .and_then(|state| state.closure)
            .is_some_and(|closure| closure.outcome != Closure::Integrated),
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
    series::link(
        ctx,
        &target.series,
        &target.item,
        change_id,
        target.expected.as_deref(),
        target.replaceable,
    )?;
    println!("item: {}:{}", target.series, target.item);
    Ok(())
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
/// the recorded slug, or the slug a registered project of that label or slug
/// has; the series matches the recorded filename or its topic. Nothing in the
/// other project is read, so a relation to a project that is gone can still
/// be removed.
fn recorded_relation(ctx: &Ctx, name: &str, target: &str) -> Result<RelatedSeries> {
    let Some((prefix, other)) = target.split_once("::") else {
        bail!("unrelate takes <project>::<series>; got {target:?}");
    };
    let here = crate::journal::resolve_dir(&ctx.cwd)?;
    let local = series::named_series(&here, name)?;
    let registered = crate::journal::registered_project(prefix)
        .ok()
        .map(|project| project.slug);
    let matches: Vec<&RelatedSeries> = local
        .related
        .iter()
        .filter(|relation| {
            relation.project == prefix || registered.as_deref() == Some(relation.project.as_str())
        })
        .filter(|relation| {
            relation.file == other
                || crate::journal::parse_artifact_name(&relation.file)
                    .is_some_and(|(_, topic, _)| topic == other)
        })
        .collect();
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
    /// One word. Unlinked: `open`, `done`, `dropped` or `split`. Linked:
    /// `opened`, `committed`, `snapshotted`, `gates-green`,
    /// `changes-requested`, `approved`, `commented`, `landed`, or `closed`
    /// for a change closed without integrating; `unknown` when this
    /// project's ledger has no such change.
    stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    change: Option<String>,
    /// Who holds a live claim on the change.
    #[serde(skip_serializing_if = "Option::is_none")]
    held_by: Option<String>,
    /// Review rounds on the change that asked for changes.
    #[serde(skip_serializing_if = "is_zero")]
    changes_requested_rounds: usize,
    /// When the change landed, and the revision it landed as.
    #[serde(skip_serializing_if = "Option::is_none")]
    landed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    /// The reason or note recorded with a done, dropped, split or reopened item.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    split_into: Vec<String>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

impl ItemRow {
    fn in_flight(&self) -> bool {
        self.change.is_some() && !matches!(self.stage, "landed" | "closed" | "dropped" | "split")
    }
}

/// The list counted as it stands.
#[derive(Serialize, Default)]
struct Counts {
    /// Items that stand: every item but the dropped and the split.
    items: usize,
    /// Landed and done items, the share's numerator.
    complete: usize,
    landed: usize,
    done: usize,
    in_flight: usize,
    /// Standing items no change carries and nobody marked done.
    open: usize,
    /// Linked items whose change closed without integrating.
    closed: usize,
    /// `complete` over `items`, rounded down; absent with no items.
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<usize>,
    since_start: SinceStart,
}

#[derive(Serialize, Clone)]
struct Landing {
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
    /// Whether the other series relates back; absent when its journal could
    /// not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    mutual: Option<bool>,
}

#[derive(Serialize)]
struct SeriesReport {
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
    last_landing: Option<Landing>,
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
    last_landing: Option<Landing>,
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
/// linked into it, and then read one linked change at a time, so what the
/// report costs follows its linked items and not the size of the ledger.
struct Ledger {
    ctx: Ctx,
    opener: Opener,
    store: std::cell::OnceCell<Option<Store>>,
}

impl Ledger {
    /// The ledger of the checkout `ctx` stands in.
    fn local(ctx: &Ctx) -> Self {
        let anchor = crate::gitio::toplevel(&ctx.cwd).unwrap_or_else(|_| ctx.cwd.clone());
        let cwd = anchor.clone();
        Self::new(ctx, &anchor, move || {
            if crate::gitio::toplevel(&cwd).is_err() {
                return Ok(None);
            }
            Store::discover_existing_for_reading(&cwd)
        })
    }

    /// The ledger of the project at `anchor`, wherever the configuration
    /// keeps it.
    fn at(ctx: &Ctx, anchor: &Path) -> Self {
        let path = anchor.to_path_buf();
        Self::new(ctx, anchor, move || {
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

    fn new(ctx: &Ctx, anchor: &Path, opener: impl Fn() -> Result<Option<Store>> + 'static) -> Self {
        Self {
            ctx: ctx.with_cwd(anchor.to_path_buf()),
            opener: Box::new(opener),
            store: std::cell::OnceCell::new(),
        }
    }

    fn store(&self) -> Option<&Store> {
        self.store
            .get_or_init(|| {
                (self.opener)().unwrap_or_else(|error| {
                    eprintln!(
                        "warning: cannot read the ledger of {}: {error:#}",
                        self.ctx.cwd.display()
                    );
                    None
                })
            })
            .as_ref()
    }

    /// One change's state, from that change's own events and the
    /// repository-wide records every reduction reads.
    fn state(&self, change: &str) -> Option<ChangeState> {
        self.store()?.state(change).ok()
    }

    /// Whether every gate the change answers to is green at its newest
    /// patchset. Requirements that cannot be read are not green: a target
    /// whose declarations are unreadable leaves them unknown, and unknown
    /// is never read as none.
    fn gates_green(&self, state: &ChangeState) -> bool {
        let Ok(declarations) = crate::declarations::for_change(&self.ctx.cwd, state) else {
            return false;
        };
        if declarations.target_unreadable {
            return false;
        }
        let dependencies = crate::status::BlockerStatus {
            schema: crate::status::BLOCKER_STATUS_SCHEMA,
            blocked: false,
            blockers_ready: Vec::new(),
        };
        crate::status::build_as_of(
            state,
            &declarations.gates,
            &declarations.policy,
            false,
            dependencies,
            Vec::new(),
            Utc::now(),
            Some(&self.ctx.cwd),
            None,
        )
        .is_ok_and(|report| report.gates.iter().all(|gate| gate.green_at_head))
    }

    /// The item as the ledger leaves it. An unlinked item's stage is what
    /// the series recorded; a linked one's is derived from its change.
    fn row(&self, item: &series::Item, now: DateTime<Utc>) -> ItemRow {
        let mut row = ItemRow {
            id: item.id.clone(),
            title: item.title.clone(),
            stage: match item.state {
                ItemState::Open => "open",
                ItemState::Done => "done",
                ItemState::Dropped => "dropped",
                ItemState::Split => "split",
            },
            change: item.change.clone(),
            held_by: None,
            changes_requested_rounds: 0,
            landed_at: None,
            revision: None,
            note: item.note.clone(),
            split_into: item.split_into.clone(),
        };
        let Some(change) = item.change.as_deref() else {
            return row;
        };
        if !item.stands() {
            return row;
        }
        let Some(state) = self.state(change) else {
            row.stage = "unknown";
            return row;
        };
        let state = &state;
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
        row.stage = self.stage(state, &mut row);
        row
    }

    fn stage(&self, state: &ChangeState, row: &mut ItemRow) -> &'static str {
        if let Some(closure) = &state.closure {
            if closure.outcome != Closure::Integrated {
                return "closed";
            }
            row.landed_at = Some(
                closure
                    .created_at
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            );
            row.revision = closure
                .landing
                .as_ref()
                .map(|landing| landing.target_after.clone())
                .or_else(|| closure.integrated_commit.clone());
            return "landed";
        }
        let Some(patchset) = state.latest_patchset() else {
            // The branch tip is one answer of the shared object reader, not
            // a Git process per item.
            let moved = crate::gitio::branch_head(&self.ctx.cwd, &state.branch)
                .is_ok_and(|head| head != state.base);
            return if moved { "committed" } else { "opened" };
        };
        if let Some(verdict) = state
            .latest_verdict()
            .filter(|verdict| verdict.patchset_id == patchset.id)
        {
            return match verdict.verdict {
                Verdict::Approved => "approved",
                Verdict::ChangesRequested => "changes-requested",
                Verdict::CommentOnly => "commented",
            };
        }
        if self.gates_green(state) {
            "gates-green"
        } else {
            "snapshotted"
        }
    }

    fn rows(&self, series: &Series, now: DateTime<Utc>) -> Vec<ItemRow> {
        series
            .items
            .iter()
            .map(|item| self.row(item, now))
            .collect()
    }
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
        match row.stage {
            "landed" => counts.landed += 1,
            "done" => counts.done += 1,
            "closed" => counts.closed += 1,
            "open" => counts.open += 1,
            _ if row.in_flight() => counts.in_flight += 1,
            _ => counts.open += 1,
        }
    }
    counts.complete = counts.landed + counts.done;
    counts.percent = (counts.items > 0).then(|| counts.complete * 100 / counts.items);
    counts
}

fn last_landing(rows: &[ItemRow]) -> Option<Landing> {
    rows.iter()
        .filter_map(|row| {
            row.landed_at.as_ref().map(|at| Landing {
                item: row.id.clone(),
                at: at.clone(),
                revision: row.revision.clone(),
            })
        })
        .max_by(|left, right| left.at.cmp(&right.at))
}

fn series_report(
    series: &Series,
    ledger: &Ledger,
    relations: Vec<Relation>,
    now: DateTime<Utc>,
) -> SeriesReport {
    let rows = ledger.rows(series, now);
    SeriesReport {
        file: series.file.clone(),
        title: series.title.clone(),
        follows: series.follows.clone(),
        started_at: series.started_at.clone(),
        retired: series.retired.clone(),
        related: relations,
        counts: counts(series, &rows),
        last_landing: last_landing(&rows),
        items: rows,
    }
}

/// One related series, read once from its own journal and ledger.
struct RelatedRead {
    slug: String,
    label: String,
    result: Result<(Series, Option<PathBuf>), String>,
}

fn read_related(journals: &[(String, PathBuf)], relation: &RelatedSeries) -> RelatedRead {
    let journal_dir = journals
        .iter()
        .find(|(slug, _)| slug == &relation.project)
        .map(|(_, dir)| dir.clone());
    let anchor = journal_dir
        .as_deref()
        .and_then(|dir| crate::journal::recorded_anchor(dir).ok().flatten())
        .map(PathBuf::from);
    let label = anchor
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| relation.project.clone());
    let result = (|| -> Result<_> {
        let dir = journal_dir
            .as_deref()
            .with_context(|| format!("no journal is registered as {}", relation.project))?;
        let events = crate::journal::read_events(dir)?;
        let series = series::replay(&events, &relation.file)
            .with_context(|| format!("{} holds no series {}", relation.project, relation.file))?;
        Ok((series, anchor.clone()))
    })()
    .map_err(|error| format!("{error:#}").replace(['\n', '\r'], " "));
    RelatedRead {
        slug: relation.project.clone(),
        label,
        result,
    }
}

fn related_report(
    ctx: &Ctx,
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
        last_landing: None,
        in_flight: Vec::new(),
    };
    match read.result {
        Err(reason) => report.unavailable = Some(reason),
        Ok((series, anchor)) => {
            let rows = match anchor.as_deref() {
                Some(anchor) => Ledger::at(ctx, anchor).rows(&series, now),
                None => Ledger::new(ctx, &ctx.cwd, || Ok(None)).rows(&series, now),
            };
            report.title = Some(series.title.clone());
            report.retired = series.retired.clone();
            report.counts = Some(counts(&series, &rows));
            report.last_landing = last_landing(&rows);
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
    let chosen = match name {
        Some(name) => vec![series::named_series(&dir, name)?],
        None => series::active_series(&dir)?,
    };
    let ledger = Ledger::local(ctx);
    let has_relations = chosen.iter().any(|series| !series.related.is_empty());
    let (journals, own) = if has_relations {
        let cfg = crate::config::load()?;
        let journals = crate::registry::known_journals(&cfg).unwrap_or_default();
        let own = journals
            .iter()
            .find(|(_, journal)| same_dir(journal, &dir))
            .map(|(slug, _)| slug.clone());
        (journals, own)
    } else {
        (Vec::new(), None)
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
                        read_related(&journals, relation),
                        Vec::new(),
                    ));
                    reads.len() - 1
                }
            };
            let (_, read, from) = &mut reads[index];
            from.push(series.file.clone());
            let mutual = read.result.as_ref().ok().map(|(other, _)| {
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
        reports.push(series_report(series, &ledger, relations, now));
    }
    let related = reads
        .into_iter()
        .map(|(relation, read, from)| related_report(ctx, read, &relation.file, from, now))
        .collect();
    let waiting_on_person = series::person_questions(&dir)?
        .into_iter()
        .map(|question| WaitingQuestion {
            file: question.file,
            question: question.question,
            heading: question.heading,
            asked_at: question.asked_at,
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

fn report(ctx: &Ctx, name: Option<&str>, json: bool) -> Result<i32> {
    let report = build(ctx, name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(0);
    }
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
    Ok(0)
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
    render_last_landing(series.last_landing.as_ref());
}

fn render_row(row: &ItemRow, width: usize) {
    let mut detail = Vec::new();
    if let Some(change) = &row.change {
        detail.push(format!("change {change}"));
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
    if let Some(at) = &row.landed_at {
        detail.push(format!(
            "landed {at}{}",
            row.revision
                .as_deref()
                .map(|revision| format!(" at {}", &revision[..revision.len().min(12)]))
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
        "  {} of {} complete ({} landed, {} done)",
        counts.complete, counts.items, counts.landed, counts.done
    );
    if let Some(percent) = counts.percent {
        line.push_str(&format!(", {percent}% by count of items"));
    }
    line.push_str(&format!(
        "; {} in flight; {} open",
        counts.in_flight, counts.open
    ));
    if counts.closed > 0 {
        line.push_str(&format!("; {} closed without landing", counts.closed));
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

fn render_last_landing(landing: Option<&Landing>) {
    match landing {
        Some(landing) => println!(
            "  last landing: {} at {}{}",
            landing.item,
            landing.at,
            landing
                .revision
                .as_deref()
                .map(|revision| format!(", {}", &revision[..revision.len().min(12)]))
                .unwrap_or_default()
        ),
        None => println!("  last landing: none"),
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
    render_last_landing(related.last_landing.as_ref());
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

/// `arc workspace progress`: every registered project's active series.
pub(crate) fn workspace(
    ctx: &Ctx,
    includes: impl Fn(Option<&Path>) -> bool,
    scope: impl Serialize,
    json: bool,
) -> Result<i32> {
    let now = Utc::now();
    let cfg = crate::config::load()?;
    let mut projects = Vec::new();
    let mut failures = Vec::new();
    let registered = crate::registry::journal_projects(&cfg)?;
    let labels: BTreeMap<String, String> = registered
        .iter()
        .map(|project| (project.slug.clone(), project.label()))
        .collect();
    for project in registered {
        if project.retired.is_some() || !includes(project.anchor.as_deref()) {
            continue;
        }
        let read = (|| -> Result<Vec<SeriesReport>> {
            let active = series::active_series(&project.journal_dir)?;
            if active.is_empty() {
                return Ok(Vec::new());
            }
            let ledger = match project.anchor.as_deref() {
                Some(anchor) => Ledger::at(ctx, anchor),
                None => Ledger::new(ctx, &project.journal_dir, || Ok(None)),
            };
            Ok(active
                .iter()
                .map(|series| {
                    let relations = series
                        .related
                        .iter()
                        .map(|relation| Relation {
                            project: labels
                                .get(&relation.project)
                                .cloned()
                                .unwrap_or_else(|| relation.project.clone()),
                            slug: relation.project.clone(),
                            file: relation.file.clone(),
                            mutual: None,
                        })
                        .collect();
                    series_report(series, &ledger, relations, now)
                })
                .collect())
        })();
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
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": WORKSPACE_PROGRESS_SCHEMA,
                "scope": scope,
                "projects": projects,
                "failures": failures,
            }))?
        );
    } else {
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
    for failure in &failures {
        eprintln!(
            "{}: unavailable ({})",
            failure.project,
            failure.reason.replace(['\n', '\r'], " ")
        );
    }
    Ok(if failures.is_empty() { 0 } else { 16 })
}
