use super::*;
use crate::policy;
use crate::ExecutionRole;

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
    blocked_by: Vec<String>,
    tags: Vec<String>,
    from_journal: Option<String>,
) -> Result<()> {
    ids::validate_slug(slug)?;
    // Validate the journal source before writing anything: a bad
    // --from-journal must fail cleanly with no branch, worktree, or event.
    let journal_kind = from_journal
        .as_deref()
        .map(|filename| crate::journal::require_open_actionable(ctx, filename))
        .transpose()?;
    let store = ctx.store()?;
    let blocked_by = blocked_by
        .iter()
        .map(|reference| store.resolve_change(reference))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let tags = normalize_tags(tags)?;

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
            base: base_rev.clone(),
            worktree: worktree_path.clone(),
            blocked_by,
            tags,
            journal_ref: from_journal.clone(),
        },
    );
    store.append_event(&ev)?;

    // Advisory bridge to the journal, both best-effort: mark the source item
    // consumed, and narrate the opening if auto-log is enabled. Neither can
    // fail the authoritative change that already exists.
    if let Some(filename) = from_journal
        .as_ref()
        .filter(|_| journal_kind.as_deref() != Some("plan"))
    {
        if let Err(error) = crate::journal::consume_superseded_by_change(ctx, filename, &change_id)
        {
            eprintln!("warning: could not mark {filename} consumed: {error:#}");
        }
        // Thread the source's content into an initial brief so the change
        // starts with the resolution instead of an empty contract. Advisory:
        // a read or write failure warns but never unwinds the change that
        // already exists.
        match crate::journal::read_artifact_body(ctx, filename) {
            Ok(body) => {
                let seeded = format!(
                    "> Seeded from journal artifact `{filename}` by `begin --from-journal`.\n\n{}",
                    body.trim_end_matches('\n')
                );
                let event = ctx.event(
                    &store,
                    &change_id,
                    Payload::BriefRecorded {
                        title: Some(format!("Seeded from {filename}")),
                        body: seeded,
                        base_revision: Some(base_rev.clone()),
                        acceptance_probes: Vec::new(),
                        plan_ref: None,
                        plan_slice: None,
                    },
                );
                if let Err(error) = store.append_event(&event) {
                    eprintln!("warning: could not seed a brief from {filename}: {error:#}");
                }
            }
            Err(error) => {
                eprintln!("warning: could not read {filename} to seed a brief: {error:#}")
            }
        }
    }
    crate::journal::auto_log(ctx, slug, &format!("opened change {change_id}"));

    println!("change: {change_id}");
    println!("branch: {branch_name}");
    if let Some(wt) = worktree_path {
        println!("worktree: {wt}");
    }
    Ok(())
}

pub fn list(ctx: &Ctx, open_only: bool, json: bool, format: ListFormat) -> Result<()> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| !open_only || !state.is_closed())
        .collect::<Vec<_>>();

    if json || matches!(format, ListFormat::Json) {
        let rows = selected
            .iter()
            .map(|state| list_row(state, &states))
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else if selected.is_empty() {
        println!("no changes");
    } else {
        match format {
            ListFormat::Compact => {
                for state in selected {
                    println!("{}", state.change_id);
                }
            }
            ListFormat::Wide => {
                println!(
                    "{:<36} {:>8} {:<12} {:<18} {:<24} Target",
                    "Change", "Priority", "Status", "Verdict", "Blocker"
                );
                for state in selected {
                    println!(
                        "{:<36} {:>8} {:<12} {:<18} {:<24} {}",
                        state.change_id,
                        state.priority,
                        change_status(state),
                        verdict_label(state),
                        blocker_label(state, &states),
                        state.target_branch
                    );
                }
            }
            ListFormat::Default | ListFormat::Json => {
                for state in selected {
                    println!(
                        "{}  [{}] {} ({})",
                        state.change_id,
                        change_status(state),
                        state.title,
                        state.branch,
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn query(ctx: &Ctx, args: QueryArgs) -> Result<()> {
    if let Some(status) = &args.status {
        if !matches!(
            status.as_str(),
            "open" | "closed" | "integrated" | "abandoned" | "superseded"
        ) {
            bail!(
                "unknown status {status:?}; expected open, closed, integrated, abandoned, or superseded"
            );
        }
    }
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let tags = normalize_tags(args.tags)?;
    let selected = states
        .values()
        .filter(|state| {
            args.status
                .as_deref()
                .is_none_or(|wanted| status_matches(state, wanted))
                && args
                    .target
                    .as_deref()
                    .is_none_or(|target| state.target_branch == target)
                && tags.iter().all(|tag| state.tags.contains(tag))
                && args.verdict.is_none_or(|verdict| {
                    state.latest_verdict().is_some_and(|v| v.verdict == verdict)
                })
                && args
                    .actor
                    .as_deref()
                    .is_none_or(|actor| state.opened_by == actor)
                && args
                    .harness
                    .as_deref()
                    .is_none_or(|harness| state.opened_harness.as_deref() == Some(harness))
        })
        .collect::<Vec<_>>();

    if args.json {
        let rows = selected
            .iter()
            .map(|state| list_row(state, &states))
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        for state in selected {
            println!("{}", state.change_id);
        }
    }
    Ok(())
}

pub fn show_selection(
    ctx: &Ctx,
    role: ExecutionRole,
    reference: Option<&str>,
    tags: Vec<String>,
    json: bool,
    at: Option<&str>,
) -> Result<()> {
    match (reference, tags.is_empty()) {
        (Some(reference), true) => show(ctx, reference, json, role, at),
        (None, false) => show_tagged(ctx, normalize_tags(tags)?, json),
        (Some(_), false) => bail!("provide a change or --tag, not both"),
        (None, true) => bail!("provide a change or at least one --tag"),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn brief(
    ctx: &Ctx,
    role: ExecutionRole,
    reference: &str,
    body_file: Option<String>,
    title: Option<String>,
    base: Option<String>,
    version: Option<usize>,
    scaffold: Option<String>,
    plan_ref: Option<String>,
    plan_slice: Option<String>,
    probes_json: Option<String>,
) -> Result<i32> {
    if plan_ref.is_some() != plan_slice.is_some() {
        bail!("--plan-ref and --plan-slice must be provided together");
    }
    if body_file.as_deref() == Some("-") && probes_json.as_deref() == Some("-") {
        bail!("--body-file - and --probes-json - cannot both read stdin");
    }
    if body_file.is_some() || scaffold.is_some() || probes_json.is_some() {
        if role != ExecutionRole::Lead {
            eprintln!(
                "role refusal: {} may not brief (requires lead)",
                role.as_str()
            );
            return Ok(9);
        }
        if version.is_some() {
            bail!("--version cannot be used when recording a brief");
        }
        if let (Some(plan_ref), Some(plan_slice)) = (&plan_ref, &plan_slice) {
            crate::journal::validate_plan_artifact(ctx, plan_ref)?;
            crate::ids::validate_slug(plan_slice)?;
        }
        // A scaffold template is prepended to the body being recorded;
        // --scaffold with no --body-file records the template alone.
        let template = match &scaffold {
            Some(name) => super::scaffold::resolve(ctx, name)?,
            None => String::new(),
        };
        let content = match &body_file {
            Some(path) => read_body_file_verbatim(path)?,
            None => String::new(),
        };
        let body = super::scaffold::prepended(&template, &content);
        let acceptance_probes = probes_json
            .as_deref()
            .map(read_acceptance_probes)
            .transpose()?
            .unwrap_or_default();
        let base_revision = Some(gitio::rev_parse(
            &ctx.cwd,
            base.as_deref().unwrap_or("HEAD"),
        )?);
        let store = ctx.store()?;
        let (change_id, _transition, state) = locked_state(&store, reference)?;
        if state.is_closed() {
            bail!("change {change_id} is closed");
        }
        let next_version = state.briefs.len() + 1;
        let event = ctx.event(
            &store,
            &change_id,
            Payload::BriefRecorded {
                title,
                body,
                base_revision,
                acceptance_probes,
                plan_ref,
                plan_slice,
            },
        );
        store.append_event(&event)?;
        println!("brief: v{next_version}");
        println!("event: {}", event.event_id);
        return Ok(0);
    }

    if title.is_some() {
        bail!("--title requires --body-file or --scaffold");
    }
    if base.is_some() {
        bail!("--base requires --body-file or --scaffold");
    }
    if plan_ref.is_some() {
        bail!("--plan-ref and --plan-slice require --body-file or --scaffold");
    }
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let selected = match version {
        Some(0) => None,
        Some(version) => state.briefs.get(version - 1),
        None => state.latest_brief(),
    };
    let selected = selected.ok_or_else(|| match version {
        Some(version) => anyhow::anyhow!("brief version {version} not found"),
        None => anyhow::anyhow!("no brief recorded for change {}", state.change_id),
    })?;
    if let Some(base_revision) = &selected.base_revision {
        println!("base-revision: {base_revision}");
    }
    if let (Some(plan_ref), Some(plan_slice)) = (&selected.plan_ref, &selected.plan_slice) {
        println!("plan-ref: {plan_ref}");
        println!("plan-slice: {plan_slice}");
        println!();
    }
    for probe in &selected.acceptance_probes {
        println!("acceptance-probe: {} = {}", probe.name, probe.command);
    }
    if !selected.acceptance_probes.is_empty() {
        println!();
    }
    print!("{}", selected.body);
    Ok(0)
}

fn read_acceptance_probes(path: &str) -> Result<Vec<AcceptanceProbe>> {
    let raw = read_body_file_verbatim(path)?;
    let probes = serde_json::from_str::<Vec<AcceptanceProbe>>(&raw)
        .with_context(|| format!("invalid acceptance probe JSON from {path:?}"))?;
    let mut names = BTreeSet::new();
    for probe in &probes {
        crate::ids::validate_slug(&probe.name)
            .with_context(|| format!("invalid acceptance probe name {:?}", probe.name))?;
        if !names.insert(probe.name.clone()) {
            bail!("duplicate acceptance probe name {:?}", probe.name);
        }
        if probe.command.trim().is_empty() {
            bail!("acceptance probe {:?} has an empty command", probe.name);
        }
    }
    Ok(probes)
}

#[allow(clippy::too_many_arguments)]
pub fn metadata(
    ctx: &Ctx,
    reference: &str,
    blocked_by: Vec<String>,
    remove_blocked_by: Vec<String>,
    tags: Vec<String>,
    remove_tags: Vec<String>,
    assign: Option<String>,
    priority: Option<i32>,
) -> Result<()> {
    let store = ctx.store()?;
    let _graph = store.lock_graph()?;
    let (change_id, _transition, state) = locked_state(&store, reference)?;
    if state.is_closed() {
        bail!("change {change_id} is closed");
    }
    let states = ctx.load_all_states(&store)?;
    let add_blocked_by = blocked_by
        .iter()
        .map(|dependency| store.resolve_change(dependency))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let remove_blocked_by = remove_blocked_by
        .iter()
        .map(|dependency| resolve_blocker_removal(&store, &state, dependency))
        .collect::<Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    for dependency in &add_blocked_by {
        if dependency == &change_id || dependency_reaches(dependency, &change_id, &states) {
            bail!("adding blocker {dependency} would create a dependency cycle");
        }
    }
    let add_tags = normalize_tags(tags)?;
    let remove_tags = normalize_tags(remove_tags)?;
    // An assignment value may contain no whitespace-delimited surprises: a
    // harness label, or empty to clear. Store the trimmed form verbatim.
    let assign = match &assign {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.chars().any(char::is_whitespace) {
                bail!("assignment harness must not contain whitespace: {value:?}");
            }
            Some(trimmed.to_string())
        }
        None => None,
    };
    if add_blocked_by.is_empty()
        && remove_blocked_by.is_empty()
        && add_tags.is_empty()
        && remove_tags.is_empty()
        && assign.is_none()
        && priority.is_none()
    {
        bail!("provide at least one metadata change");
    }
    let event = ctx.event(
        &store,
        &change_id,
        Payload::MetadataUpdated {
            add_blocked_by,
            remove_blocked_by,
            add_tags,
            remove_tags,
            assign,
            priority,
        },
    );
    store.append_event(&event)?;
    println!("event: {}", event.event_id);
    Ok(())
}

#[derive(serde::Serialize)]
struct MetadataOutput<'a> {
    schema: &'static str,
    change_id: &'a str,
    blocked_by: &'a [String],
    tags: &'a [String],
    assigned_to: Option<&'a str>,
    priority: i32,
}

pub fn read_metadata(ctx: &Ctx, reference: &str, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let (change_id, state) = ctx.load_state(&store, reference)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&MetadataOutput {
                schema: "arc-metadata/1",
                change_id: &change_id,
                blocked_by: &state.blocked_by,
                tags: &state.tags,
                assigned_to: state.assigned_to.as_deref(),
                priority: state.priority,
            })?
        );
    } else {
        println!("change: {change_id}");
        println!("blocked-by: {}", state.blocked_by.join(", "));
        println!("tags: {}", state.tags.join(", "));
        println!(
            "assigned-to: {}",
            state.assigned_to.as_deref().unwrap_or("")
        );
        println!("priority: {}", state.priority);
    }
    Ok(())
}

pub fn status_cmd(
    ctx: &Ctx,
    reference: &str,
    get: Option<&str>,
    fields: Option<&str>,
    at: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let output = match at {
        Some(at) => {
            let st = super::reduce_at(&store, &change_id, at)?;
            status_output_as_of(ctx, &store, &st)?
        }
        None => {
            let st = state::reduce(&store.load_events(&change_id)?)?;
            status_output(ctx, &store, &st)?
        }
    };
    print_projected(serde_json::to_value(output)?, get, fields)?;
    Ok(())
}

pub(crate) fn print_projected(
    value: serde_json::Value,
    get: Option<&str>,
    fields: Option<&str>,
) -> Result<()> {
    match (get, fields) {
        (Some(path), None) => {
            let value =
                crate::project::get(value, path).with_context(|| format!("no value at {path}"))?;
            match value {
                serde_json::Value::String(value) => println!("{value}"),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                    println!("{}", serde_json::to_string(&value)?);
                }
                value => println!("{value}"),
            }
        }
        (None, Some(fields)) => println!(
            "{}",
            serde_json::to_string(&crate::project::fields(value, fields))?
        ),
        (None, None) => println!("{}", serde_json::to_string_pretty(&value)?),
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting projection flags"),
    }
    Ok(())
}

pub(crate) fn status_output(ctx: &Ctx, store: &Store, state: &ChangeState) -> Result<StatusOutput> {
    status_output_with(ctx, store, state, ctx.report(store, state)?)
}

fn status_output_as_of(ctx: &Ctx, store: &Store, state: &ChangeState) -> Result<StatusOutput> {
    status_output_with(ctx, store, state, ctx.report_as_of(store, state)?)
}

fn status_output_with(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    report: StatusReport,
) -> Result<StatusOutput> {
    let states = ctx.load_all_states(store)?;
    let suggested_alternatives = if report.blocker_status.blocked {
        find_unblocked_changes(&state.change_id, &states)
    } else {
        Vec::new()
    };
    Ok(StatusOutput {
        report,
        suggested_alternatives,
    })
}

pub fn blocker_status_cmd(ctx: &Ctx, reference: &str) -> Result<()> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let states = ctx.load_all_states(&store)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&dependency_status(&state, &states))?
    );
    Ok(())
}

pub fn is_blocked(ctx: &Ctx, reference: &str) -> Result<i32> {
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let states = ctx.load_all_states(&store)?;
    let blocker_status = dependency_status(&state, &states);
    if blocker_status.blocked {
        for blocker in blocker_status
            .blockers_ready
            .iter()
            .filter(|blocker| !blocker.integrated)
        {
            println!("blocked by {} ({})", blocker.change_id, blocker.status);
        }
        Ok(1)
    } else {
        println!("ready: all prerequisite changes are integrated");
        Ok(0)
    }
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

fn list_row(state: &ChangeState, states: &BTreeMap<String, ChangeState>) -> serde_json::Value {
    serde_json::json!({
        "change_id": state.change_id,
        "slug": state.slug,
        "title": state.title,
        "profile": state.profile,
        "branch": state.branch,
        "target_branch": state.target_branch,
        "state": if state.is_closed() { "closed" } else { "open" },
        "status": change_status(state),
        "verdict": verdict_label(state),
        "blocked_by": state.blocked_by,
        "blocker": blocker_label(state, states),
        "tags": state.tags,
        "priority": state.priority,
    })
}

fn status_matches(state: &ChangeState, wanted: &str) -> bool {
    match wanted {
        "closed" => state.is_closed(),
        other => change_status(state) == other,
    }
}

fn verdict_label(state: &ChangeState) -> &'static str {
    match state.latest_verdict().map(|verdict| verdict.verdict) {
        Some(Verdict::Approved) => "approved",
        Some(Verdict::ChangesRequested) => "changes-requested",
        Some(Verdict::CommentOnly) => "comment-only",
        None => "none",
    }
}

fn blocker_label(state: &ChangeState, states: &BTreeMap<String, ChangeState>) -> String {
    let dependencies = dependency_status(state, states);
    if let Some(blocker) = dependencies
        .blockers_ready
        .iter()
        .find(|blocker| !blocker.integrated)
    {
        return format!("blocked-by:{}", blocker.slug);
    }
    if !state.open_blocking_findings().is_empty() {
        return format!("{} findings", state.open_blocking_findings().len());
    }
    if state.hold.is_some() {
        return "hold".into();
    }
    "—".into()
}

fn show(
    ctx: &Ctx,
    reference: &str,
    json: bool,
    role: ExecutionRole,
    at: Option<&str>,
) -> Result<()> {
    let store = ctx.store()?;
    let change_id = store.resolve_change(reference)?;
    let st = match at {
        Some(at) => super::reduce_at(&store, &change_id, at)?,
        None => state::reduce(&store.load_events(&change_id)?)?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&st)?);
    } else {
        let states = ctx.load_all_states(&store)?;
        let report = match at {
            Some(_) => ctx.report_as_of(&store, &st)?,
            None => ctx.report(&store, &st)?,
        };
        let alternatives = if report.blocker_status.blocked {
            find_unblocked_changes(&st.change_id, &states)
        } else {
            Vec::new()
        };
        print!("{}", render::markdown(&st, &report, &alternatives));
        if !matches!(role, ExecutionRole::Implementer) {
            let policy = policy::load(&gitio::toplevel(&ctx.cwd)?)?;
            if !policy.review.checklist.is_empty() {
                println!("\n## Review checklist\n");
                for item in policy.review.checklist {
                    println!("- [ ] {item}");
                }
            }
        }
    }
    Ok(())
}

fn show_tagged(ctx: &Ctx, tags: Vec<String>, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let states = ctx.load_all_states(&store)?;
    let selected = states
        .values()
        .filter(|state| tags.iter().all(|tag| state.tags.contains(tag)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no changes match tags {}", tags.join(", "));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&selected)?);
    } else {
        for state in selected {
            let report = ctx.report(&store, state)?;
            let alternatives = if report.blocker_status.blocked {
                find_unblocked_changes(&state.change_id, &states)
            } else {
                Vec::new()
            };
            print!("{}", render::markdown(state, &report, &alternatives));
        }
    }
    Ok(())
}

fn resolve_blocker_removal(store: &Store, state: &ChangeState, reference: &str) -> Result<String> {
    if state.blocked_by.iter().any(|blocker| blocker == reference) {
        return Ok(reference.to_string());
    }
    let matches = state
        .blocked_by
        .iter()
        .filter(|blocker| blocker.starts_with(reference))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [blocker] => Ok((*blocker).clone()),
        [] => store.resolve_change(reference),
        _ => bail!(
            "ambiguous blocker {reference:?}: matches {}",
            matches
                .iter()
                .map(|blocker| blocker.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn dependency_reaches(start: &str, target: &str, states: &BTreeMap<String, ChangeState>) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(change_id) = pending.pop() {
        if change_id == target {
            return true;
        }
        if !visited.insert(change_id) {
            continue;
        }
        if let Some(state) = states.get(change_id) {
            pending.extend(state.blocked_by.iter().map(String::as_str));
        }
    }
    false
}
