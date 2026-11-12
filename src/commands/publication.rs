//! Local publication checks bind complete gate receipts to a target revision.

use super::*;
use crate::model::{GateEvent, PublicationGateResult};
use sha2::{Digest, Sha256};

pub fn publication(
    ctx: &Ctx,
    target: &str,
    profile: &str,
    skip_green: bool,
    expected_revision: Option<&str>,
    associated_change: Option<&str>,
) -> Result<i32> {
    let revision = gitio::branch_head(&ctx.cwd, target)
        .with_context(|| format!("cannot resolve publication target {target:?}"))?;
    if let Some(expected) = expected_revision {
        if !matches!(expected.len(), 40 | 64)
            || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("--revision must be a full commit ID");
        }
        if !expected.eq_ignore_ascii_case(&revision) {
            bail!("publication target {target} is at {revision}, not the requested revision {expected}");
        }
    }
    let store = ctx.store()?;
    ctx.ensure_declared_actor(&store)?;
    let tree = gitio::commit_tree(&ctx.cwd, &revision)?;
    let (gates, authority) = publication_gates(ctx, &store, target, &revision, associated_change)?;
    if gates.has_budgets() {
        store.adopt_budgets()?;
    }
    let configuration = configuration_identity(&gates, profile, &authority)?;
    let required = gates.required_for_event(profile, GateEvent::Publication);
    if required.is_empty() {
        bail!("no publication gates declared for profile {profile}");
    }
    let declarations = declarations_identity(&required)?;
    let prior_events = store.load_repository_events()?;
    let previous = prior_events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::PublicationVerified {
                target: previous_target,
                revision,
                profile: previous_profile,
                result: VerifyResult::Pass,
                budget_accepted,
                ..
            } if previous_target == target
                && previous_profile == profile
                && budget_accepted.unwrap_or(true) =>
            {
                Some(revision.clone())
            }
            _ => None,
        });
    let reusable = prior_events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            Payload::PublicationVerified {
                target: previous_target,
                revision: previous_revision,
                tree: previous_tree,
                profile: previous_profile,
                gates,
                declarations: previous_declarations,
                result,
                ..
            } if previous_target == target
                && previous_tree == &tree
                && previous_profile == profile
                && previous_declarations == &declarations =>
            {
                Some((gates, result, previous_revision == &revision))
            }
            _ => None,
        });
    let path = super::lifecycle::worktree_path_for(
        &ctx.cwd,
        &format!("publication-{}", ids::new_identifier()),
    )?;
    let scratch = super::gatekeeping::ScratchWorktree::create(&ctx.cwd, path, &revision)?;
    let mut results = BTreeMap::new();
    let mut passed = true;
    // A gate that was not admitted ran nothing, so the guard is not a pass;
    // it is a failure only if some gate that did run failed.
    let mut not_admitted = false;
    let mut failed = false;
    let mut budget_refused = false;
    println!("publication target: {target} at {revision}");
    for (name, gate) in required {
        let environment = match gate.environment.as_deref() {
            Some(command) => {
                gates::environment_probe(&scratch.path, command, gate.timeout)?.identity
            }
            None => None,
        };
        let environment_known = gate.environment.is_none() || environment.is_some();
        if !environment_known {
            eprintln!("publication gate {name}: environment probe yielded no identity; a pass cannot count");
        }
        let reused = skip_green
            && environment_known
            && reusable.is_some_and(|(receipts, result, same_commit)| {
                *result == VerifyResult::Pass
                    && (same_commit || gate.binds == gates::Binding::Tree)
                    && receipts.get(name).is_some_and(|receipt| {
                        receipt.command == gate.command
                            && receipt.timeout_seconds == gate.timeout
                            && receipt.environment == environment
                            && receipt.result == VerifyResult::Pass
                    })
            });
        let clean_before = gitio::git(
            &scratch.path,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )?
        .is_empty();
        let output_artifact;
        let mut timing;
        let mut hostname = Some(super::local_hostname());
        let result = if reused && clean_before {
            let receipt = &reusable.expect("reusable publication").0[name];
            timing = receipt.timing.clone();
            hostname = receipt.hostname.clone();
            output_artifact = receipt.output_artifact.clone();
            println!("gate {name}: skipped (publication pass at target tree)");
            VerifyResult::Pass
        } else {
            let (limit, origin) = gate.execution_limit()?;
            let run = super::gatekeeping::run_gate_budgeted(
                &gate.command,
                &scratch.path,
                limit.map(Duration::from_millis),
                &store.root,
                &store.repository_id,
                crate::exclusive::Request::for_gate(gate)
                    .as_ref()
                    .map(|request| (request, "publication", name.as_str())),
            )?;
            timing = run.timing;
            timing.execution_limit_ms = limit;
            timing.execution_limit_origin = origin;
            timing.budget_at_run = gate.budget.map(|_| gate.budget_judgement(timing.run_ms));
            output_artifact = run.output_artifact;
            if let Some(tail) = run.output_tail {
                if !tail.is_empty() {
                    println!("{tail}");
                }
            }
            let clean_after = gitio::git(
                &scratch.path,
                &["status", "--porcelain=v1", "--untracked-files=normal"],
            )?
            .is_empty();
            match VerifyResult::observed(run.status.success(), run.status.code(), run.timed_out) {
                VerifyResult::Pass
                    if !(clean_before
                        && clean_after
                        && environment_known
                        && gitio::head(&scratch.path)? == revision) =>
                {
                    VerifyResult::Fail
                }
                observed => observed,
            }
        };
        let judgement = gate.budget_judgement(timing.run_ms);
        if let Some(reason) = judgement.reason(&timing, hostname.as_deref().unwrap_or("unrecorded"))
        {
            eprintln!("publication gate {name}: {reason}");
            budget_refused = true;
        }
        println!("publication gate {name}: {}", result.as_str());
        passed &= result == VerifyResult::Pass;
        not_admitted |= result == VerifyResult::NotAdmitted;
        failed |= result == VerifyResult::Fail;
        results.insert(
            name.clone(),
            PublicationGateResult {
                timing,
                hostname,
                output_artifact,
                command: gate.command.clone(),
                timeout_seconds: gate.timeout,
                environment,
                result,
            },
        );
    }
    match publication_gates(ctx, &store, target, &revision, associated_change).and_then(
        |(current, authority)| {
            if current.has_budgets() {
                store.adopt_budgets()?;
            }
            configuration_identity(&current, profile, &authority)
        },
    ) {
        Ok(current) if current == configuration => {}
        Ok(_) => {
            failed = true;
            eprintln!("publication gate declarations changed while checks ran; rerun the guard");
        }
        Err(error) => {
            failed = true;
            eprintln!("publication gate declarations became unavailable: {error:#}");
        }
    }
    for (name, gate) in gates.required_for_event(profile, GateEvent::Publication) {
        if let Some(probe) = gate.environment.as_deref() {
            let current = gates::environment_probe(&scratch.path, probe, gate.timeout)?.identity;
            if current.is_none()
                || results
                    .get(name)
                    .and_then(|receipt| receipt.environment.as_ref())
                    != current.as_ref()
            {
                failed = true;
                eprintln!("publication gate {name}: environment changed while checks ran; rerun the guard");
            }
        }
    }
    if gitio::branch_head(&ctx.cwd, target)? != revision {
        failed = true;
        eprintln!("publication target moved while checks ran; rerun the guard");
    }
    let retention_ref = format!("refs/arc/publication/{}", ids::new_identifier());
    gitio::update_ref(&ctx.cwd, &retention_ref, &revision)?;
    let payload = Payload::PublicationVerified {
        retention_ref,
        target: target.to_owned(),
        revision: revision.clone(),
        tree,
        profile: profile.to_owned(),
        gates: results,
        result: if failed {
            VerifyResult::Fail
        } else if not_admitted {
            VerifyResult::NotAdmitted
        } else {
            VerifyResult::Pass
        },
        declarations,
        budget_accepted: gates.has_budgets().then_some(!budget_refused),
    };
    {
        let _events = store.lock_repository_events()?;
        let mut event = ctx.event(&store, Store::REPOSITORY_SCOPE, payload)?;
        store.append_local_repository_event(&mut event)?;
        println!("publication event: {}", event.event_id);
    }
    if failed || budget_refused {
        println!("integrations since last successful publication pass:");
        for (id, state) in store.readable_states()? {
            let Some(closure) = state.closure else {
                continue;
            };
            if closure.outcome != Closure::Integrated
                || closure.target_branch.as_deref() != Some(target)
            {
                continue;
            }
            let Some(commit) = closure.integrated_commit else {
                continue;
            };
            if gitio::is_ancestor(&ctx.cwd, &commit, &revision).unwrap_or(false)
                && !previous.as_deref().is_some_and(|previous| {
                    gitio::is_ancestor(&ctx.cwd, &commit, previous).unwrap_or(false)
                })
            {
                println!("  {id} {commit}");
            }
        }
    }
    Ok(if passed && !failed && !budget_refused {
        0
    } else {
        1
    })
}

fn declarations_identity(required: &[(&String, &gates::Gate)]) -> Result<String> {
    let declarations = serde_json::to_vec(
        &required
            .iter()
            .map(|(name, gate)| ((*name).clone(), gates::declaration_identity(gate)))
            .collect::<BTreeMap<_, _>>(),
    )?;
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(declarations))
    ))
}

/// Read budgets from an integration baseline or the destination of an associated
/// open change. An unaccepted branch cannot authorize its own budget declarations.
fn publication_gates(
    ctx: &Ctx,
    store: &Store,
    target: &str,
    revision: &str,
    associated_change: Option<&str>,
) -> Result<(gates::GatesFile, String)> {
    let own = gates::load_at(&ctx.cwd, revision)?;
    let states = ctx.load_all_states(store)?;
    let associated = if let Some(reference) = associated_change {
        let id = store.resolve_change(reference)?;
        Some(
            states
                .get(&id)
                .context("associated publication change is unavailable")?
                .clone(),
        )
    } else {
        let matching = states
            .values()
            .filter(|state| !state.is_closed() && state.branch == target)
            .collect::<Vec<_>>();
        if matching.len() > 1 {
            bail!("publication target has multiple associated changes; select one with --change");
        }
        matching.first().map(|state| (*state).clone())
    };
    let (mut required, authority) = if let Some(state) = associated {
        if state.is_closed()
            || state.branch != target
            || gitio::branch_head(&ctx.cwd, &state.branch)? != revision
        {
            bail!("publication --change must name the open change owning the target branch at this revision");
        }
        let declarations = ctx.declarations(&state)?;
        (declarations.gates, format!("change:{}", state.change_id))
    } else {
        let work_branches = states
            .values()
            .map(|state| state.branch.as_str())
            .collect::<BTreeSet<_>>();
        let destinations = states
            .values()
            .map(crate::declarations::accepted_target)
            .filter(|branch| !work_branches.contains(branch))
            .collect::<BTreeSet<_>>();
        let branch = if destinations.contains(target) {
            Some(target.to_string())
        } else {
            gitio::default_branch(&ctx.cwd)
                .filter(|branch| destinations.contains(branch.as_str()))
                .or_else(|| {
                    (destinations.len() == 1).then(|| destinations.first().unwrap().to_string())
                })
        };
        let default_head = branch
            .map(|branch| gitio::branch_head(&ctx.cwd, &branch))
            .transpose()?;
        let baseline = default_head.or_else(|| {
            states.values().find_map(|state| {
                state
                    .closure
                    .as_ref()
                    .filter(|closure| closure.outcome == Closure::Integrated)
                    .and_then(|closure| closure.integrated_commit.as_ref())
                    .filter(|commit| commit.as_str() == revision)
                    .cloned()
            })
        });
        if let Some(baseline) = baseline {
            (
                gates::load_at(&ctx.cwd, &baseline)?,
                format!("accepted:{baseline}"),
            )
        } else {
            let repository_budget = own
                .gates
                .values()
                .flat_map(|gate| &gate.budget_sources)
                .chain(own.budget.iter().flat_map(|rules| &rules.sources))
                .any(|source| source.source.starts_with(".arc/gates.toml at "));
            if repository_budget {
                bail!("publication budget authority is unavailable: target needs a recorded integration destination or an associated open change (--change)");
            }
            (gates::GatesFile::default(), "operator-only".into())
        }
    };
    if let Some(baseline) = authority.strip_prefix("accepted:") {
        crate::gate_raise::require_records(&ctx.cwd, baseline, revision, &mut required)?;
    }
    required.owe_also(own, "accepted publication baseline", "publication target");
    let errors = required.budget_errors();
    if !errors.is_empty() {
        bail!("invalid publication budgets: {}", errors.join("; "));
    }
    Ok((required, authority))
}

fn configuration_identity(
    gates: &gates::GatesFile,
    profile: &str,
    authority: &str,
) -> Result<String> {
    let required = gates.required_for_event(profile, GateEvent::Publication);
    let budgets = required
        .iter()
        .map(|(name, gate)| (name.as_str(), (gate.budget, &gate.budget_sources)))
        .collect::<BTreeMap<_, _>>();
    let bytes = serde_json::to_vec(&(
        authority,
        declarations_identity(&required)?,
        &gates.budget,
        budgets,
    ))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}
