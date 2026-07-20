//! Local writability probes for executor sandboxes.

use super::*;
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize)]
struct WritabilityOutput {
    schema: &'static str,
    checks: Vec<WritabilityCheck>,
}

#[derive(Serialize)]
struct WritabilityCheck {
    name: &'static str,
    ok: bool,
    detail: String,
}

/// Probe each writable surface needed by an executor before it starts work.
pub fn check_writable(ctx: &Ctx, json: bool) -> Result<i32> {
    let root = match Store::resolve_root(&ctx.cwd) {
        Ok(root) => root,
        Err(error) => return finish(json, vec![failed("store-root", error)]),
    };
    let store = match ctx.store() {
        Ok(store) => store,
        Err(error) => {
            return finish(
                json,
                vec![failed(
                    "store-root",
                    anyhow::anyhow!("cannot write {}: {error}", root.display()),
                )],
            )
        }
    };
    let mut checks = Vec::new();
    for (name, result) in [
        ("store-root", probe_file(&store.root)),
        ("lock", probe_lock(&store)),
        ("events", probe_events(&store)),
        ("git-ref", probe_ref(ctx)),
    ] {
        match result {
            Ok(detail) => checks.push(WritabilityCheck {
                name,
                ok: true,
                detail,
            }),
            Err(error) => {
                checks.push(failed(name, error));
                return finish(json, checks);
            }
        }
    }
    finish(json, checks)
}

fn probe_file(dir: &Path) -> Result<String> {
    let path = dir.join(format!(".probe-{}.tmp", ids::new_event_id()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
    Ok(path.display().to_string())
}

fn probe_lock(store: &Store) -> Result<String> {
    let path = store.root.join("locks/probe.lock");
    drop(store.lock_probe()?);
    Ok(path.display().to_string())
}

fn probe_events(store: &Store) -> Result<String> {
    let dir = store.probe_events_dir()?;
    probe_file(&dir)
}

fn probe_ref(ctx: &Ctx) -> Result<String> {
    let Some(head) = gitio::head_if_present(&ctx.cwd)? else {
        return Ok("skipped: unborn HEAD".into());
    };
    let name = format!("refs/arc/probe-{}", ids::new_event_id());
    gitio::update_ref(&ctx.cwd, &name, &head)?;
    if let Err(error) = gitio::delete_ref(&ctx.cwd, &name) {
        return Err(error).context(format!("cannot remove probe ref {name}"));
    }
    Ok(name)
}

fn failed(name: &'static str, error: anyhow::Error) -> WritabilityCheck {
    WritabilityCheck {
        name,
        ok: false,
        detail: error.to_string(),
    }
}

fn finish(json: bool, checks: Vec<WritabilityCheck>) -> Result<i32> {
    let passed = checks.iter().all(|check| check.ok);
    if json {
        println!(
            "{}",
            serde_json::to_string(&WritabilityOutput {
                schema: "arc-writability/1",
                checks,
            })?
        );
    } else {
        for check in checks {
            if check.ok {
                if check.detail == "skipped: unborn HEAD" {
                    println!("{}", check.detail);
                } else {
                    println!("ok: {}", check.name);
                }
            } else {
                println!("fail: {}: {}", check.name, check.detail);
            }
        }
    }
    Ok(if passed { 0 } else { 1 })
}
