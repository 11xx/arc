//! Unsigned staging preparation and authority-owned reconciliation.
use super::*;
use crate::integration::{self, Decision, IntegrationFacts, IntegrationPlan, Mode, Refusal, Shape};
use serde::{Deserialize, Serialize};

struct Selection {
    branch: String,
    before: String,
    held: Option<String>,
    recipient: ProvisionalRecipient,
    authority: Option<AuthorityObserved>,
    project: String,
}

fn checked_out_staging(cwd: &Path, branch: &str) -> Result<Option<PathBuf>> {
    Ok(gitio::worktree_inventory(cwd)?
        .into_iter()
        .find(|checkout| checkout.branch.as_deref() == Some(branch))
        .map(|checkout| checkout.path))
}

fn refuse_selection(
    code: i32,
    reason: String,
) -> Result<std::result::Result<Selection, (i32, String)>> {
    Ok(Err((code, reason)))
}

fn select(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    into: Option<&str>,
    replica: Option<&str>,
    forge: Option<&str>,
) -> Result<std::result::Result<Selection, (i32, String)>> {
    let status = crate::replica::status(store)?;
    let Some(local) = &status.local else {
        return refuse_selection(
            17,
            "provisional execution requires a paired replica; this store is unpaired".into(),
        );
    };
    if status.peers.is_empty() {
        return refuse_selection(
            17,
            "provisional execution requires a paired replica with a peer".into(),
        );
    }
    if status
        .authority
        .as_ref()
        .and_then(|a| a.holder.as_ref())
        .is_some_and(|holder| holder.repository_id == store.repository_id)
    {
        return refuse_selection(
            17,
            format!(
                "replica {} holds authority; use ordinary integrate",
                local.name
            ),
        );
    }
    if crate::ids::validate_id_component(&local.name).is_err() {
        return refuse_selection(
            17,
            format!(
                "replica name {:?} cannot define one staging namespace component",
                local.name
            ),
        );
    }
    let declarations = crate::declarations::for_change(&ctx.cwd, state)?;
    if declarations.target_unreadable {
        return refuse_selection(
            6,
            format!(
                "target branch {:?} is unreadable; canonical declarations cannot be established",
                state.target_branch
            ),
        );
    }
    if declarations.policy.replica.provisional_enabled == Some(false) {
        return refuse_selection(17, "provisional execution disabled by canonical project or operator replica.provisional_enabled=false".into());
    }
    let prefix = format!("arc/provisional/{}/", local.name);
    let branch = into
        .map(|value| {
            value
                .strip_prefix("refs/heads/")
                .unwrap_or(value)
                .to_string()
        })
        .unwrap_or_else(|| {
            format!(
                "{prefix}{}",
                state
                    .target_branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&state.target_branch)
            )
        });
    let primary = gitio::current_branch(&gitio::primary_worktree(&ctx.cwd)?)?;
    let states = store.readable_states()?;
    let unreadable: Vec<_> = store
        .list_change_ids()?
        .into_iter()
        .filter(|id| !states.contains_key(id))
        .collect();
    if !unreadable.is_empty() {
        return refuse_selection(17, format!("cannot establish protection for refs/heads/{branch}: unreadable canonical/source records {}", unreadable.join(", ")));
    }
    let protection = if states.values().any(|s| {
        s.target_branch.trim_start_matches("refs/heads/") == branch
            || s.closure
                .as_ref()
                .and_then(|closure| closure.target_branch.as_deref())
                .is_some_and(|target| target.trim_start_matches("refs/heads/") == branch)
    }) {
        Some("canonical target")
    } else if states
        .values()
        .any(|s| s.branch.trim_start_matches("refs/heads/") == branch)
    {
        Some("change source branch")
    } else if primary.as_deref() == Some(branch.as_str()) {
        Some("primary checkout branch")
    } else if declarations
        .policy
        .replica
        .protected_targets
        .iter()
        .any(|target| target.trim_start_matches("refs/heads/") == branch)
    {
        Some("replica.protected_targets")
    } else if !branch.starts_with(&prefix) {
        Some("foreign staging namespace")
    } else {
        None
    };
    if let Some(source) = protection {
        return refuse_selection(17, format!("refs/heads/{branch} is protected by {source}; destination must remain under refs/heads/{prefix}"));
    }
    gitio::git(
        &ctx.cwd,
        &["check-ref-format", &format!("refs/heads/{branch}")],
    )?;
    if let Some(path) = checked_out_staging(&ctx.cwd, &branch)? {
        bail!(
            "staging ref refs/heads/{branch} is checked out at {}; choose an unchecked staging ref",
            path.display()
        );
    }
    let recipient = match (replica, forge) {
        (Some(name), None) => {
            let Some(peer) = status.peers.iter().chain(status.local.iter()).find(|peer| peer.name == name) else { return refuse_selection(17, format!("no determinable recipient: unknown replica {name}; use --for-replica with a paired name or --for-forge REFERENCE")); };
            ProvisionalRecipient::Replica { repository_id: peer.repository_id.clone() }
        }
        (None, Some(reference)) if !reference.trim().is_empty() => ProvisionalRecipient::Forge { reference: reference.into() },
        (None, None) => match status.authority.as_ref().and_then(|a| a.holder.as_ref()) {
            Some(holder) => ProvisionalRecipient::Replica { repository_id: holder.repository_id.clone() },
            None => return refuse_selection(17, format!("authority has no holder (offer {:?}); specify --for-replica NAME or --for-forge REFERENCE", status.authority.as_ref().and_then(|a| a.offer_in_flight.as_ref()).map(|offer| &offer.offer_id))),
        },
        _ => bail!("conflicting --for-replica/--for-forge or blank forge recipient"),
    };
    let held = gitio::ref_value(&ctx.cwd, &format!("refs/heads/{branch}"))?;
    let before = held
        .clone()
        .unwrap_or(gitio::branch_head(&ctx.cwd, &state.target_branch)?);
    Ok(Ok(Selection {
        branch,
        before,
        held,
        recipient,
        authority: crate::replica::observed_authority(store)?,
        project: status
            .project_id
            .context("paired store has no project ID")?,
    }))
}

fn report(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    selection: &Selection,
) -> Result<(
    StatusReport,
    integration::Declarations,
    Vec<ProvisionalPrerequisite>,
)> {
    let mut declarations = ctx.declarations(state)?;
    declarations.notes.extend(
        declarations
            .gates
            .owe_also(
                crate::gates::inspect_at(&ctx.cwd, &selection.before)?,
                "canonical target",
                "staging history",
            )
            .iter()
            .map(crate::gates::GateDivergence::describe),
    );
    let mut selected = state.clone();
    if selection.held.is_some() {
        selected.target_branch = selection.branch.clone();
    }
    let states = store.readable_states()?;
    let mut dependencies = dependency_status(state, &states);
    let canonical_dependencies = dependencies.clone();
    let mut provisional = Vec::new();
    for dependency in &mut dependencies.blockers_ready {
        if !dependency.integrated {
            if let Some(prerequisite) = states
                .get(&dependency.change_id)
                .map(|s| {
                    crate::provisional::satisfies_local_prerequisite(
                        &ctx.cwd,
                        s,
                        &store.repository_id,
                        &selection.before,
                    )
                })
                .transpose()?
                .flatten()
            {
                dependency.integrated = true;
                dependency.status = "provisional".into();
                dependency.recovery = None;
                provisional.push(prerequisite);
            }
        }
    }
    dependencies.blocked = dependencies
        .blockers_ready
        .iter()
        .any(|dep| !dep.integrated);
    let mut report = status::build_at_with_shape(
        &selected,
        &ctx.cwd,
        &declarations.gates,
        &declarations.policy,
        declarations.target_unreadable,
        dependencies,
        changes_blocked_by(&state.change_id, &states),
        chrono::Utc::now(),
        fork::fork_slug_for_branch(&ctx.cwd, &state.branch)?,
        Shape::NoFf,
    )?;
    report.blocker_status = canonical_dependencies;
    report.require_replay_snapshot(crate::provisional::needs_replay_snapshot(
        &ctx.cwd, store, state,
    )?);
    report.select_mode(Mode::Provisional, None);
    report.provisional_prerequisites = provisional.clone();
    report.provisional_staging_destination = Some(selection.branch.clone());
    let normalized = super::gatekeeping::normalized_declarations(declarations, state);
    Ok((report, normalized, provisional))
}

fn basis(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    report: &StatusReport,
    declarations: integration::Declarations,
    provisional: Vec<ProvisionalPrerequisite>,
) -> Result<AuthorizationBasis> {
    let patchset = state.latest_patchset().context("no patchset recorded")?;
    let approval = integration::approval(state, report, &patchset.id);
    if approval.verdict_event_id.is_none()
        && approval.external_verdict.is_none()
        && !state.debt_waives_latest_patchset()
    {
        bail!("nothing authorizes the prepared patchset: record review or explicit debt");
    }
    let mut prerequisites = Vec::new();
    for dependency in &state.blocked_by {
        if provisional.iter().any(|p| &p.change_id == dependency) {
            continue;
        }
        let mut prior = store.state(dependency)?;
        let mut seen = BTreeSet::new();
        while seen.insert(prior.change_id.clone()) {
            let Some(closure) = &prior.closure else {
                bail!("prerequisite {} has no canonical closure", prior.change_id);
            };
            if closure.outcome == Closure::Integrated {
                prerequisites.push(PrerequisiteClosure {
                    change_id: prior.change_id.clone(),
                    closure_event_id: closure.event_id.clone(),
                    integrated_commit: closure.integrated_commit.clone(),
                });
                break;
            }
            prior = store.state(
                closure
                    .superseded_by
                    .as_deref()
                    .context("prerequisite closed without integration")?,
            )?;
        }
    }
    let selected_verdict_provenance = approval
        .verdict_event_id
        .as_ref()
        .and_then(|id| state.verdicts.iter().find(|v| &v.event_id == id))
        .map(
            |verdict| crate::import_authorization::SelectedVerdictProvenance {
                origin_replica_id: verdict.origin_replica_id.clone(),
                admission_provenance: state
                    .privilege_scope
                    .as_ref()
                    .map(|scope| scope.provenance(&verdict.event_id))
                    .unwrap_or("legacy-unknown")
                    .into(),
                receipt: state
                    .privilege_scope
                    .as_ref()
                    .and_then(|scope| scope.admission_for(&verdict.event_id))
                    .cloned(),
            },
        );
    ctx.ensure_target_declares_actor(state)?;
    Ok(AuthorizationBasis {
        selected_verdict_provenance,
        verdict_event_id: approval.verdict_event_id,
        external_verdict: approval.external_verdict,
        verdict_provisional: approval.verdict_provisional,
        audit_debt_event_id: approval.audit_debt_event_id,
        gate_evidence: report
            .gates
            .iter()
            .map(|gate| {
                Ok((
                    gate.name.clone(),
                    gate.evidence_event_id
                        .clone()
                        .context("green gate has no evidence")?,
                ))
            })
            .collect::<Result<_>>()?,
        prerequisites,
        provisional_prerequisites: provisional,
        blocking_findings: report.open_blocking_findings.clone(),
        holds: report
            .holds
            .iter()
            .map(|hold| hold.hold_event_id.clone())
            .collect(),
        gates: declarations.gates,
        policy: declarations.policy,
        danger: Some(report.danger.clone()),
    })
}

fn plan(
    ctx: &Ctx,
    store: &Store,
    state: &ChangeState,
    selection: &Selection,
    expected: Option<&IntegrationPlan>,
) -> Result<std::result::Result<integration::ProvisionalPlan, i32>> {
    let (report, declarations, provisional) = report(ctx, store, state, selection)?;
    let authorization = basis(
        ctx,
        store,
        state,
        &report,
        declarations.clone(),
        provisional,
    );
    let (confirmation, confirmed_declarations, provisional) =
        self::report(ctx, store, state, selection)?;
    let confirmed = basis(
        ctx,
        store,
        state,
        &confirmation,
        confirmed_declarations,
        provisional,
    );
    let patchset = state.latest_patchset();
    let mut completed_provisional = None;
    for event in state.provisional_integrations.iter().rev() {
        if event.repository_id != store.repository_id {
            continue;
        }
        if let Payload::ChangeProvisionallyIntegrated {
            source_patchset_id,
            source_head,
            staging_branch,
            provisional_commit,
            recipient,
            publication_target,
            ..
        } = &event.payload
        {
            if patchset.is_some_and(|patchset| {
                *source_patchset_id == patchset.id && *source_head == patchset.head
            }) && *staging_branch == selection.branch
                && *recipient == selection.recipient
                && *publication_target == state.target_branch
                && gitio::is_ancestor(&ctx.cwd, provisional_commit, &selection.before)?
            {
                completed_provisional = Some(integration::ProvisionalCompletion {
                    event_id: event.event_id.clone(),
                    commit: provisional_commit.clone(),
                });
                break;
            }
        }
    }
    let facts = IntegrationFacts {
        mode: Mode::Provisional,
        mode_refusal: None,
        publication_target: state.target_branch.clone(),
        recipient: Some(selection.recipient.clone()),
        completed_provisional,
        lifecycle_allows_integration: report.state == "open"
            && !state.iterating
            && report.fork.is_none(),
        shape: Shape::NoFf,
        target_is_ancestor: Ok(true),
        target: selection.branch.clone(),
        approved: patchset.map(|patchset| integration::Approved {
            patchset_id: patchset.id.clone(),
            head: patchset.head.clone(),
        }),
        ready: report.integrate_ready(),
        approval: integration::approval(state, &report, patchset.map_or("", |p| &p.id)),
        declarations: Ok(declarations),
        basis: authorization,
        confirmation: Ok(integration::Confirmation {
            ready: confirmation.integrate_ready(),
            basis: confirmed,
        }),
        contribution: Ok(None),
        checkout: Err(anyhow::anyhow!("provisional staging has no checkout")),
        tracked_dirt: Ok(false),
        target_revision: Ok(selection.before.clone()),
        evaluated_tree: patchset
            .context("no patchset recorded")
            .and_then(|patchset| {
                gitio::merge_outcome(&ctx.cwd, &selection.before, &patchset.head).map(|m| m.tree)
            }),
        write_collisions: Ok(Vec::new()),
        untouched: Ok(Vec::new()),
        already_contained: Ok(false),
    };
    let decision = integration::decide(facts, expected);
    if let Some(moved) = integration::describe_moved(&decision.moved) {
        if matches!(decision.decision, Decision::Refused(_)) {
            eprintln!("{moved}");
        } else {
            eprintln!("warning: {moved}");
        }
    }
    match decision.decision {
        Decision::Provisional(plan) => Ok(Ok(plan)),
        Decision::Refused(Refusal::NotReady) => {
            eprint!("{}", render::blocker_explanation(state, &report));
            Ok(Err(status::check_exit_code(&report)))
        }
        Decision::Refused(Refusal::Diverged) => {
            let patchset = patchset.context("no patchset recorded")?;
            eprintln!("conflicting staging merge: {} and {}; run git rebase {} in the change checkout, then arc snapshot {} --verify and obtain fresh review or debt", selection.before, patchset.head, selection.branch, state.change_id);
            Ok(Err(11))
        }
        Decision::Refused(error) => Err(error.into_error()),
        _ => bail!("provisional decision produced a canonical effect"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Intent {
    pub event: Event,
    pub old_staging: Option<String>,
}

fn finish(ctx: &Ctx, store: &Store, intent: &Intent) -> Result<()> {
    let Payload::ChangeProvisionallyIntegrated {
        staging_branch,
        staging_before,
        source_head,
        provisional_commit,
        evaluated_tree,
        ..
    } = &intent.event.payload
    else {
        bail!("provisional intent has the wrong event kind");
    };
    let raw = gitio::git(&ctx.cwd, &["cat-file", "commit", provisional_commit])?;
    let commit = gitio::read_commit(&ctx.cwd, provisional_commit)?;
    if commit.parents != [staging_before, source_head].map(Clone::clone)
        || commit.tree != *evaluated_tree
        || raw
            .lines()
            .take_while(|line| !line.is_empty())
            .any(|line| line.starts_with("gpgsig"))
    {
        bail!("provisional commit {provisional_commit} has incorrect parents/tree or a signature");
    }
    if checked_out_staging(&ctx.cwd, staging_branch)?.is_some() {
        bail!("staging ref refs/heads/{staging_branch} is checked out; intent retained");
    }
    let staging = format!("refs/heads/{staging_branch}");
    let retention =
        crate::provisional::retention_ref(&intent.event.change_id, &intent.event.event_id);
    if store.event_is_published(&intent.event)? {
        store.publish_provisional_event(&intent.event)?;
        store.clear_provisional_intent(&intent.event.change_id)?;
        println!("recovered completed provisional obligation: {}; merge {provisional_commit}; current staging work is preserved", intent.event.event_id);
        return Ok(());
    }
    let held = gitio::ref_value(&ctx.cwd, &staging)?;
    let kept = gitio::ref_value(&ctx.cwd, &retention)?;
    if held.as_deref() == Some(provisional_commit) && kept.as_deref() == Some(provisional_commit) {
        // Both refs prove the exact transition completed.
    } else if held == intent.old_staging && kept.is_none() {
        gitio::update_refs(
            &ctx.cwd,
            &[
                gitio::RefUpdate {
                    name: staging,
                    old: intent
                        .old_staging
                        .clone()
                        .unwrap_or_else(|| "0".repeat(provisional_commit.len())),
                    new: provisional_commit.clone(),
                },
                gitio::RefUpdate {
                    name: retention,
                    old: "0".repeat(provisional_commit.len()),
                    new: provisional_commit.clone(),
                },
            ],
        )?;
    } else {
        bail!("provisional intent {} conflicts with observed refs: staging={held:?}, retention={kept:?}; competing work is preserved", intent.event.event_id);
    }
    store.publish_provisional_event(&intent.event)?;
    store.clear_provisional_intent(&intent.event.change_id)?;
    println!("provisionally integrated: {provisional_commit}\nstaging: refs/heads/{staging_branch}\nobligation: {}", intent.event.event_id);
    Ok(())
}

pub fn integrate_provisionally(
    ctx: &Ctx,
    references: &[String],
    args: IntegrateArgs,
) -> Result<i32> {
    if references.len() != 1 || !args.tags.is_empty() {
        eprintln!("--provisional requires one change; queues and --tag conflict");
        return Ok(2);
    }
    if args.shape == Some(Shape::FfOnly) || args.cleanup == Some(true) || args.discard.is_some() {
        eprintln!("--provisional conflicts with --shape ff-only, --cleanup and --discard");
        return Ok(2);
    }
    if args.for_replica.is_some() && args.for_forge.is_some() {
        bail!("--for-replica conflicts with --for-forge");
    }
    let store = ctx.store()?;
    let _authority = crate::replica::lock(&store)?;
    let (_, initial) = ctx.load_state(&store, &references[0])?;
    let selection = match select(
        ctx,
        &store,
        &initial,
        args.into.as_deref(),
        args.for_replica.as_deref(),
        args.for_forge.as_deref(),
    )? {
        Ok(s) => s,
        Err((code, refusal)) => {
            eprintln!("Cannot prepare provisional integration: {refusal}");
            return Ok(code);
        }
    };
    let _target = store.lock_target(&selection.branch)?;
    let _transition = store.lock_transition(&initial.change_id)?;
    let mut state = store.state(&initial.change_id)?;
    if let Some(intent) = store.provisional_intent(&state.change_id)? {
        if let Payload::ChangeProvisionallyIntegrated {
            staging_branch,
            recipient,
            ..
        } = &intent.event.payload
        {
            if *staging_branch != selection.branch
                || (args.for_replica.is_some() || args.for_forge.is_some())
                    && *recipient != selection.recipient
            {
                bail!("requested staging/recipient differs from retained provisional intent {}; retry its exact destination", intent.event.event_id);
            }
        }

        if args.dry_run {
            bail!(
                "provisional intent {} is unfinished; retry arc integrate {} --provisional",
                intent.event.event_id,
                state.change_id
            );
        }
        finish(ctx, &store, &intent)?;
        return Ok(0);
    }
    if state.is_closed() {
        eprintln!(
            "change {} is closed; use arc replica provisional resolve for reconciliation",
            state.change_id
        );
        return Ok(6);
    }
    if !args.dry_run {
        ctx.ensure_declared_actor(&store)?;
        ctx.ensure_target_declares_actor(&state)?;
        if state.iterating {
            eprintln!(
                "change declares it is iterating; clear it with arc iterating {} --off",
                state.change_id
            );
            return Ok(13);
        }
        if fork::fork_slug_for_branch(&ctx.cwd, &state.branch)?.is_none() {
            if let Some(debt) = args.debt {
                super::audit::declare_debt_locked(ctx, &store, &state, &debt.reason, debt.kind)?;
                state = store.state(&state.change_id)?;
            }
        }
    }
    let expected = args
        .expect_basis
        .map(|path| -> Result<IntegrationPlan> {
            let plan: IntegrationPlan = serde_json::from_slice(&std::fs::read(&path)?)?;
            if plan.schema != integration::PLAN_SCHEMA
                || plan.change_id != state.change_id
                || plan.mode != Mode::Provisional
            {
                bail!("expected basis is not this change's provisional plan");
            }
            Ok(plan)
        })
        .transpose()?;
    let plan = match plan(ctx, &store, &state, &selection, expected.as_ref())? {
        Ok(plan) => plan,
        Err(code) => return Ok(code),
    };
    if args.dry_run {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&IntegrationPlan::of_provisional(
                    &state.change_id,
                    &plan
                ))?
            );
        } else if let Some(reuse) = &plan.reuse {
            println!(
                "would reuse provisional obligation {}; merge {}; staging refs remain at {}",
                reuse.event_id, reuse.commit, plan.staging_before
            );
        } else {
            println!("would prepare unsigned no-ff merge into refs/heads/{}; publication target {}; recipient {:?}; obligation remains open", plan.staging_branch, plan.publication_target, plan.recipient);
        }
        return Ok(0);
    }
    if let Some(reuse) = &plan.reuse {
        println!(
            "reused provisional obligation: {}\nprovisionally integrated: {}",
            reuse.event_id, reuse.commit
        );
        return Ok(0);
    }
    let message = args
        .message
        .unwrap_or_else(|| format!("merge({}): {}", state.slug, state.title));
    let commit = gitio::commit_tree_unsigned(
        &ctx.cwd,
        &plan.evaluated_tree,
        &[&plan.staging_before, &plan.approved.head],
        &message,
    )?;
    let fresh = select(
        ctx,
        &store,
        &state,
        Some(&selection.branch),
        args.for_replica.as_deref(),
        args.for_forge.as_deref(),
    )?
    .map_err(|(_, reason)| anyhow::anyhow!(reason))?;
    if fresh.before != selection.before
        || fresh.held != selection.held
        || fresh.recipient != selection.recipient
        || fresh.authority != selection.authority
        || gitio::branch_head(&ctx.cwd, &state.branch)? != plan.approved.head
    {
        bail!("provisional basis/ref moved before publication; no ref moved");
    }
    let confirmed = match self::plan(ctx, &store, &state, &fresh, None)? {
        Ok(plan) => plan,
        Err(code) => return Ok(code),
    };
    if confirmed != plan {
        bail!("provisional authorization basis moved before publication; no ref moved");
    }
    let event = ctx.event(
        &store,
        &state.change_id,
        Payload::ChangeProvisionallyIntegrated {
            project_id: selection.project,
            source_patchset_id: plan.approved.patchset_id,
            source_head: plan.approved.head,
            publication_target: plan.publication_target,
            staging_branch: plan.staging_branch,
            staging_before: plan.staging_before,
            provisional_commit: commit,
            evaluated_tree: plan.evaluated_tree,
            recipient: plan.recipient,
            authority_observed: selection.authority,
            authorization: plan.authorization,
        },
    );
    let event = event?;
    let intent = Intent {
        event,
        old_staging: selection.held,
    };
    store.write_provisional_intent(&intent)?;
    finish(ctx, &store, &intent)?;
    Ok(0)
}

pub fn check_provisional(
    ctx: &Ctx,
    reference: &str,
    into: Option<String>,
    explain: bool,
    json: bool,
    shape: Option<Shape>,
) -> Result<i32> {
    if shape == Some(Shape::FfOnly) {
        eprintln!("--provisional conflicts with --shape ff-only");
        return Ok(2);
    }
    let store = ctx.store()?;
    let (_, state) = ctx.load_state(&store, reference)?;
    let selection = select(ctx, &store, &state, into.as_deref(), None, None)?;
    let (code, report, refusal) = match selection {
        Ok(selection) => {
            let (report, _, _) = report(ctx, &store, &state, &selection)?;
            (status::check_exit_code(&report), Some(report), None)
        }
        Err((code, refusal)) => (code, None, Some(refusal)),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "schema":"arc-check/4", "change_id": state.change_id, "mode":"provisional", "ready":code==0, "exit_code":code, "refusal":refusal, "blockers":report.as_ref().map(|r| r.blockers()), "provisional_prerequisites":report.as_ref().map(|r| &r.provisional_prerequisites) })
            )?
        );
    } else if let Some(refusal) = refusal {
        eprintln!("Cannot prepare provisional integration: {refusal}");
    } else if let Some(report) = report {
        if code != 0 || explain {
            print!("{}", render::check_explanation(&state, &report));
        } else {
            println!("ready for unsigned provisional preparation");
        }
    }
    Ok(code)
}

pub fn resolve(
    ctx: &Ctx,
    reference: &str,
    integration: &str,
    selected: &[String],
    reason: &str,
) -> Result<i32> {
    if reason.trim().is_empty() {
        bail!("--because must state a reconciliation reason");
    }
    let store = ctx.store()?;
    let _authority = crate::replica::lock(&store)?;
    if let Some(refusal) = crate::replica::integration_refusal(&store)? {
        eprintln!("Cannot resolve provisional integration: {refusal}");
        return Ok(17);
    }
    let Some(grant) = crate::replica::observed_authority(&store)? else {
        eprintln!("resolution requires a paired current authority grant; this store is unpaired");
        return Ok(17);
    };
    let (_, state) = ctx.load_state(&store, reference)?;
    let _transition = store.lock_transition(&state.change_id)?;
    let state = store.state(&state.change_id)?;
    let events = store.load_events(&state.change_id)?;
    let accepting = events
        .iter()
        .find(|event| event.event_id == integration)
        .with_context(|| format!("missing integration resolution reference {integration}"))?;
    if let Payload::ChangeIntegrated {
        authority_grant_event_id: Some(grant),
        ..
    } = &accepting.payload
    {
        if crate::replica::grant_holder(&store, grant)?.as_deref() != Some(&accepting.repository_id)
        {
            bail!("integration {integration} names an invalid authority grant {grant}");
        }
    }
    if state.closure.as_ref().is_none_or(|closure| {
        closure.event_id != integration || closure.outcome != Closure::Integrated
    }) {
        bail!(
            "{integration} is not the valid accepting integration of {}",
            state.change_id
        );
    }
    if state.privilege_scope.as_ref().is_some_and(|scope| {
        !scope.permits(
            &accepting.event_id,
            &accepting.repository_id,
            crate::import_authorization::Role::Integration,
        )
    }) {
        bail!("integration {integration} lacks admitted integration capability");
    }
    let selected = if selected.is_empty() {
        crate::provisional::unresolved(&state)
            .iter()
            .map(|row| row.event_id.clone())
            .collect()
    } else {
        selected.to_vec()
    };
    let resolutions = selected
        .iter()
        .map(|id| {
            crate::provisional::validate_resolution(&state, id, accepting, &accepting.repository_id)
                .map(|basis| (id, basis))
        })
        .collect::<Result<Vec<_>>>()?;
    for (id, (patchset, kind)) in resolutions {
        if crate::provisional::project_obligations(&state)
            .iter()
            .any(|row| {
                row.event_id == *id && row.integration_event_id.as_deref() == Some(integration)
            })
        {
            println!("provisional obligation {id} is already resolved by {integration}");
            continue;
        }
        let event = ctx.event(
            &store,
            &state.change_id,
            Payload::ProvisionalIntegrationResolved {
                provisional_event_id: id.clone(),
                integration_event_id: integration.into(),
                accepted_patchset_id: patchset,
                authority_grant_event_id: grant.grant_event_id.clone(),
                kind,
                reason: reason.into(),
            },
        );
        let event = event?;
        store.append_event(&event)?;
        if crate::provisional::project_obligations(&state)
            .iter()
            .any(|row| {
                row.event_id == *id && matches!(row.recipient, ProvisionalRecipient::Forge { .. })
            })
        {
            println!("resolved by assertion: provisional obligation {id} by {integration}");
        } else {
            println!("resolved provisional obligation {id} by {integration}");
        }
    }
    Ok(0)
}
