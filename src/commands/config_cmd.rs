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
    /// Whether a failure here is a warning rather than a blocked path. The
    /// probe answers what an executor can write; a capability it reports on
    /// without owning is carried for the diagnosis and decides no exit code.
    advisory: bool,
    /// Whether the text view prints this passing check's detail. Most details
    /// are the path the probe used and the name already says which capability
    /// passed; a check whose detail changes what the caller should do says it
    /// out loud instead.
    #[serde(skip)]
    show_detail: bool,
}

impl WritabilityCheck {
    /// A capability the executor itself must have; failing one is a blocked
    /// path and stops the probe.
    fn required(name: &'static str, ok: bool, detail: String, show_detail: bool) -> Self {
        Self {
            name,
            ok,
            detail,
            advisory: false,
            show_detail,
        }
    }

    /// A capability reported for the diagnosis, whose failure warns.
    fn advisory(name: &'static str, ok: bool, detail: String) -> Self {
        Self {
            name,
            ok,
            detail,
            advisory: true,
            show_detail: true,
        }
    }
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
        ("commit", probe_commit(ctx)),
    ] {
        match result {
            Ok(detail) => checks.push(WritabilityCheck::required(name, true, detail, false)),
            Err(error) => {
                checks.push(failed(name, error));
                return finish(json, checks);
            }
        }
    }
    checks.push(probe_signing(ctx));
    checks.push(probe_journal(ctx));
    finish(json, checks)
}

/// Where journal writes will land.
///
/// A journal this process cannot write is not a failure: the write spools to
/// the repository-local outbox and a later caller files it. The check reports
/// which of the two will happen, so an executor knows before it writes rather
/// than discovering it from a spooled path afterwards, and it never turns the
/// probe's exit code non-zero on its own.
fn probe_journal(ctx: &Ctx) -> WritabilityCheck {
    match crate::journal::writability(&ctx.cwd) {
        Ok((dir, None)) => {
            WritabilityCheck::required("journal", true, dir.display().to_string(), false)
        }
        Ok((dir, Some(outbox))) => WritabilityCheck::required(
            "journal",
            true,
            format!(
                "{} is unwritable: writes spool to {}, file them with `arc journal spool --promote`",
                dir.display(),
                outbox.display()
            ),
            true,
        ),
        Err(error) => {
            WritabilityCheck::required("journal", true, format!("unresolved: {error:#}"), true)
        }
    }
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

/// Committing is the other capability the ceremony needs, and the one a
/// sandboxed executor otherwise discovers only once a slice is ready to land.
/// Probe it in a throwaway repository so the target repository gains no commit.
///
/// Whether a commit can be made and whether it can be signed are two facts,
/// and only the first is writability: a reachable repository with an
/// unreachable signing agent is a signing problem reported as one, not a
/// repository the executor should be told it cannot write.
fn probe_commit(ctx: &Ctx) -> Result<String> {
    probe_commit_throwaway(ctx, false)?;
    Ok("unsigned commit".into())
}

/// Whether the credential a signed commit needs is reachable.
///
/// Advisory, because signing is not always the probing process's job: work
/// made in a sandbox is signed by whoever lands it, so an unreachable agent
/// here is a fact the caller needs and never a reason for that caller to
/// stop. It is still a warning rather than a note — a project whose
/// `commit.gpgsign` is on cannot land a commit until signing works somewhere,
/// so the line says the signature could not be produced. Where the project
/// signs nothing, it says so instead of claiming a capability never exercised.
fn probe_signing(ctx: &Ctx) -> WritabilityCheck {
    if !signing_required(ctx) {
        return WritabilityCheck::advisory(
            "signing",
            true,
            "not required (commit.gpgsign is off)".into(),
        );
    }
    match probe_commit_throwaway(ctx, true) {
        Ok(()) => WritabilityCheck::advisory("signing", true, "signed commit".into()),
        Err(error) => WritabilityCheck::advisory(
            "signing",
            false,
            format!("commit.gpgsign is on and the signature could not be produced: {error:#}"),
        ),
    }
}

/// Make one probe commit in a repository that is deleted either way, so the
/// target repository gains nothing from having been asked.
fn probe_commit_throwaway(ctx: &Ctx, signed: bool) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("arc-commit-probe-{}", ids::new_event_id()));
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let result = probe_commit_in(&dir, ctx, signed);
    let _ = fs::remove_dir_all(&dir);
    result
}

fn probe_commit_in(dir: &Path, ctx: &Ctx, signed: bool) -> Result<()> {
    gitio::git(dir, &["init", "--quiet"])?;
    // A probe repository has no user, and inheriting an unset global identity
    // would fail for a reason unrelated to what is being probed.
    gitio::git(dir, &["config", "user.name", "arc probe"])?;
    gitio::git(dir, &["config", "user.email", "probe@arc.invalid"])?;
    // The probe repository inherits global config, so pin signing to what the
    // target repository resolves to. A global `commit.gpgsign` would otherwise
    // make the probe sign a commit the real ceremony never signs, and fail on a
    // credential that never applies.
    gitio::git(
        dir,
        &[
            "config",
            "commit.gpgsign",
            if signed { "true" } else { "false" },
        ],
    )?;
    if signed {
        // Carry the project's signing key so the probe exercises the same
        // credential the real commit will use.
        if let Some(key) = git_config(ctx, "user.signingkey") {
            gitio::git(dir, &["config", "user.signingkey", &key])?;
        }
        if let Some(format) = git_config(ctx, "gpg.format") {
            gitio::git(dir, &["config", "gpg.format", &format])?;
        }
    }
    gitio::git(
        dir,
        &["commit", "--allow-empty", "--quiet", "-m", "arc probe"],
    )
    .context("cannot create a commit")?;
    Ok(())
}

/// Git accepts every boolean spelling for `commit.gpgsign` — `yes`, `on`, `1`,
/// `True` — so ask Git to resolve it rather than matching one of them. Reading
/// the raw string would report a signing repository as unsigned, which is the
/// case this probe exists to catch.
fn signing_required(ctx: &Ctx) -> bool {
    gitio::git(
        &ctx.cwd,
        &["config", "--get", "--type=bool", "commit.gpgsign"],
    )
    .is_ok_and(|value| value.trim() == "true")
}

fn git_config(ctx: &Ctx, key: &str) -> Option<String> {
    gitio::git(&ctx.cwd, &["config", "--get", key])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn failed(name: &'static str, error: anyhow::Error) -> WritabilityCheck {
    // Render the whole cause chain: the outermost context names which
    // capability failed, and only the innermost says why. A probe that
    // reports "cannot create a commit" without "gpg-agent unreachable" costs
    // the reader the diagnosis the probe exists to deliver.
    WritabilityCheck::required(name, false, format!("{error:#}"), true)
}

/// The exit code answers one question — can this process write what the
/// ceremony needs — so only the required checks decide it. An advisory
/// finding is printed for the reader and never turns a usable sandbox into a
/// refusal to start.
fn finish(json: bool, checks: Vec<WritabilityCheck>) -> Result<i32> {
    let passed = checks.iter().all(|check| check.ok || check.advisory);
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
            if !check.ok {
                let label = if check.advisory { "warn" } else { "fail" };
                println!("{label}: {}: {}", check.name, check.detail);
            } else if check.show_detail {
                println!("ok: {}: {}", check.name, check.detail);
            } else if check.detail == "skipped: unborn HEAD" {
                println!("{}", check.detail);
            } else {
                println!("ok: {}", check.name);
            }
        }
    }
    Ok(if passed { 0 } else { 1 })
}
