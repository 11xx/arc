use super::*;
use std::io::Write;
use std::path::Component;

const OWNER: &str = "owner";
const LEASE: &str = "lease";
const INCOMPLETE: &str = "incomplete";

pub(super) fn lease_path(build: &Path) -> Result<PathBuf> {
    control_path(build, LEASE)
}

pub(super) fn incomplete_path(build: &Path) -> Result<PathBuf> {
    control_path(build, INCOMPLETE)
}

fn control_path(build: &Path, kind: &str) -> Result<PathBuf> {
    let parent = build
        .parent()
        .context("path-stable storage has no parent")?;
    Ok(parent.join(format!(".arc-worktree-{}.{kind}", source_path_key(build))))
}

struct Storage {
    build: PathBuf,
    cache: PathBuf,
    incomplete: PathBuf,
    guard: CommandLease,
}

impl Storage {
    fn open(path: &Path, hook: &Hook, timeout: u64) -> Result<Self> {
        let common = fs::canonicalize(gitio::common_dir(path)?)?;
        let build = if let Some(directory) = &hook.directory {
            let candidate = if directory.is_absolute() {
                directory.clone()
            } else {
                path.join(directory)
            };
            let resolved = resolve_directory(&candidate)?;
            if resolved.starts_with(path) && !resolved.starts_with(&common) {
                bail!("path-stable build directory must be outside the source checkout or inside Git common administrative storage");
            }
            crate::confinement::require_external(path, &resolved)?;
            resolved
        } else {
            let arc = private_directory(&common, "arc")?;
            let cache = private_directory(&arc, "cache")?;
            let stable = private_directory(&cache, "path-stable-worktrees")?;
            stable.join(source_path_key(path))
        };
        if path.starts_with(&build) || common.starts_with(&build) {
            bail!("path-stable build storage must not contain the source checkout or Git common directory");
        }
        fs::DirBuilder::new().recursive(true).mode(0o700).create(
            build
                .parent()
                .context("path-stable storage has no parent")?,
        )?;
        let build_lease = control_path(&build, LEASE)?;
        let file = acquire_lease(&build_lease, "path-stable build")?;
        owned_marker(
            &control_path(&build, OWNER)?,
            &format!(
                "arc-worktree-path-stable/1\n{}\n{}\n",
                source_path_key(&common),
                source_path_key(&build)
            ),
        )?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&build)?;
        if !fs::symlink_metadata(&build)?.file_type().is_dir() || fs::canonicalize(&build)? != build
        {
            bail!("path-stable storage changed identity while opening");
        }
        let cache = owned_cache(&common, &cache_key(hook, timeout))?;
        Ok(Self {
            build: build.clone(),
            cache: cache.clone(),
            incomplete: control_path(&build, INCOMPLETE)?,
            guard: CommandLease {
                files: vec![file],
                build_lease: Some(build_lease),
                storage: Some(CommandStorage {
                    build: build.clone(),
                    cache,
                    owned_build: hook.directory.is_none(),
                }),
            },
        })
    }

    fn configure(&self, command: &mut Command, path: &Path, source: &Source) {
        command
            .envs(environment(
                path,
                &source.revision,
                &source.tree,
                &self.cache,
                &self.build,
            ))
            .env_remove("CARGO_TARGET_DIR")
            .env("GIT_OPTIONAL_LOCKS", "0");
        self.guard.inherit(command);
    }

    fn require_complete(&self) -> Result<()> {
        match fs::symlink_metadata(&self.incomplete) {
            Ok(_) => bail!("path-stable preparation is incomplete; run successful preparation before using its outputs"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).context("cannot inspect path-stable preparation state"),
        }
    }

    /// Run the preparation command, if one is declared, reporting whether
    /// it ran and succeeded.
    fn prepare(&mut self, path: &Path, source: &Source, hook: &Hook, timeout: u64) -> Result<bool> {
        let Some(command) = hook.command.as_deref() else {
            self.require_complete()?;
            return Ok(false);
        };
        let cache_lease = lease(&self.cache)?;
        self.guard.files.push(cache_lease);
        let incomplete = &self.incomplete;
        let mut marker = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(incomplete)?;
        if !marker.metadata()?.is_file() {
            bail!("path-stable preparation state is not a regular file");
        }
        marker.write_all(b"arc-worktree-incomplete/1\n")?;
        marker.sync_all()?;
        let outcome = run_hook(
            path,
            source,
            command,
            timeout,
            &self.cache,
            &self.build,
            &self.guard,
        );
        let after = Source::observe(path)?;
        if after != *source {
            return Err(SourceMutation(source.changed_fields(&after)).into());
        }
        outcome.context("path-stable preparation failed; retained outputs are unavailable")?;
        fs::remove_file(incomplete)?;
        // The stable lease spans preparation and its caller; the cache is
        // protected only while the hook can write it.
        self.guard.files.pop();
        Ok(true)
    }
}

/// Resolve existing ancestors before checking source containment. Missing
/// components are literal directories and cannot redirect into the source.
fn resolve_directory(path: &Path) -> Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match fs::symlink_metadata(&resolved) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        resolved = fs::canonicalize(&resolved)?;
                    }
                    Ok(metadata) if !metadata.is_dir() => bail!(
                        "path-stable build ancestor is not a directory: {}",
                        resolved.display()
                    ),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).context("cannot resolve path-stable build storage")
                    }
                }
            }
            Component::Prefix(_) => unreachable!("Unix paths have no prefix"),
        }
    }
    Ok(resolved)
}

/// Prepare stable storage, reporting whether a preparation command ran and
/// succeeded.
pub(super) fn prepare(path: &Path, (hook, timeout): &(Hook, u64)) -> Result<bool> {
    let source = Source::observe(path)?;
    let mut storage = Storage::open(path, hook, *timeout)?;
    let prepared = storage.prepare(path, &source, hook, *timeout)?;
    eprintln!(
        "worktree preparation: path-stable storage {}",
        storage.build.display()
    );
    Ok(prepared)
}

pub(super) fn configure(
    command: &mut Command,
    path: &Path,
    (hook, timeout): &(Hook, u64),
    usage: Use,
) -> Result<CommandLease> {
    let source = Source::observe(path)?;
    let mut storage = Storage::open(path, hook, *timeout)?;
    match usage {
        Use::Gate => {
            if storage.prepare(path, &source, hook, *timeout)? {
                crate::slots::record_built(path)?;
            }
        }
        Use::Probe => storage.require_complete()?,
        Use::Invalidate => {}
    }
    storage.configure(command, path, &source);
    Ok(storage.guard)
}
