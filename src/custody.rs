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
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const LAYOUT: &str = "sha256";
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

/// The ledger's private root: owner access only.
fn create_dirs(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

fn open_new(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))
}

/// Copy a regular file to `destination` through a temporary sibling, verifying
/// the bytes actually copied against `content`. Nothing is published unless
/// they match.
fn publish_verified(source: &Path, destination: &Path, content: &Content) -> Result<()> {
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
    create_dirs(directory)?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        content.sha256,
        crate::ids::new_identifier()
    ));
    let result = (|| -> Result<()> {
        let mut output = open_new(&temporary)?;
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
        publish_verified(source, &self.path(&content.sha256), content)?;
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
}
