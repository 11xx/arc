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
    let gates = gates::load_at(&ctx.cwd, &revision)?;
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
                ..
            } if previous_target == target && previous_profile == profile => Some(revision.clone()),
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
        let mut output_artifact = None;
        let result = if reused && clean_before {
            println!("gate {name}: skipped (publication pass at target tree)");
            VerifyResult::Pass
        } else {
            let run = super::gatekeeping::run_gate(
                &gate.command,
                &scratch.path,
                gate.timeout,
                &store.root,
                &store.repository_id,
            )?;
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
        println!("publication gate {name}: {}", result.as_str());
        passed &= result == VerifyResult::Pass;
        not_admitted |= result == VerifyResult::NotAdmitted;
        failed |= result == VerifyResult::Fail;
        results.insert(
            name.clone(),
            PublicationGateResult {
                output_artifact,
                command: gate.command.clone(),
                timeout_seconds: gate.timeout,
                environment,
                result,
            },
        );
    }
    match gates::load_at(&ctx.cwd, &revision).and_then(|current| {
        declarations_identity(&current.required_for_event(profile, GateEvent::Publication))
    }) {
        Ok(current) if current == declarations => {}
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
    };
    {
        let _events = store.lock_repository_events()?;
        let mut event = ctx.event(&store, Store::REPOSITORY_SCOPE, payload)?;
        store.append_local_repository_event(&mut event)?;
        println!("publication event: {}", event.event_id);
    }
    if failed {
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
    Ok(if passed && !failed { 0 } else { 1 })
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
