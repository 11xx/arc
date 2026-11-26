//! The required-set scheduler shares command execution and receipt recording.
use super::*;
use crate::operation_println as println;

pub(super) fn mode(required: &[(&String, &gates::Gate)]) -> VerificationRunMode {
    if required.iter().any(|(_, gate)| gate.concurrent) {
        VerificationRunMode::Declared
    } else {
        VerificationRunMode::Sequential
    }
}

pub(super) fn run_required(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    required: Vec<(&String, &gates::Gate)>,
    mut input: impl FnMut(&String, &gates::Gate) -> VerificationInput,
) -> Result<usize> {
    if required.iter().any(|(_, gate)| gate.concurrent) {
        let state = store.state(change_id)?;
        let declarations = ctx.declarations(&state)?;
        let complete = declarations.gates.required_for(&state.profile);
        if let Some(total) = declarations
            .gates
            .budget
            .as_ref()
            .and_then(|rules| rules.total)
            .filter(|_| {
                crate::gate_concurrency::accepted(&complete).is_some()
                    && required.len() < complete.len()
            })
        {
            anyhow::ensure!(crate::gate_concurrency::total(&required).is_none_or(|cost| cost <= total),
                "remaining gates run alone and their budgets exceed the total; measure this exact batch or use --rerun to execute the full accepted batch");
        }
    }
    let errors = crate::gate_concurrency::lock_errors(required.iter().copied());
    anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
    if required.iter().any(|(_, gate)| gate.concurrent) {
        store.adopt_concurrency()?;
    }
    let (alone, concurrent): (Vec<_>, Vec<_>) =
        required.into_iter().partition(|(_, gate)| !gate.concurrent);
    let mut passed = 0;
    for (name, gate) in alone {
        passed += usize::from(record_verification(ctx, store, change_id, input(name, gate))? == 0);
    }
    let acceptance = crate::gate_concurrency::accepted(&concurrent)
        .filter(|accepted| {
            crate::gate_concurrency::load(&ctx.cwd)
                .is_some_and(|current| current.same_claim(accepted))
        })
        .cloned();
    if let Some(acceptance) = acceptance {
        let inputs = concurrent
            .iter()
            .map(|(name, gate)| input(name, gate))
            .collect();
        passed += batch(ctx, store, change_id, inputs, acceptance)?;
    } else {
        for (name, gate) in concurrent {
            eprintln!("gate {name}: running alone; concurrent batch has no current local measurement for this exact membership (a batch of one runs alone)");
            passed +=
                usize::from(record_verification(ctx, store, change_id, input(name, gate))? == 0);
        }
    }
    Ok(passed)
}

fn batch(
    ctx: &Ctx,
    store: &Store,
    change_id: &str,
    inputs: Vec<VerificationInput>,
    acceptance: crate::gate_concurrency::Acceptance,
) -> Result<usize> {
    store.adopt_concurrency()?;
    let revision = gitio::fresh_read(|| gitio::head(&ctx.cwd))?;
    let before = gitio::worktree_tree(&ctx.cwd)?;
    let dirt = gitio::dirt(&ctx.cwd)?;
    let dirty =
        dirt.tracked || dirt.untracked || before != gitio::commit_tree(&ctx.cwd, &revision)?;
    let receipt = crate::gate_concurrency::Receipt::new(&acceptance)?;
    eprintln!(
        "running concurrent batch {}: {}",
        receipt.batch_id,
        inputs
            .iter()
            .filter_map(|input| input.gate.as_deref())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut completed = std::thread::scope(|scope| {
        let handles = inputs
            .into_iter()
            .map(|input| {
                let revision = revision.clone();
                scope.spawn(move || {
                    execute_verification(
                        input,
                        &ctx.cwd,
                        revision,
                        super::super::local_hostname(),
                        &store.root,
                        &store.repository_id,
                        change_id,
                        false,
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut completed = Vec::new();
        let mut first_error = None;
        for handle in handles {
            match handle
                .join()
                .map_err(|_| anyhow::anyhow!("gate worker panicked"))
                .and_then(|result| result)
            {
                Ok(item) => completed.push(item),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(completed), Err)
    })?;
    let moved = gitio::worktree_tree(&ctx.cwd)? != before
        || gitio::fresh_read(|| gitio::head(&ctx.cwd))? != revision;
    let current_state = store.state(change_id)?;
    let current = ctx.declarations(&current_state)?;
    let local = crate::gate_concurrency::load(&ctx.cwd);
    let accepted = local
        .as_ref()
        .is_some_and(|local| local.reference().same_claim(&receipt.acceptance));
    let mut passed = 0;
    let mut work_references = crate::gate_work::References::default();
    for item in &mut completed {
        item.tested_tree = Some(before.clone());
        item.worktree_dirty = Some(dirty);
        item.worktree_dirty_tracked = Some(dirt.tracked);
        item.worktree_dirty_untracked = Some(dirt.untracked);
        item.tree_moved = moved;
        item.timing.concurrent_batch = Some(receipt.clone());
        crate::gate_work::refuse_unattributed_batch(&mut item.timing);
        attach_work_reference(&mut work_references, store, &ctx.cwd, item)?;
        let gate = item
            .gate
            .as_ref()
            .and_then(|name| current.gates.gates.get(name));
        let judgement = if item.landing.is_some() {
            item.timing.budget_at_run.clone()
        } else {
            gate.map(|gate| Box::new(gate.budget_judgement(&item.timing)))
        };
        item.display_budget = judgement.clone();
        if let Some(reason) = judgement
            .as_ref()
            .and_then(|budget| budget.reason(&item.timing, &item.hostname))
        {
            println!(
                "gate {}: {reason}",
                item.gate.as_deref().unwrap_or("command")
            );
        }
        let declaration_ok = gate.is_some_and(|gate| {
            Some(gates::declaration_identity(gate)).as_ref() == item.declaration.as_ref()
        });
        passed += usize::from(
            item.result == VerifyResult::Pass
                && !dirty
                && !moved
                && accepted
                && declaration_ok
                && judgement.as_ref().is_none_or(|budget| budget.accepted()),
        );
    }
    if dirty || moved || !accepted {
        eprintln!("concurrent batch is not green: dirty before={dirty}, moved={moved}, local acceptance current={accepted}");
    }
    append_verifications(ctx, store, change_id, completed)?;
    Ok(passed)
}
