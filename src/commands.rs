use crate::bundle::{Bundle, ValidatedBundle};
use crate::gates;
use crate::gitio;
use crate::ids;
use crate::model::*;
use crate::render;
use crate::state::{self, ChangeState};
use crate::status::{self, StatusReport};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Ctx {
    pub cwd: PathBuf,
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
}

impl Ctx {
    fn store(&self) -> Result<Store> {
        Store::discover(&self.cwd)
    }

    fn event(&self, store: &Store, change_id: &str, payload: Payload) -> Event {
        Event {
            schema_version: SCHEMA_VERSION,
            event_id: ids::new_event_id(),
            repository_id: store.repository_id.clone(),
            change_id: change_id.to_string(),
            actor: self.actor.clone(),
            harness: self.harness.clone(),
            session: self.session.clone(),
            created_at: chrono::Utc::now(),
            payload,
        }
    }

    fn load_state(&self, store: &Store, reference: &str) -> Result<(String, ChangeState)> {
        let change_id = store.resolve_change(reference)?;
        let events = store.load_events(&change_id)?;
        let state = state::reduce(&events)?;
        Ok((change_id, state))
    }

    fn report(&self, state: &ChangeState) -> Result<StatusReport> {
        let toplevel = gitio::toplevel(&self.cwd)?;
        let gates = gates::load(&toplevel)?;
        status::build(state, &self.cwd, &gates)
    }
}

pub fn read_body(body: Option<String>, body_file: Option<String>) -> Result<String> {
    let text = match (body, body_file) {
        (Some(b), None) => b,
        (None, Some(f)) if f == "-" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        (None, Some(f)) => {
            std::fs::read_to_string(&f).with_context(|| format!("cannot read body file {f}"))?
        }
        (None, None) => bail!("provide --body or --body-file (use '-' for stdin)"),
        (Some(_), Some(_)) => bail!("--body and --body-file are mutually exclusive"),
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("body is empty");
    }
    Ok(trimmed.to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn begin(
    ctx: &Ctx,
    slug: &str,
    title: Option<String>,
    profile: &str,
    target: Option<String>,
    base: Option<String>,
    branch: Option<String>,
    worktree: Option<String>,
    no_worktree: bool,
    adopt: Option<String>,
) -> Result<()> {
    ids::validate_slug(slug)?;
    let store = ctx.store()?;

    let mut open_change_branches: Vec<String> = Vec::new();
    for existing in store.list_change_ids()? {
        let events = store.load_events(&existing)?;
        let st = state::reduce(&events)?;
        if st.is_closed() {
            continue;
        }
        if st.slug == slug {
            bail!(
                "open change {existing} already uses slug {slug:?}; continue it or close it first"
            );
        }
        open_change_branches.push(st.branch);
    }

    // Changes derive from the branch they intend to merge into. The
    // default is the primary worktree's branch (the main checkout,
    // normally master/main) — never whatever branch happens to be
    // checked out here, which may itself be work in progress. Stacking
    // on another open change requires an explicit --target.
    let explicit_target = target.is_some();
    let target_branch = match target {
        Some(t) => t,
        None => gitio::primary_worktree_branch(&ctx.cwd)?
            .or(gitio::current_branch(&ctx.cwd)?)
            .context("cannot determine a target branch (detached?); pass --target")?,
    };
    if !explicit_target && open_change_branches.contains(&target_branch) {
        bail!(
            "default target {target_branch:?} is another open change's branch; \
             pass --target explicitly to stack changes deliberately"
        );
    }
    let target_head = gitio::branch_head(&ctx.cwd, &target_branch)?;

    let change_id = ids::new_change_id(slug);
    let title = title.unwrap_or_else(|| slug.replace('-', " "));

    let (branch_name, base_rev, worktree_path) = if let Some(adopted) = adopt {
        if !gitio::branch_exists(&ctx.cwd, &adopted) {
            bail!("--adopt branch {adopted:?} does not exist");
        }
        let branch_head = gitio::branch_head(&ctx.cwd, &adopted)?;
        let base_rev = match base {
            Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
            None => gitio::merge_base(&ctx.cwd, &target_head, &branch_head)?,
        };
        let wt = gitio::worktree_for_branch(&ctx.cwd, &adopted)?.map(|p| p.display().to_string());
        (adopted, base_rev, wt)
    } else {
        let branch_name = branch.unwrap_or_else(|| format!("arc/{slug}"));
        if gitio::branch_exists(&ctx.cwd, &branch_name) {
            bail!("branch {branch_name:?} already exists; use --adopt {branch_name} to track it");
        }
        let base_rev = match base {
            Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
            None => target_head.clone(),
        };
        gitio::create_branch(&ctx.cwd, &branch_name, &base_rev)?;
        let wt = if no_worktree {
            None
        } else {
            let path = match worktree {
                Some(p) => PathBuf::from(p),
                None => default_worktree_path(&ctx.cwd, slug)?,
            };
            if path.exists() {
                bail!("worktree path {} already exists", path.display());
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            gitio::add_worktree(&ctx.cwd, &path, &branch_name)?;
            Some(path.display().to_string())
        };
        (branch_name, base_rev, wt)
    };

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ChangeOpened {
            slug: slug.to_string(),
            title,
            profile: profile.to_string(),
            target_branch,
            branch: branch_name.clone(),
            base: base_rev,
            worktree: worktree_path.clone(),
        },
    );
    store.append_event(&ev)?;

    println!("change: {change_id}");
    println!("branch: {branch_name}");
    if let Some(wt) = worktree_path {
        println!("worktree: {wt}");
    }
    Ok(())
}

fn default_worktree_path(cwd: &Path, slug: &str) -> Result<PathBuf> {
    let toplevel = gitio::toplevel(cwd)?;
    let repo_name = toplevel
        .file_name()
        .context("cannot determine repository name")?
        .to_string_lossy()
        .into_owned();
    let config = crate::config::load()?;
    Ok(config.worktrees_dir.join(format!("{repo_name}-{slug}")))
}

pub fn list(ctx: &Ctx, open_only: bool, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let mut rows = Vec::new();
    for id in store.list_change_ids()? {
        let events = store.load_events(&id)?;
        let st = state::reduce(&events)?;
        if open_only && st.is_closed() {
            continue;
        }
        rows.push(serde_json::json!({
            "change_id": id,
            "slug": st.slug,
            "title": st.title,
            "profile": st.profile,
            "branch": st.branch,
            "state": if st.is_closed() { "closed" } else { "open" },
        }));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if rows.is_empty() {
        println!("no changes");
    } else {
        for r in rows {
            println!(
                "{}  [{}] {} ({})",
                r["change_id"].as_str().unwrap_or(""),
                r["state"].as_str().unwrap_or(""),
                r["title"].as_str().unwrap_or(""),
                r["branch"].as_str().unwrap_or(""),
            );
        }
    }
    Ok(())
}

pub fn show(ctx: &Ctx, reference: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
    } else {
        let report = ctx.report(&st)?;
        print!("{}", render::markdown(&st, &report));
    }
    Ok(())
}

pub fn status_cmd(ctx: &Ctx, reference: &str) -> Result<()> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&st)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

pub fn export_bundle(ctx: &Ctx, reference: &str, output: &str) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let bundle = Bundle::export(&store, &change_id)?;
    let bytes = bundle.to_bytes()?;
    if output == "-" {
        std::io::stdout().write_all(&bytes)?;
        eprintln!("events: {}", bundle.event_count);
        eprintln!("sha256: {}", bundle.events_sha256);
        eprintln!("output: -");
    } else {
        std::fs::write(output, bytes)
            .with_context(|| format!("cannot write export bundle {output}"))?;
        println!("events: {}", bundle.event_count);
        println!("sha256: {}", bundle.events_sha256);
        println!("output: {output}");
    }
    Ok(())
}

pub fn import_bundle(ctx: &Ctx, input: &str, dry_run: bool) -> Result<i32> {
    let bytes = if input == "-" {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(input).with_context(|| format!("cannot read import bundle {input}"))?
    };
    // Parsing validates every path-bearing ID, checksum, envelope, and
    // patchset field before the local store is inspected or created.
    let validated = Bundle::parse(&bytes)?;
    let root = Store::resolve_root(&ctx.cwd)?;
    let local_repository_id = Store::repository_id_at(&root)?;

    let mut new_events = Vec::new();
    let mut skipped_events = Vec::new();
    let mut conflicts = Vec::new();
    for event in &validated.events {
        match Store::raw_event_at(&root, &validated.bundle.change_id, &event.event_id)? {
            None => new_events.push(event.event_id.clone()),
            Some(existing) => match serde_json::from_slice::<serde_json::Value>(&existing) {
                Ok(value) if value == event.value => skipped_events.push(event.event_id.clone()),
                _ => conflicts.push(event.event_id.clone()),
            },
        }
    }

    let mut missing_objects = Vec::new();
    let mut pins = Vec::new();
    for patchset in &validated.patchsets {
        if !gitio::commit_exists(&ctx.cwd, &patchset.base)? {
            missing_objects.push((patchset.event_id.clone(), "base", patchset.base.clone()));
        }
        if gitio::commit_exists(&ctx.cwd, &patchset.head)? {
            pins.push((
                gitio::retention_ref(&validated.bundle.change_id, &patchset.patchset_id),
                patchset.head.clone(),
            ));
        } else {
            missing_objects.push((patchset.event_id.clone(), "head", patchset.head.clone()));
        }
    }

    print_import_report(
        &validated,
        local_repository_id.as_deref(),
        &new_events,
        &skipped_events,
        &conflicts,
        &missing_objects,
        &pins,
        dry_run,
    );
    if !conflicts.is_empty() {
        println!("aborted: no events or refs written");
        return Ok(1);
    }
    if dry_run {
        return Ok(0);
    }

    let store = Store::discover(&ctx.cwd)?;
    if local_repository_id.is_none() && store.repository_id != validated.bundle.repository_id {
        println!(
            "repository: bundle {} differs from local {} (expected for cross-machine import)",
            validated.bundle.repository_id, store.repository_id
        );
    }
    for event in &validated.events {
        if new_events.contains(&event.event_id) {
            store.append_raw_event(&validated.bundle.change_id, &event.event_id, &event.bytes)?;
        }
    }
    for (name, head) in pins {
        gitio::update_ref(&ctx.cwd, &name, &head)?;
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn print_import_report(
    validated: &ValidatedBundle,
    local_repository_id: Option<&str>,
    new_events: &[String],
    skipped_events: &[String],
    conflicts: &[String],
    missing_objects: &[(String, &str, String)],
    pins: &[(String, String)],
    dry_run: bool,
) {
    if let Some(local) = local_repository_id {
        if local != validated.bundle.repository_id {
            println!(
                "repository: bundle {} differs from local {local} (expected for cross-machine import)",
                validated.bundle.repository_id
            );
        }
    }
    for event_id in new_events {
        println!("new: {event_id}");
    }
    for event_id in skipped_events {
        println!("skipped: {event_id}");
    }
    for event_id in conflicts {
        println!("conflict: {event_id}");
    }
    for (event_id, kind, oid) in missing_objects {
        println!("warning: event {event_id} is missing {kind} commit {oid}");
    }
    for (event_id, event_type) in &validated.unknown_event_types {
        println!("unknown event type: {event_id} {event_type} (preserved verbatim)");
    }
    for (name, head) in pins {
        if dry_run {
            println!("would pin: {name} -> {head}");
        } else {
            println!("pin: {name} -> {head}");
        }
    }
    println!(
        "summary: new={} skipped={} conflicts={} missing_objects={}",
        new_events.len(),
        skipped_events.len(),
        conflicts.len(),
        missing_objects.len()
    );
    if dry_run {
        println!("dry-run: no events or refs written");
    }
}

pub fn check(ctx: &Ctx, reference: &str) -> Result<i32> {
    let store = ctx.store()?;
    let (_, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&st)?;
    if report.integrate_ready {
        println!("ready: all integration gates pass");
    } else {
        for b in &report.blockers {
            println!("blocker: {b:?}");
        }
    }
    Ok(status::check_exit_code(&report))
}

pub fn snapshot(ctx: &Ctx, reference: &str, base: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let head = gitio::branch_head(&ctx.cwd, &st.branch)?;
    let base_rev = match base {
        Some(b) => gitio::rev_parse(&ctx.cwd, &b)?,
        None => st.base.clone(),
    };
    if let Some(p) = st.latest_patchset() {
        if p.head == head && p.base == base_rev {
            println!("patchset: {} (unchanged)", p.id);
            return Ok(());
        }
    }
    let target_head = gitio::branch_head(&ctx.cwd, &st.target_branch).ok();
    let merge_base = target_head
        .as_deref()
        .and_then(|t| gitio::merge_base(&ctx.cwd, t, &head).ok());
    let patchset_id = format!("ps-{:02}", st.patchsets.len() + 1);
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::PatchsetAdded {
            patchset_id: patchset_id.clone(),
            base: base_rev,
            head: head.clone(),
            merge_base,
        },
    );
    store.append_event(&ev)?;
    // Pin this head with its own ref: reviewed heads must stay reachable
    // individually, even if the branch is rewound or deleted later.
    gitio::update_ref(
        &ctx.cwd,
        &gitio::retention_ref(&change_id, &patchset_id),
        &head,
    )?;
    println!("patchset: {patchset_id}");
    println!("head: {head}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub struct AnchorArgs {
    pub path: Option<String>,
    pub side: Side,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub context: Option<String>,
}

fn build_anchor(
    ctx: &Ctx,
    st: &ChangeState,
    patchset_id: Option<&str>,
    args: &AnchorArgs,
) -> Result<Option<Anchor>> {
    let Some(path) = &args.path else {
        if args.line_start.is_some() {
            bail!("--line requires --path");
        }
        return Ok(None);
    };
    let patchset = match patchset_id {
        Some(id) => st.patchsets.iter().find(|p| p.id == id),
        None => st.latest_patchset(),
    };
    let blob = patchset.and_then(|p| {
        let rev = match args.side {
            Side::Base => &p.base,
            Side::Head => &p.head,
        };
        gitio::blob_oid(&ctx.cwd, rev, path)
    });
    Ok(Some(Anchor {
        path: path.clone(),
        side: args.side,
        blob,
        line_start: args.line_start,
        line_end: args.line_end.or(args.line_start),
        context: args.context.clone(),
    }))
}

fn resolve_patchset_id(st: &ChangeState, patchset: Option<String>) -> Result<Option<String>> {
    match patchset {
        Some(id) => {
            if !st.patchsets.iter().any(|p| p.id == id) {
                bail!("unknown patchset {id:?}");
            }
            Ok(Some(id))
        }
        None => Ok(st.latest_patchset().map(|p| p.id.clone())),
    }
}

pub fn comment(
    ctx: &Ctx,
    reference: &str,
    body: String,
    patchset: Option<String>,
    anchor_args: &AnchorArgs,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::CommentAdded {
            body,
            patchset_id,
            anchor,
        },
    );
    store.append_event(&ev)?;
    println!("event: {}", ev.event_id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn finding(
    ctx: &Ctx,
    reference: &str,
    summary: String,
    body: Option<String>,
    blocking: bool,
    severity: Severity,
    patchset: Option<String>,
    anchor_args: &AnchorArgs,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let patchset_id = resolve_patchset_id(&st, patchset)?;
    let anchor = build_anchor(ctx, &st, patchset_id.as_deref(), anchor_args)?;
    let finding_id = ids::new_finding_id();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::FindingAdded {
            finding_id: finding_id.clone(),
            blocking,
            severity,
            summary,
            body,
            patchset_id,
            anchor,
        },
    );
    store.append_event(&ev)?;
    println!("finding: {finding_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn reply(ctx: &Ctx, reference: &str, parent_event_id: String, body: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let known = st.comments.iter().any(|c| c.event_id == parent_event_id)
        || st
            .findings
            .values()
            .any(|f| f.origin_event == parent_event_id);
    if !known {
        bail!("no comment or finding event {parent_event_id:?} in this change");
    }
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ReplyAdded {
            parent_event_id,
            body,
        },
    );
    store.append_event(&ev)?;
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn resolve(
    ctx: &Ctx,
    reference: &str,
    finding: String,
    disposition: DispositionStatus,
    commit: Option<String>,
    evidence: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let finding_id = st.resolve_finding_id(&finding)?;
    let commit = match commit {
        Some(c) => Some(gitio::rev_parse(&ctx.cwd, &c)?),
        None => None,
    };
    let supersedes: Vec<String> = st.findings[&finding_id]
        .tips()
        .iter()
        .map(|t| t.event_id.clone())
        .collect();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::DispositionRecorded {
            finding_id: finding_id.clone(),
            status: disposition,
            commit,
            evidence,
            supersedes,
        },
    );
    store.append_event(&ev)?;
    println!("finding: {finding_id} → {disposition:?}");
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn review(
    ctx: &Ctx,
    reference: &str,
    verdict: Verdict,
    patchset: Option<String>,
    findings_json: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let patchset_id = resolve_patchset_id(&st, patchset)?
        .context("no patchset to review; run `arc snapshot` first")?;

    let inline: Vec<InlineFinding> = match findings_json {
        None => Vec::new(),
        Some(src) => {
            let text = if src == "-" {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                std::fs::read_to_string(&src)
                    .with_context(|| format!("cannot read findings file {src}"))?
            };
            let inputs: Vec<FindingInput> =
                serde_json::from_str(&text).context("malformed findings JSON")?;
            inputs
                .into_iter()
                .map(|f| {
                    let anchor = f.anchor.map(|a| {
                        let anchor_args = AnchorArgs {
                            path: Some(a.path),
                            side: a.side,
                            line_start: a.line_start,
                            line_end: a.line_end,
                            context: a.context,
                        };
                        build_anchor(ctx, &st, Some(&patchset_id), &anchor_args)
                            .ok()
                            .flatten()
                    });
                    InlineFinding {
                        finding_id: ids::new_finding_id(),
                        blocking: f.blocking,
                        severity: f.severity,
                        summary: f.summary,
                        body: f.body,
                        anchor: anchor.flatten(),
                    }
                })
                .collect()
        }
    };

    if verdict == Verdict::Approved && inline.iter().any(|f| f.blocking) {
        bail!("cannot approve while recording blocking findings in the same review");
    }

    let finding_ids: Vec<String> = inline.iter().map(|f| f.finding_id.clone()).collect();
    let ev = ctx.event(
        &store,
        &change_id,
        Payload::VerdictRecorded {
            patchset_id: patchset_id.clone(),
            verdict,
            findings: inline,
        },
    );
    store.append_event(&ev)?;
    println!("verdict: {verdict:?} on {patchset_id}");
    for id in finding_ids {
        println!("finding: {id}");
    }
    println!("event: {}", ev.event_id);
    Ok(())
}

pub fn verify(
    ctx: &Ctx,
    reference: &str,
    gate: Option<String>,
    command: Option<String>,
) -> Result<i32> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let toplevel = gitio::toplevel(&ctx.cwd)?;
    let cmd = match (&gate, command) {
        (Some(name), None) => {
            let gates = gates::load(&toplevel)?;
            gates
                .gates
                .get(name)
                .with_context(|| format!("gate {name:?} not declared in .arc/gates.toml"))?
                .command
                .clone()
        }
        (None, Some(c)) => c,
        (Some(_), Some(_)) => bail!("--gate and --command are mutually exclusive"),
        (None, None) => bail!("provide --gate <name> or --command <cmd>"),
    };
    let revision = gitio::head(&ctx.cwd)?;
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into());

    eprintln!("running: {cmd}");
    let started = std::time::Instant::now();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&ctx.cwd)
        .status()
        .context("failed to run gate command")?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let exit_code = out.code().unwrap_or(-1);
    let result = if out.success() {
        VerifyResult::Pass
    } else {
        VerifyResult::Fail
    };

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::VerificationRecorded {
            gate,
            command: cmd,
            revision: revision.clone(),
            result,
            exit_code,
            duration_ms,
            hostname,
        },
    );
    store.append_event(&ev)?;
    let _ = st;
    println!("verification: {result:?} at {revision}");
    println!("event: {}", ev.event_id);
    Ok(if out.success() { 0 } else { 1 })
}

pub fn hold(ctx: &Ctx, reference: &str, reason: String) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is closed");
    }
    let ev = ctx.event(&store, &change_id, Payload::HoldSet { reason });
    store.append_event(&ev)?;
    println!("hold set on {change_id}");
    Ok(())
}

pub fn release_hold(ctx: &Ctx, reference: &str, reason: Option<String>) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.hold.is_none() {
        bail!("no active hold on {change_id}");
    }
    let ev = ctx.event(&store, &change_id, Payload::HoldReleased { reason });
    store.append_event(&ev)?;
    println!("hold released on {change_id}");
    Ok(())
}

pub fn integrate(
    ctx: &Ctx,
    reference: &str,
    into: Option<String>,
    message: Option<String>,
    cleanup: bool,
) -> Result<i32> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    let report = ctx.report(&st)?;
    if !report.integrate_ready {
        for b in &report.blockers {
            eprintln!("blocker: {b:?}");
        }
        return Ok(status::check_exit_code(&report));
    }

    let target = into.unwrap_or_else(|| st.target_branch.clone());
    // The approved head, merged by exact SHA so a branch moved after
    // approval can never smuggle unreviewed commits into the merge.
    let approved_head = st
        .latest_patchset()
        .context("no patchset recorded")?
        .head
        .clone();

    let wt = gitio::worktree_for_branch(&ctx.cwd, &target)?
        .with_context(|| format!("no worktree has {target:?} checked out; check it out first"))?;
    if !gitio::is_clean(&wt)? {
        bail!("target worktree {} is not clean", wt.display());
    }
    let old_target = gitio::branch_head(&ctx.cwd, &target)?;
    let msg = message.unwrap_or_else(|| format!("merge({}): {}", st.slug, st.title));

    if let Err(e) = gitio::git(
        &wt,
        &["merge", "--no-ff", "--no-edit", "-m", &msg, &approved_head],
    ) {
        let _ = gitio::git(&wt, &["merge", "--abort"]);
        bail!("merge failed (aborted): {e}");
    }

    let merged = gitio::head(&wt)?;
    let parents = gitio::commit_parents(&wt, &merged)?;
    if parents != vec![old_target.clone(), approved_head.clone()] {
        bail!(
            "merge commit {merged} has unexpected parents {parents:?}; \
             expected [{old_target}, {approved_head}] — target moved during \
             integration, inspect before trusting this merge"
        );
    }

    let ev = ctx.event(
        &store,
        &change_id,
        Payload::ChangeClosed {
            outcome: Closure::Integrated,
            integrated_commit: Some(merged.clone()),
            superseded_by: None,
        },
    );
    store.append_event(&ev)?;
    release_retention_refs(ctx, &change_id, Some(&merged))?;

    println!("integrated: {merged}");
    println!("event: {}", ev.event_id);

    if cleanup {
        // Run cleanup git commands from the target worktree: ctx.cwd may be
        // inside the change worktree that is about to be removed.
        if let Some(wt_path) = &st.worktree {
            let p = PathBuf::from(wt_path);
            if p.exists() && p != wt {
                gitio::git(&wt, &["worktree", "remove", wt_path])?;
                println!("removed worktree {wt_path}");
            }
        }
        // -d refuses unless merged: exactly the safety we want.
        gitio::git(&wt, &["branch", "-d", &st.branch])?;
        println!("deleted branch {}", st.branch);
    }
    Ok(0)
}

pub fn close(
    ctx: &Ctx,
    reference: &str,
    integrated: Option<String>,
    abandoned: bool,
    superseded_by: Option<String>,
) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, st) = ctx.load_state(&store, reference)?;
    if st.is_closed() {
        bail!("change {change_id} is already closed");
    }
    let (payload, integrated_rev) = match (integrated, abandoned, superseded_by) {
        (Some(rev), false, None) => {
            let rev = gitio::rev_parse(&ctx.cwd, &rev)?;
            (
                Payload::ChangeClosed {
                    outcome: Closure::Integrated,
                    integrated_commit: Some(rev.clone()),
                    superseded_by: None,
                },
                Some(rev),
            )
        }
        (None, true, None) => (
            Payload::ChangeClosed {
                outcome: Closure::Abandoned,
                integrated_commit: None,
                superseded_by: None,
            },
            None,
        ),
        (None, false, Some(other)) => {
            let other_id = store.resolve_change(&other)?;
            (
                Payload::ChangeClosed {
                    outcome: Closure::Superseded,
                    integrated_commit: None,
                    superseded_by: Some(other_id),
                },
                None,
            )
        }
        _ => bail!("provide exactly one of --integrated <rev>, --abandoned, --superseded <change>"),
    };
    let ev = ctx.event(&store, &change_id, payload);
    store.append_event(&ev)?;
    release_retention_refs(ctx, &change_id, integrated_rev.as_deref())?;
    println!("closed: {change_id}");
    println!("event: {}", ev.event_id);
    Ok(())
}

/// Drop a change's retention refs only for heads proven reachable from
/// the integration commit. Everything else stays pinned: abandoned or
/// externally rewritten (squash/rebase) work must never become
/// GC-collectable through arc. Unpinning by hand remains possible with
/// `git update-ref -d refs/arc/keep/<change>/<patchset>`.
fn release_retention_refs(ctx: &Ctx, change_id: &str, integrated: Option<&str>) -> Result<()> {
    let refs = gitio::list_refs(&ctx.cwd, &gitio::retention_prefix(change_id))?;
    for (name, oid) in refs {
        let reachable = match integrated {
            Some(rev) => gitio::is_ancestor(&ctx.cwd, &oid, rev)?,
            None => false,
        };
        if reachable {
            let _ = gitio::delete_ref(&ctx.cwd, &name);
        } else {
            println!("kept {name}: head {oid} is not reachable from the integrated commit");
        }
    }
    Ok(())
}
