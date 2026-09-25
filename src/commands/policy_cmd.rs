//! Read and write repository-local operator policy.

use crate::commands::Ctx;
use crate::gitio;
use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

pub fn path(ctx: &Ctx) -> Result<i32> {
    let top = gitio::toplevel(&ctx.cwd)?;
    println!("{}", crate::policy::operator_path(&top)?.display());
    Ok(0)
}

pub fn write(ctx: &Ctx, text: &str) -> Result<i32> {
    crate::policy::validate_operator_text(text)?;
    crate::gates::validate_operator_text(text)?;
    let top = gitio::toplevel(&ctx.cwd)?;
    let path = crate::policy::operator_path(&top)?;
    write_atomically(&path, text.as_bytes())?;
    println!("operator policy written: {}", path.display());
    Ok(0)
}

pub fn show(ctx: &Ctx) -> Result<i32> {
    let top = gitio::toplevel(&ctx.cwd)?;
    let policy = crate::policy::load(&top)?;
    let gates = crate::gates::inspect(&top)?;
    let operator_path = crate::policy::operator_path(&top)?;
    println!("operator policy: {}", operator_path.display());
    println!("effective policy:");
    let forbid_self_approval = policy.policy.forbid_self_approval.to_string();
    print_rule(
        "policy.forbid_self_approval",
        &forbid_self_approval,
        &format!("policy.forbid_self_approval={forbid_self_approval}"),
        &policy.sources,
    );
    let require_declared_actor = policy.policy.require_declared_actor.to_string();
    print_rule(
        "policy.require_declared_actor",
        &require_declared_actor,
        &format!("policy.require_declared_actor={require_declared_actor}"),
        &policy.sources,
    );
    let debt_count_threshold = option_value(policy.policy.debt_count_threshold);
    print_rule(
        "policy.debt_count_threshold",
        &debt_count_threshold,
        &format!("policy.debt_count_threshold={debt_count_threshold}"),
        &policy.sources,
    );
    let debt_age_threshold = option_value(policy.policy.debt_age_threshold_seconds);
    print_rule(
        "policy.debt_age_threshold_seconds",
        &debt_age_threshold,
        &format!("policy.debt_age_threshold_seconds={debt_age_threshold}"),
        &policy.sources,
    );
    let free_floor = option_value(policy.policy.worktree_free_floor_bytes);
    print_rule(
        "policy.worktree_free_floor_bytes",
        &free_floor,
        &format!("policy.worktree_free_floor_bytes={free_floor}"),
        &policy.sources,
    );
    let provenance = policy.provenance.git_identity.as_str();
    print_rule(
        "provenance.git_identity",
        provenance,
        &format!("provenance.git_identity={provenance}"),
        &policy.sources,
    );
    for item in &policy.review.checklist {
        print_rule(
            &format!("review.checklist[{item:?}]"),
            item,
            &format!("review.checklist[{item:?}]"),
            &policy.sources,
        );
    }
    for pattern in &policy.danger.paths {
        print_rule(
            &format!("danger.paths[{pattern:?}]"),
            "dangerous",
            &format!("danger.paths[{pattern:?}]"),
            &policy.sources,
        );
    }
    for pattern in &policy.danger.acknowledged_safe {
        print_rule(
            &format!("danger.acknowledged_safe[{pattern:?}]"),
            "acknowledged safe",
            &format!("danger.acknowledged_safe[{pattern:?}]"),
            &policy.sources,
        );
    }
    for root in &policy.danger.source_roots {
        print_rule(
            &format!("danger.source_roots[{root:?}]"),
            "closed classification root",
            &format!("danger.source_roots[{root:?}]"),
            &policy.sources,
        );
    }
    let effective_scalars = [
        format!("policy.forbid_self_approval={forbid_self_approval}"),
        format!("policy.require_declared_actor={require_declared_actor}"),
        format!("policy.debt_count_threshold={debt_count_threshold}"),
        format!("policy.debt_age_threshold_seconds={debt_age_threshold}"),
        format!("policy.worktree_free_floor_bytes={free_floor}"),
        format!("provenance.git_identity={provenance}"),
    ];
    for (rule, sources) in policy.sources.as_map() {
        if (rule.starts_with("policy.") || rule.starts_with("provenance."))
            && !effective_scalars.contains(&rule)
        {
            println!("declaration {rule}: declared by {}", sources.join(", "));
        }
    }
    for (name, gate) in &gates.gates {
        println!(
            "gate {name}: command = {:?}; profiles = {}; timeout = {}; declared by {}",
            gate.command,
            if gate.profiles.is_empty() {
                "all".to_string()
            } else {
                gate.profiles.join(", ")
            },
            option_value(gate.timeout),
            gate.declared_by.join(", ")
        );
    }
    for conflict in &gates.conflicts {
        let declarations = conflict
            .declarations
            .iter()
            .map(crate::gates::GateDeclaration::describe)
            .collect::<Vec<_>>()
            .join("; ");
        println!("gate conflict {}: {declarations}", conflict.name);
    }
    Ok(0)
}

fn print_rule(name: &str, value: &str, source_key: &str, sources: &crate::policy::PolicySources) {
    let source = sources.sources_for(source_key);
    if source.is_empty() {
        println!("  {name} = {value} (Arc default)");
    } else {
        println!("  {name} = {value} (declared by {})", source.join(", "));
    }
}

fn option_value<T: std::fmt::Display>(value: Option<T>) -> String {
    value
        .map(|item| item.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("operator policy path has no parent")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .or_else(|error| if parent.is_dir() { Ok(()) } else { Err(error) })
        .with_context(|| format!("cannot create {}", parent.display()))?;

    let temp = path.with_file_name(format!(
        ".operator-policy-{}.tmp",
        crate::ids::new_event_id()
    ));
    let publish = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .with_context(|| format!("cannot create {}", temp.display()))?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
            .with_context(|| format!("cannot publish operator policy at {}", path.display()))?;
        File::open(parent)
            .with_context(|| format!("cannot open {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("cannot sync {}", parent.display()))?;
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    publish
}
