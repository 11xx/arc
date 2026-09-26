use super::Ctx;
use anyhow::Result;

pub fn id(ctx: &Ctx, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let identity = crate::replica::id(&store);
    if json {
        println!("{}", serde_json::to_string(&identity)?);
    } else {
        println!("repository: {}", identity.repository_id);
    }
    Ok(())
}

pub fn status(ctx: &Ctx, json: bool) -> Result<()> {
    let store = ctx.store()?;
    let status = crate::replica::status(&store)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        crate::replica::render_status(&status);
    }
    Ok(())
}

pub fn init(ctx: &Ctx, name: &str) -> Result<()> {
    let store = ctx.store()?;
    crate::replica::init(ctx, &store, name)
}

pub fn pair(ctx: &Ctx, name: &str, repository_id: &str) -> Result<()> {
    let store = ctx.store()?;
    crate::replica::pair(ctx, &store, name, repository_id)
}

pub fn export(ctx: &Ctx, output: &str) -> Result<()> {
    let store = ctx.store()?;
    crate::replica::export(&store, output)
}

pub fn import(ctx: &Ctx, input: &str, dry_run: bool) -> Result<i32> {
    let store = ctx.store()?;
    crate::replica::import(ctx, &store, input, dry_run)
}

pub fn offer(ctx: &Ctx, recipient: &str) -> Result<()> {
    let store = ctx.store()?;
    crate::replica::offer(ctx, &store, recipient)
}

pub fn reclaim(ctx: &Ctx, reason: &str) -> Result<()> {
    let store = ctx.store()?;
    crate::replica::reclaim(ctx, &store, reason)
}

pub fn confirm_return(ctx: &Ctx) -> Result<()> {
    let store = ctx.store()?;
    crate::replica::confirm_return(ctx, &store)
}
