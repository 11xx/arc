//! Content-addressed custody for the bytes retained-content references name.
//!
//! A reference in the ledger is a claim about bytes. Custody holds the bytes
//! under the ledger's private root, keyed by SHA-256, and a reference reads as
//! available only while a regular file at its digest has exactly the recorded
//! length and digest. A missing file reads as unavailable, never as empty
//! content, and bytes that fail verification read as corrupt. Arc never
//! resolves a provider handle: bytes arrive as files an adapter materialized
//! or inside a sidecar an export wrote, and are verified before they are
//! published.
use crate::model::Payload;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const SIDECAR_SCHEMA: &str = "arc-custody-bundle/1";
pub const MANIFEST_FILE: &str = "manifest.json";
const LAYOUT: &str = "sha256";
const MANIFEST_LIMIT: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

/// The bytes a reference names: their digest and exact length.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Content {
    pub sha256: String,
    pub byte_length: u64,
}

impl From<&crate::forge_observation::RetainedContentRef> for Content {
    fn from(reference: &crate::forge_observation::RetainedContentRef) -> Self {
        Content {
            sha256: reference.sha256.clone(),
            byte_length: reference.byte_length,
        }
    }
}

/// Every content reference the payloads carry.
pub fn referenced<'a>(payloads: impl Iterator<Item = &'a Payload>) -> BTreeSet<Content> {
    let mut found = BTreeSet::new();
    for payload in payloads {
        match payload {
            Payload::ForgeObservationRecorded { observation } => {
                found.extend(observation.contents().map(Content::from));
            }
            Payload::CandidateEvidenceReceived { evidence, .. } => {
                found.extend(evidence.receipts.iter().map(Content::from));
            }
            _ => {}
        }
    }
    found
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// No bytes are held for the digest.
    Unavailable,
    /// Bytes are held and fail verification, or cannot be read.
    Corrupt(String),
}

impl Availability {
    pub fn label(&self) -> &'static str {
        match self {
            Availability::Available => "available",
            Availability::Unavailable => "unavailable",
            Availability::Corrupt(_) => "corrupt",
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }
}

pub fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

enum Verdict {
    Verified,
    Mismatch(String),
}

/// Read to the end, checking the exact length and digest, optionally copying
/// what is read. Reading stops one byte past the recorded length, so a file
/// of any size is bounded by the claim it is checked against.
fn stream(
    reader: &mut impl Read,
    content: &Content,
    mut sink: Option<&mut File>,
) -> std::io::Result<Verdict> {
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > content.byte_length {
            return Ok(Verdict::Mismatch(format!(
                "is longer than the recorded length {}",
                content.byte_length
            )));
        }
        hash.update(&buffer[..read]);
        if let Some(sink) = sink.as_deref_mut() {
            sink.write_all(&buffer[..read])?;
        }
    }
    if total != content.byte_length {
        return Ok(Verdict::Mismatch(format!(
            "has length {total}, not the recorded length {}",
            content.byte_length
        )));
    }
    if hex::encode(hash.finalize()) != content.sha256 {
        return Ok(Verdict::Mismatch(
            "does not match the recorded digest".to_string(),
        ));
    }
    Ok(Verdict::Verified)
}

fn inspect(path: &Path, content: &Content) -> Availability {
    if !is_digest(&content.sha256) {
        return Availability::Corrupt("the recorded digest is not a sha256".into());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Availability::Unavailable
        }
        Err(error) => return Availability::Corrupt(format!("unreadable: {error}")),
    };
    if !metadata.file_type().is_file() {
        return Availability::Corrupt("not a regular file".into());
    }
    match File::open(path).and_then(|mut file| stream(&mut file, content, None)) {
        Ok(Verdict::Verified) => Availability::Available,
        Ok(Verdict::Mismatch(reason)) => Availability::Corrupt(reason),
        Err(error) => Availability::Corrupt(format!("unreadable: {error}")),
    }
}

#[derive(Clone, Copy)]
enum Visibility {
    /// The ledger's private root: owner access only.
    Private,
    /// A transport directory, created as the caller's umask allows.
    Shared,
}

fn create_dirs(path: &Path, visibility: Visibility) -> Result<()> {
    match visibility {
        Visibility::Private => fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path),
        Visibility::Shared => fs::create_dir_all(path),
    }
    .with_context(|| format!("cannot create {}", path.display()))
}

/// Write `bytes` beside `destination` and rename them into place, so a reader
/// sees the whole file or none of it.
fn write_atomic(destination: &Path, bytes: &[u8], visibility: Visibility) -> Result<()> {
    let directory = destination
        .parent()
        .context("destination has no parent directory")?;
    create_dirs(directory, visibility)?;
    let name = destination
        .file_name()
        .context("destination has no file name")?
        .to_string_lossy();
    let temporary = directory.join(format!(".{name}.{}.tmp", crate::ids::new_identifier()));
    let result = (|| -> Result<()> {
        let mut file = open_new(&temporary, visibility)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, destination)
            .with_context(|| format!("cannot publish {}", destination.display()))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn open_new(path: &Path, visibility: Visibility) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    if matches!(visibility, Visibility::Private) {
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

/// Copy a regular file to `destination` through a temporary sibling, verifying
/// the bytes actually copied against `content`. Nothing is published unless
/// they match.
fn publish_verified(
    source: &Path,
    destination: &Path,
    content: &Content,
    visibility: Visibility,
) -> Result<()> {
    let mut input =
        File::open(source).with_context(|| format!("cannot read {}", source.display()))?;
    ensure!(
        input.metadata()?.is_file(),
        "{} is not a regular file",
        source.display()
    );
    let directory = destination
        .parent()
        .context("destination has no parent directory")?;
    create_dirs(directory, visibility)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        content.sha256,
        crate::ids::new_identifier()
    ));
    let result = (|| -> Result<()> {
        let mut output = open_new(&temporary, visibility)?;
        if let Verdict::Mismatch(reason) = stream(&mut input, content, Some(&mut output))? {
            bail!("content {} {reason}", short(&content.sha256));
        }
        output.sync_all()?;
        fs::rename(&temporary, destination)
            .with_context(|| format!("cannot publish {}", destination.display()))?;
        File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn short(digest: &str) -> &str {
    &digest[..digest.len().min(12)]
}

/// A path that names something inside the directory it is joined to: relative,
/// with no empty, `.` or `..` component and no separator but `/`.
pub fn safe_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.starts_with('/')
            && !path.contains(['\\', '\0'])
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "unsafe path {path:?}"
    );
    Ok(())
}

/// What a ledger holds for the references it records.
pub struct Custody {
    objects: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    Written,
    AlreadyHeld,
}

impl Custody {
    pub fn at(ledger_root: &Path) -> Custody {
        Custody {
            objects: ledger_root.join("custody").join(LAYOUT),
        }
    }

    pub fn path(&self, sha256: &str) -> PathBuf {
        self.objects.join(sha256)
    }

    /// Read the bytes' state, verifying length and digest.
    pub fn availability(&self, content: &Content) -> Availability {
        if !is_digest(&content.sha256) {
            return Availability::Corrupt("the recorded digest is not a sha256".into());
        }
        inspect(&self.path(&content.sha256), content)
    }

    /// Publish a regular file's bytes under their digest. Bytes already held
    /// and verified are left alone; damaged ones are replaced.
    pub fn install(&self, content: &Content, source: &Path) -> Result<Installed> {
        ensure!(is_digest(&content.sha256), "invalid content digest");
        if self.availability(content).is_available() {
            return Ok(Installed::AlreadyHeld);
        }
        publish_verified(
            source,
            &self.path(&content.sha256),
            content,
            Visibility::Private,
        )?;
        Ok(Installed::Written)
    }
}

/// Adapter-materialized files, verified and ready to publish.
///
/// The adapter supplies each file at `DIR/sha256/<digest>`. Only the digests a
/// document references are looked up, so nothing else in the directory is
/// read. A reference with no file stays unavailable.
pub struct Materialization {
    files: Vec<(Content, PathBuf)>,
}

pub fn plan_materialization(dir: &Path, wanted: &BTreeSet<Content>) -> Result<Materialization> {
    let root = fs::canonicalize(dir)
        .with_context(|| format!("cannot read custody directory {}", dir.display()))?;
    ensure!(root.is_dir(), "{} is not a directory", root.display());
    let layout = root.join(LAYOUT);
    match fs::symlink_metadata(&layout) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("custody directory has no {LAYOUT}/ directory")
        }
        Err(error) => return Err(error).context("cannot inspect the custody layout directory"),
    }
    let layout =
        fs::canonicalize(&layout).context("cannot resolve the custody layout directory")?;
    ensure!(
        layout.starts_with(&root) && layout.is_dir(),
        "the {LAYOUT}/ directory leaves the custody directory"
    );
    let mut files = Vec::new();
    for content in wanted {
        ensure!(is_digest(&content.sha256), "invalid content digest");
        let candidate = layout.join(&content.sha256);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot inspect {}", candidate.display()))
            }
        };
        let resolved = if metadata.file_type().is_symlink() {
            let resolved = fs::canonicalize(&candidate)
                .with_context(|| format!("cannot resolve symlink {}", candidate.display()))?;
            ensure!(
                resolved.starts_with(&root),
                "symlink {} leaves the custody directory",
                short(&content.sha256)
            );
            resolved
        } else {
            candidate
        };
        ensure!(
            fs::metadata(&resolved)?.is_file(),
            "content {} is not a regular file",
            short(&content.sha256)
        );
        let mut file = File::open(&resolved)?;
        if let Verdict::Mismatch(reason) = stream(&mut file, content, None)? {
            bail!("content {} {reason}", short(&content.sha256));
        }
        files.push((content.clone(), resolved));
    }
    Ok(Materialization { files })
}

impl Materialization {
    /// Publish every verified file. Each is re-verified as it is copied.
    pub fn publish(&self, custody: &Custody) -> Result<()> {
        for (content, source) in &self.files {
            custody.install(content, source)?;
        }
        Ok(())
    }
}

/// The sidecar an export writes beside its bundle: `manifest.json` and one
/// `sha256/<digest>` file per available digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub change_id: String,
    /// Whether every entry is available.
    pub complete: bool,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryState {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub sha256: String,
    pub byte_length: u64,
    pub state: EntryState,
    /// Where an available entry's bytes are, relative to the sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why an unavailable entry has no bytes: `absent` or `corrupt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Entry {
    fn content(&self) -> Content {
        Content {
            sha256: self.sha256.clone(),
            byte_length: self.byte_length,
        }
    }
}

/// The manifest digest an arc bundle carries, which binds the sidecar to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub schema: String,
    pub manifest_sha256: String,
}

impl Binding {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == SIDECAR_SCHEMA,
            "unsupported custody schema {:?}",
            self.schema
        );
        ensure!(
            is_digest(&self.manifest_sha256),
            "custody manifest digest {:?} is not a sha256",
            self.manifest_sha256
        );
        Ok(())
    }
}

pub struct Exported {
    pub binding: Binding,
    pub entries: usize,
    pub available: usize,
    pub unavailable: usize,
}

impl Exported {
    pub fn complete(&self) -> bool {
        self.unavailable == 0
    }
}

/// A sidecar written beside its destination and not yet in place. Dropping it
/// without publishing removes it, so an export that fails leaves nothing
/// behind.
pub struct Staged {
    staging: Option<PathBuf>,
    destination: PathBuf,
}

impl Staged {
    /// Move the finished sidecar into place. The destination is replaced only
    /// when it is absent or an empty directory.
    pub fn publish(mut self) -> Result<()> {
        let staging = self.staging.as_deref().expect("a staged sidecar");
        fs::rename(staging, &self.destination)
            .with_context(|| format!("cannot publish {}", self.destination.display()))?;
        self.staging = None;
        if let Some(parent) = self
            .destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(staging) = &self.staging {
            let _ = fs::remove_dir_all(staging);
        }
    }
}

/// Write the sidecar for `wanted`, to be published at `dir`, which must be
/// absent or empty. Available bytes are copied once per digest; a source that
/// is absent or damaged gets an explicit unavailable entry, and the manifest
/// says it is incomplete. Everything is written beside `dir` first, so `dir`
/// holds either the whole sidecar or what it held before.
pub fn export_sidecar(
    custody: &Custody,
    change_id: &str,
    wanted: &BTreeSet<Content>,
    dir: &Path,
) -> Result<(Exported, Staged)> {
    match fs::read_dir(dir) {
        Ok(mut entries) => ensure!(
            entries.next().is_none(),
            "refusing to write a custody sidecar into the non-empty {}",
            dir.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("cannot use {}", dir.display())),
    }
    let name = dir
        .file_name()
        .context("a custody sidecar needs a directory name")?
        .to_string_lossy();
    let parent = dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_dirs(parent, Visibility::Shared)?;
    let staging = parent.join(format!(".{name}.{}.staging", crate::ids::new_identifier()));
    fs::create_dir(&staging).with_context(|| format!("cannot create {}", staging.display()))?;
    let staged = Staged {
        staging: Some(staging.clone()),
        destination: dir.to_path_buf(),
    };
    create_dirs(&staging.join(LAYOUT), Visibility::Shared)?;
    let mut entries = Vec::new();
    for content in wanted {
        let state = custody.availability(content);
        let mut entry = Entry {
            sha256: content.sha256.clone(),
            byte_length: content.byte_length,
            state: EntryState::Unavailable,
            path: None,
            reason: None,
        };
        match state {
            Availability::Available => {
                let path = format!("{LAYOUT}/{}", content.sha256);
                publish_verified(
                    &custody.path(&content.sha256),
                    &staging.join(&path),
                    content,
                    Visibility::Shared,
                )?;
                entry.state = EntryState::Available;
                entry.path = Some(path);
            }
            Availability::Unavailable => entry.reason = Some("absent".into()),
            Availability::Corrupt(_) => entry.reason = Some("corrupt".into()),
        }
        entries.push(entry);
    }
    let available = entries
        .iter()
        .filter(|entry| entry.state == EntryState::Available)
        .count();
    let manifest = Manifest {
        schema: SIDECAR_SCHEMA.to_string(),
        change_id: change_id.to_string(),
        complete: available == entries.len(),
        entries,
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    write_atomic(&staging.join(MANIFEST_FILE), &bytes, Visibility::Shared)?;
    Ok((
        Exported {
            binding: Binding {
                schema: SIDECAR_SCHEMA.to_string(),
                manifest_sha256: hex::encode(Sha256::digest(&bytes)),
            },
            entries: manifest.entries.len(),
            available,
            unavailable: manifest.entries.len() - available,
        },
        staged,
    ))
}

/// A sidecar judged against its bundle: every file checked, nothing published.
pub struct Sidecar {
    files: Vec<(Content, PathBuf)>,
    unavailable: Vec<Content>,
}

/// What publishing a sidecar did, or would do.
pub struct Outcome {
    pub installed: usize,
    pub held: usize,
    pub unavailable: usize,
}

impl Outcome {
    pub fn complete(&self) -> bool {
        self.unavailable == 0
    }
}

/// Judge the sidecar in `dir` against the bundle that binds it. The manifest
/// must hash to the bundle's binding, belong to the bundle's change, and list
/// exactly the content the bundled events reference. Available entries must
/// name their digest's path inside the sidecar and hold exactly the recorded
/// bytes in a regular file that is not a symlink. All of it is checked before
/// the caller publishes anything.
pub fn load_sidecar(
    dir: &Path,
    binding: Option<&Binding>,
    change_id: &str,
    referenced: &BTreeSet<Content>,
) -> Result<Sidecar> {
    let binding = binding.context("the bundle binds no custody manifest; refusing the sidecar")?;
    binding.validate()?;
    let root = fs::canonicalize(dir)
        .with_context(|| format!("cannot read custody sidecar {}", dir.display()))?;
    ensure!(root.is_dir(), "{} is not a directory", root.display());
    let manifest_path = root.join(MANIFEST_FILE);
    let metadata =
        fs::symlink_metadata(&manifest_path).context("the custody sidecar has no manifest.json")?;
    ensure!(
        metadata.file_type().is_file(),
        "the custody manifest is a symlink or not a regular file"
    );
    ensure!(
        metadata.len() <= MANIFEST_LIMIT,
        "the custody manifest exceeds {MANIFEST_LIMIT} bytes"
    );
    let bytes = fs::read(&manifest_path)?;
    let actual = hex::encode(Sha256::digest(&bytes));
    ensure!(
        actual == binding.manifest_sha256,
        "the custody manifest digest {actual} does not match the bundle's binding {}",
        binding.manifest_sha256
    );
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("malformed custody manifest")?;
    ensure!(
        manifest.schema == SIDECAR_SCHEMA,
        "unsupported custody manifest schema {:?}",
        manifest.schema
    );
    ensure!(
        manifest.change_id == change_id,
        "the custody manifest belongs to change {:?}, not {change_id:?}",
        manifest.change_id
    );
    let mut listed = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for entry in &manifest.entries {
        ensure!(
            is_digest(&entry.sha256),
            "custody entry digest {:?} is not a sha256",
            entry.sha256
        );
        ensure!(
            listed.insert(entry.content()),
            "custody entry {} is listed twice",
            short(&entry.sha256)
        );
        match entry.state {
            EntryState::Available => {
                let path = entry.path.as_deref().with_context(|| {
                    format!("available entry {} has no path", short(&entry.sha256))
                })?;
                ensure!(entry.reason.is_none(), "an available entry names a reason");
                safe_relative(path)?;
                ensure!(
                    path == format!("{LAYOUT}/{}", entry.sha256),
                    "path {path:?} does not name its content digest"
                );
                ensure!(
                    paths.insert(path.to_string()),
                    "two entries share the path {path:?}"
                );
            }
            EntryState::Unavailable => {
                ensure!(entry.path.is_none(), "an unavailable entry names a path");
                ensure!(
                    matches!(entry.reason.as_deref(), Some("absent" | "corrupt")),
                    "an unavailable entry needs the reason absent or corrupt"
                );
            }
        }
    }
    ensure!(
        manifest.complete
            == manifest
                .entries
                .iter()
                .all(|entry| entry.state == EntryState::Available),
        "the manifest's complete flag disagrees with its entries: complete custody \
         cannot list unavailable content"
    );
    if let Some(extra) = listed.difference(referenced).next() {
        bail!(
            "the manifest names content no bundled event references: {}",
            extra.sha256
        );
    }
    if let Some(missing) = referenced.difference(&listed).next() {
        bail!(
            "the manifest omits content a bundled event references: {}",
            missing.sha256
        );
    }
    let mut files = Vec::new();
    let mut unavailable = Vec::new();
    if manifest
        .entries
        .iter()
        .any(|entry| entry.state == EntryState::Available)
    {
        let layout = root.join(LAYOUT);
        ensure!(
            fs::symlink_metadata(&layout)
                .map(|metadata| metadata.file_type().is_dir())
                .unwrap_or(false),
            "the sidecar's {LAYOUT}/ is a symlink or not a directory"
        );
    }
    for entry in &manifest.entries {
        let content = entry.content();
        if entry.state == EntryState::Unavailable {
            unavailable.push(content);
            continue;
        }
        let file = root.join(entry.path.as_deref().expect("validated path"));
        let metadata = fs::symlink_metadata(&file)
            .with_context(|| format!("custody content {} is missing", short(&entry.sha256)))?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "custody content {} is a symlink",
            short(&entry.sha256)
        );
        ensure!(
            metadata.file_type().is_file(),
            "custody content {} is not a regular file",
            short(&entry.sha256)
        );
        let mut reader = File::open(&file)?;
        if let Verdict::Mismatch(reason) = stream(&mut reader, &content, None)? {
            bail!("custody content {} {reason}", short(&entry.sha256));
        }
        files.push((content, file));
    }
    Ok(Sidecar { files, unavailable })
}

impl Sidecar {
    /// What publishing would do, against what the custody already holds.
    pub fn plan(&self, custody: &Custody) -> Outcome {
        self.tally(custody)
    }

    /// Publish each file the custody does not already hold, verifying the
    /// bytes again as they are copied.
    pub fn install(&self, custody: &Custody) -> Result<Outcome> {
        let outcome = self.tally(custody);
        for (content, file) in &self.files {
            custody.install(content, file)?;
        }
        Ok(outcome)
    }

    fn tally(&self, custody: &Custody) -> Outcome {
        let mut outcome = Outcome {
            installed: 0,
            held: 0,
            unavailable: 0,
        };
        for (content, _) in &self.files {
            if custody.availability(content).is_available() {
                outcome.held += 1;
            } else {
                outcome.installed += 1;
            }
        }
        for content in &self.unavailable {
            if custody.availability(content).is_available() {
                outcome.held += 1;
            } else {
                outcome.unavailable += 1;
            }
        }
        outcome
    }
}

/// What a bundle that binds a sidecar says when none is supplied: how much
/// stays unavailable, which manifest would supply it, and how to install it.
pub fn unsupplied_note(binding: &Binding, unavailable: usize) -> String {
    format!(
        "{unavailable} references unavailable (bundle binds manifest {}; pass --custody-dir <dir> to install it)",
        binding.manifest_sha256
    )
}

/// How many of `referenced` this custody cannot supply.
pub fn unavailable_among(custody: &Custody, referenced: &BTreeSet<Content>) -> usize {
    referenced
        .iter()
        .filter(|content| !custody.availability(content).is_available())
        .count()
}

/// A write that is not finished this long after it began has been abandoned.
const ORPHANED_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

/// A custody file that does not hold what its name says, or a temporary file
/// an interrupted write left behind.
#[derive(Debug, PartialEq, Eq)]
pub enum Damage {
    Mismatch { path: PathBuf, reason: String },
    Orphaned { path: PathBuf },
}

/// Inspect every file in custody without changing any. A file named for a
/// digest must be a regular file whose bytes hash to that digest; a hidden
/// temporary file is an orphan once it is old enough that no write can still
/// be filling it. Mismatches come first.
pub fn audit(ledger_root: &Path, now: std::time::SystemTime) -> Result<Vec<Damage>> {
    let objects = ledger_root.join("custody").join(LAYOUT);
    let entries = match fs::read_dir(&objects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", objects.display()))
        }
    };
    let mut names = entries
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    let mut mismatched = Vec::new();
    let mut orphaned = Vec::new();
    for name in names {
        let path = objects.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if is_digest(&name) {
            let reason = if !metadata.file_type().is_file() {
                Some("not a regular file".to_string())
            } else {
                match hash_file(&path) {
                    Ok(actual) if actual == name => None,
                    Ok(actual) => Some(format!("its bytes hash to {actual}")),
                    Err(error) => Some(format!("unreadable: {error}")),
                }
            };
            if let Some(reason) = reason {
                mismatched.push(Damage::Mismatch { path, reason });
            }
        } else if name.starts_with('.') && name.ends_with(".tmp") {
            let age = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .unwrap_or_default();
            if age >= ORPHANED_AFTER {
                orphaned.push(Damage::Orphaned { path });
            }
        }
    }
    mismatched.extend(orphaned);
    Ok(mismatched)
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex::encode(hash.finalize()));
        }
        hash.update(&buffer[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(body: &str) -> Content {
        Content {
            sha256: hex::encode(Sha256::digest(body.as_bytes())),
            byte_length: body.len() as u64,
        }
    }

    #[test]
    fn a_relative_path_stays_inside_its_directory() {
        for safe in ["sha256/abc", "a", "a/b/c"] {
            assert!(safe_relative(safe).is_ok(), "{safe}");
        }
        for unsafe_path in [
            "",
            "/etc/passwd",
            "../x",
            "a/../x",
            "a/./b",
            "a//b",
            "a/",
            "a\\b",
            "a\0b",
            ".",
            "..",
        ] {
            assert!(safe_relative(unsafe_path).is_err(), "{unsafe_path:?}");
        }
    }

    #[test]
    fn a_reference_reads_by_what_the_bytes_are() {
        let dir = tempfile::tempdir().unwrap();
        let custody = Custody::at(dir.path());
        let body = content("body");
        assert_eq!(custody.availability(&body), Availability::Unavailable);

        let source = dir.path().join("source");
        fs::write(&source, "body").unwrap();
        assert_eq!(custody.install(&body, &source).unwrap(), Installed::Written);
        assert_eq!(custody.availability(&body), Availability::Available);
        assert_eq!(
            custody.install(&body, &source).unwrap(),
            Installed::AlreadyHeld
        );

        fs::write(custody.path(&body.sha256), "").unwrap();
        assert!(matches!(
            custody.availability(&body),
            Availability::Corrupt(_)
        ));
        fs::write(custody.path(&body.sha256), "bodies").unwrap();
        assert!(matches!(
            custody.availability(&body),
            Availability::Corrupt(_)
        ));
        fs::write(custody.path(&body.sha256), "bddy").unwrap();
        assert!(matches!(
            custody.availability(&body),
            Availability::Corrupt(_)
        ));

        // Damaged bytes are replaced by verified ones.
        assert_eq!(custody.install(&body, &source).unwrap(), Installed::Written);
        assert_eq!(custody.availability(&body), Availability::Available);

        // Empty content is available only as an existing empty file.
        let empty = content("");
        assert_eq!(custody.availability(&empty), Availability::Unavailable);
    }

    #[test]
    fn an_install_that_mismatches_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let custody = Custody::at(dir.path());
        let source = dir.path().join("source");
        fs::write(&source, "other").unwrap();
        let body = content("body");
        assert!(custody.install(&body, &source).is_err());
        assert_eq!(custody.availability(&body), Availability::Unavailable);
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("custody/sha256"))
            .map(|entries| entries.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_digest_that_is_not_a_sha256_never_names_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let custody = Custody::at(dir.path());
        let hostile = Content {
            sha256: "../../etc/passwd".into(),
            byte_length: 1,
        };
        assert!(matches!(
            custody.availability(&hostile),
            Availability::Corrupt(_)
        ));
        assert!(custody.install(&hostile, dir.path()).is_err());
        assert!(!is_digest(&"A".repeat(64)));
        assert!(is_digest(&"a".repeat(64)));
    }

    #[test]
    fn a_staged_sidecar_is_removed_unless_it_is_published() {
        let dir = tempfile::tempdir().unwrap();
        let custody = Custody::at(dir.path());
        let destination = dir.path().join("out").join("sidecar");
        let (exported, staged) =
            export_sidecar(&custody, "change", &BTreeSet::new(), &destination).unwrap();
        assert!(exported.complete());
        assert!(!destination.exists());
        let siblings = |path: &Path| fs::read_dir(path).unwrap().count();
        assert_eq!(siblings(&dir.path().join("out")), 1);
        drop(staged);
        assert_eq!(siblings(&dir.path().join("out")), 0);

        let (_, staged) =
            export_sidecar(&custody, "change", &BTreeSet::new(), &destination).unwrap();
        staged.publish().unwrap();
        assert!(destination.join(MANIFEST_FILE).is_file());
        assert_eq!(siblings(&dir.path().join("out")), 1);
    }

    #[test]
    fn an_audit_names_damage_and_leaves_every_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let custody = Custody::at(dir.path());
        let source = dir.path().join("source");
        fs::write(&source, "body").unwrap();
        let body = content("body");
        custody.install(&body, &source).unwrap();
        let now = std::time::SystemTime::now();
        assert_eq!(audit(dir.path(), now).unwrap(), Vec::new());

        fs::write(custody.path(&body.sha256), "bodies").unwrap();
        let objects = dir.path().join("custody/sha256");
        let temporary = objects.join(".x.tmp");
        fs::write(&temporary, "partial").unwrap();
        // A temporary file is an orphan only once nothing can still be writing it.
        assert_eq!(audit(dir.path(), now).unwrap().len(), 1);
        let later = now + ORPHANED_AFTER + std::time::Duration::from_secs(1);
        let found = audit(dir.path(), later).unwrap();
        assert!(
            matches!(&found[0], Damage::Mismatch { path, .. } if path == &custody.path(&body.sha256))
        );
        assert_eq!(
            found[1],
            Damage::Orphaned {
                path: temporary.clone()
            }
        );
        assert_eq!(fs::read(custody.path(&body.sha256)).unwrap(), b"bodies");
        assert!(temporary.exists());
    }
}
