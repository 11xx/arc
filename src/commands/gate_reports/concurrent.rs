//! Five clean rounds establish acceptance for one exact host-local batch.
use super::*;
use crate::operation_println as println;

pub(super) fn measure(ctx: &Ctx, runs: u32, selected: Option<&str>) -> Result<i32> {
    anyhow::ensure!(
        runs == 5 && selected.is_none(),
        "--concurrent requires five rounds of the complete batch; --gate is standalone"
    );
    let root = gitio::toplevel(&ctx.cwd)?;
    let declared = gates::inspect(&root)?;
    declared.ensure_unconflicted()?;
    let required = declared
        .gates
        .iter()
        .filter(|(_, gate)| gate.concurrent)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        required.len() >= 2,
        "concurrent measurement needs at least two concurrent gates"
    );
    let host = crate::gate_concurrency::host()
        .context("host identity unavailable; cannot accept concurrency")?;
    let store = ctx.store()?;
    crate::gate_concurrency::revoke(&store, &host)?;
    crate::warm_worktree::prepare(&root)?;
    let revision = gitio::head(&root)?;
    let tree = gitio::commit_tree(&root, &revision)?;
    let dirt = gitio::dirt(&root)?;
    anyhow::ensure!(
        !dirt.tracked && !dirt.untracked && gitio::worktree_tree(&root)? == tree,
        "concurrent measurement requires a clean worktree"
    );
    let members = crate::gate_concurrency::members(required.iter().copied());
    println!(
        "concurrent measurement: {} physical cores, {} logical cores",
        crate::verification_timing::physical_core_count()
            .map_or_else(|| "unknown".into(), |n| n.to_string()),
        std::thread::available_parallelism().map_or_else(|_| "unknown".into(), |n| n.to_string())
    );
    let mut rounds = Vec::new();
    let mut wall_by_gate: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut batch_wall = Vec::new();
    let mut successful = true;
    for round in 1..=5 {
        let started = Instant::now();
        let results = std::thread::scope(|scope| {
            let handles = required
                .iter()
                .map(|(name, gate)| {
                    let root = &root;
                    scope.spawn(move || {
                        super::super::gatekeeping::measure_gate(gate, name, root)
                            .map(|run| ((*name).clone(), run))
                    })
                })
                .collect::<Vec<_>>();
            let mut results = Vec::new();
            let mut first_error = None;
            for handle in handles {
                match handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("measurement worker panicked"))
                    .and_then(|result| result)
                {
                    Ok(run) => results.push(run),
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
            first_error.map_or(Ok(results), Err)
        })?;
        let elapsed = started.elapsed().as_millis() as u64;
        batch_wall.push(elapsed);
        println!(
            "concurrent round {round}/5: batch wall {}",
            ms(Some(elapsed))
        );
        let mut samples = BTreeMap::new();
        for (name, run) in results {
            let result =
                VerifyResult::observed(run.status.success(), run.status.code(), run.timed_out);
            let timing = run.timing;
            let passed =
                result == VerifyResult::Pass && !run.admission_expired && timing.run_ms.is_some();
            successful &= passed;
            println!(
                "{name} {round}/5: wall {}; CPU {}; wait {}; load {:?}; {}",
                ms(timing.run_ms),
                ms(timing
                    .cpu_user_ms
                    .zip(timing.cpu_system_ms)
                    .map(|(u, s)| u.saturating_add(s))),
                ms(timing.wait_ms),
                timing.load_average,
                if run.admission_expired {
                    "not measured (not-admitted)"
                } else {
                    result.as_str()
                }
            );
            wall_by_gate
                .entry(name.clone())
                .or_default()
                .extend(timing.run_ms);
            if !passed {
                if let Some(tail) = run.output_tail {
                    eprintln!("{name}: {tail}");
                }
            }
            samples.insert(
                name,
                crate::gate_concurrency::Sample {
                    passed,
                    run_ms: timing.run_ms,
                    cpu_user_ms: timing.cpu_user_ms,
                    cpu_system_ms: timing.cpu_system_ms,
                    wait_ms: timing.wait_ms,
                },
            );
        }
        let dirt = gitio::dirt(&root)?;
        anyhow::ensure!(
            !dirt.tracked
                && !dirt.untracked
                && gitio::head(&root)? == revision
                && gitio::worktree_tree(&root)? == tree,
            "worktree moved during concurrent measurement; no acceptance recorded"
        );
        let current = gates::inspect(&root)?;
        anyhow::ensure!(
            crate::gate_concurrency::members(current.gates.iter()) == members,
            "concurrent declarations changed during measurement; no acceptance recorded"
        );
        rounds.push(samples);
    }
    for (name, mut wall) in wall_by_gate {
        let summary = Distribution::of(&mut wall, 0);
        println!(
            "{name}: wall median {}, min {}, max {}, spread {}",
            ms(summary.median_ms),
            ms(wall.first().copied()),
            ms(summary.worst_ms),
            ms(wall.last().zip(wall.first()).map(|(max, min)| max - min))
        );
    }
    println!(
        "concurrent batch: min {}, max {}",
        ms(batch_wall.iter().min().copied()),
        ms(batch_wall.iter().max().copied())
    );
    if !successful {
        println!("concurrent batch not accepted: every gate must pass all five rounds");
        return Ok(1);
    }
    let mut acceptance = crate::gate_concurrency::Acceptance {
        id: String::new(),
        producing_store: store.repository_id.clone(),
        host,
        measured_at: chrono::Utc::now(),
        members,
        rounds,
    };
    crate::gate_concurrency::save(&store, &mut acceptance)?;
    println!(
        "concurrent batch accepted: {} at {}",
        acceptance.id, acceptance.measured_at
    );
    Ok(0)
}
