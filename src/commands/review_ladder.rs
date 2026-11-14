//! Registry publication stays in the invoking change's checkout.

use super::Ctx;
use crate::custody::anchored::Dir;
use crate::{declarations, gitio, review_ladder};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

pub fn import(ctx: &Ctx, reference: &str, file: &Path) -> Result<i32> {
    let store = ctx.store()?;
    let id = store.resolve_change(reference)?;
    let _transition = store.lock_transition(&id)?;
    let state = store.state(&id)?;
    let top = gitio::toplevel(&ctx.cwd)?;
    let current = crate::context::infer_change(&store, &top)?;
    if current.as_deref() != Some(id.as_str())
        || gitio::fresh_read(|| gitio::current_branch(&top))?.as_deref() != Some(&state.branch)
    {
        bail!("review-ladder import requires the current open change's worktree; no other checkout is written");
    }
    ctx.ensure_target_declares_actor(&state)?;
    let text =
        fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let ladder = review_ladder::parse(&text)
        .with_context(|| format!("invalid review ladder {}", file.display()))?;
    let authority = declarations::authorizing_ladder(&ctx.cwd, &state, None)?;
    let text = toml::to_string_pretty(&ladder)?;
    let root = Dir::open_root(&top)?;
    let directory = root
        .descend(".arc", Some(0o755))?
        .context("cannot open the current checkout's .arc directory")?;
    let temporary = format!(".review-ladder-{}.tmp", crate::ids::new_identifier());
    let mut output = directory.create_file(&temporary, 0o644)?;
    let result = (|| -> Result<()> {
        output.write_all(text.as_bytes())?;
        output.sync_all()?;
        directory.rename(&temporary, "review-ladder.toml")?;
        directory.sync()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result?;
    println!(
        "candidate review ladder written: {}",
        top.join(review_ladder::FILE).display()
    );
    println!(
        "authorizing registry on {}: {} version(s)",
        state.target_branch,
        authority.as_ref().map_or(0, |ladder| ladder.version.len())
    );
    Ok(0)
}

pub fn show(ctx: &Ctx, reference: Option<&str>, json: bool) -> Result<i32> {
    let store = ctx.store()?;
    let target = if let Some(reference) = reference {
        let state = store.state(&store.resolve_change(reference)?)?;
        declarations::accepted_target(&state).to_string()
    } else {
        let branch = gitio::current_branch(&ctx.cwd)?;
        let mut matches = Vec::new();
        for id in store.list_change_ids()? {
            let state = store.state(&id)?;
            if Some(state.branch.as_str()) == branch.as_deref() {
                matches.push(state);
            }
        }
        match matches.as_slice() {
            [state] => declarations::accepted_target(state).to_string(),
            [] => gitio::default_branch(&ctx.cwd)
                .context("cannot establish a target branch; select one with --change")?,
            _ => bail!("ambiguous change branch; select one with --change"),
        }
    };
    let ladder = declarations::accepted_ladder(&ctx.cwd, &target)?.with_context(|| {
        format!("no accepted review ladder on {target}; import one in a change")
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&ladder)?);
    } else {
        print!("{}", toml::to_string_pretty(&ladder)?);
    }
    Ok(0)
}
