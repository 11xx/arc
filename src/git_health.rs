//! Repository configuration health shared by orientation and diagnostics.

use crate::gitio;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub code: &'static str,
    pub detail: String,
}

/// Fixture identity exceptions require both a debug executable and a marker
/// in that repository's common directory, as well as the explicit test flag.
pub fn fixture_identity(cwd: &Path) -> bool {
    cfg!(debug_assertions)
        && std::env::var_os("ARC_TEST_FIXTURE_IDENTITY").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        && gitio::common_dir(cwd).is_ok_and(|dir| dir.join("arc-test-fixture").is_file())
}

fn boolean(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        value => value.parse::<i64>().ok().map(|value| value != 0),
    }
}

/// One config read includes all scopes and includes; repeated keys use Git's
/// last-value rule within each scope. No Git process is run per key.
pub fn checkout_root(cwd: &Path) -> Result<std::path::PathBuf> {
    gitio::toplevel(cwd).or_else(|_| {
        let common = gitio::common_dir(cwd)?;
        Ok(if common.file_name().is_some_and(|name| name == ".git") {
            common
                .parent()
                .context("Git directory has no parent")?
                .to_path_buf()
        } else {
            cwd.to_path_buf()
        })
    })
}

pub fn inspect(cwd: &Path) -> Result<Vec<Finding>> {
    let raw = gitio::git(
        cwd,
        &["config", "--null", "--list", "--show-scope", "--includes"],
    )?;
    let mut values = BTreeMap::new();
    let mut fields = raw.split('\0');
    while let Some(scope) = fields.next().filter(|scope| !scope.is_empty()) {
        let entry = fields.next().context("Git config omitted an entry")?;
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        values.insert((scope, key), value);
    }
    let policy = checkout_root(cwd)?.join(".arc/policy.toml");
    let allow_identity = match std::fs::read_to_string(&policy) {
        Ok(text) => {
            let document: toml::Value = toml::from_str(&text)
                .with_context(|| format!("cannot read Git health policy {}", policy.display()))?;
            match document
                .get("git")
                .and_then(|git| git.get("allow_local_identity"))
            {
                Some(value) => value
                    .as_bool()
                    .context("git.allow_local_identity must be a boolean")?,
                None => false,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", policy.display()))
        }
    };
    let fixture = fixture_identity(cwd);
    let mut findings = Vec::new();
    for key in ["user.name", "user.email"] {
        for scope in ["local", "worktree"] {
            if let Some(value) = values.get(&(scope, key)) {
                let seeded = fixture
                    && matches!(
                        (key, *value),
                        ("user.name", "Tester") | ("user.email", "tester@example.invalid")
                    );
                if !allow_identity && !seeded {
                    findings.push(Finding {
                        code: "repository-local-identity",
                        detail: format!("{scope} {key} = {value:?}; remove it with git config --{scope} --unset-all {key}, or declare [git] allow_local_identity = true in .arc/policy.toml"),
                    });
                }
            }
        }
    }
    let common = gitio::common_dir(cwd)?;
    let has_checkout = (common.file_name().is_some_and(|name| name == ".git")
        && common
            .parent()
            .is_some_and(|parent| parent.join(".git").is_dir()))
        || std::fs::read_dir(common.join("worktrees"))
            .is_ok_and(|mut entries| entries.next().is_some());
    let shared_bare = values
        .get(&("local", "core.bare"))
        .or_else(|| values.get(&("global", "core.bare")))
        .or_else(|| values.get(&("system", "core.bare")))
        .and_then(|value| boolean(value))
        == Some(true);
    let checkout_bare = values
        .get(&("worktree", "core.bare"))
        .and_then(|value| boolean(value))
        == Some(true);
    if has_checkout && (shared_bare || checkout_bare) {
        findings.push(Finding {
            code: "bare-with-worktree",
            detail: "core.bare = true in a repository with a checkout or linked worktrees; restore core.bare = false".into(),
        });
    }
    for key in ["commit.gpgsign", "tag.gpgsign"] {
        if values
            .get(&("global", key))
            .and_then(|value| boolean(value))
            == Some(true)
        {
            for scope in ["local", "worktree"] {
                if values.get(&(scope, key)).and_then(|value| boolean(value)) == Some(false) {
                    findings.push(Finding {
                        code: "repository-signing-disabled",
                        detail: format!("{scope} {key} = false overrides global signing; remove the repository override"),
                    });
                }
            }
        }
    }
    Ok(findings)
}

pub fn problems(cwd: &Path) -> Vec<Finding> {
    inspect(cwd).unwrap_or_else(|error| {
        vec![Finding {
            code: "git-config-unreadable",
            detail: format!("{error:#}"),
        }]
    })
}

/// A commit whose raw author or committer address uses a reserved domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservedCommit {
    pub commit: String,
    pub role: &'static str,
    pub email: String,
}

/// How a reserved-domain commit is replaced, shared by the refusal and by
/// readiness's next step so the two never name different remedies.
pub const REAUTHOR: &str = "re-author it with a real identity (for HEAD: git -c user.name='<name>' -c user.email='<email>' commit --amend --reset-author --no-edit; for earlier commits use git rebase -i)";

impl std::fmt::Display for ReservedCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "commit {} has reserved-domain {} address {:?}; {REAUTHOR}, then take a fresh arc snapshot",
            self.commit, self.role, self.email
        )
    }
}

fn reserved_domain(email: &str) -> bool {
    let domain = email
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim_end_matches('.').to_ascii_lowercase())
        .unwrap_or_default();
    [
        "invalid",
        "example",
        "test",
        "localhost",
        "example.com",
        "example.org",
        "example.net",
    ]
    .iter()
    .any(|reserved| domain == *reserved || domain.ends_with(&format!(".{reserved}")))
}

/// The first of `commits`, each `(commit, author email, committer email)`,
/// naming a reserved domain.
pub(crate) fn first_reserved<'a>(
    commits: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> Option<ReservedCommit> {
    commits.into_iter().find_map(|(commit, author, committer)| {
        [("author", author), ("committer", committer)]
            .into_iter()
            .find(|(_, email)| reserved_domain(email))
            .map(|(role, email)| ReservedCommit {
                commit: commit.to_string(),
                role,
                email: email.to_string(),
            })
    })
}

/// The first commit in `base..head` with a reserved-domain address, read from
/// the raw objects: mailmap presentation cannot replace raw identities.
pub fn reserved_identity(cwd: &Path, base: &str, head: &str) -> Result<Option<ReservedCommit>> {
    if fixture_identity(cwd) {
        return Ok(None);
    }
    type CommitRange = (std::path::PathBuf, String, String);
    static CHECKED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<CommitRange>>> =
        std::sync::OnceLock::new();
    let key = (gitio::is_full_object_id(base) && gitio::is_full_object_id(head))
        .then(|| (cwd.to_path_buf(), base.to_string(), head.to_string()));
    let checked = CHECKED.get_or_init(Default::default);
    if key.as_ref().is_some_and(|key| {
        checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(key)
    }) {
        return Ok(None);
    }
    let found = match gitio::listing::range_identities(cwd, base, head) {
        Some(commits) => first_reserved(commits.iter().map(|(commit, author, committer)| {
            (commit.as_str(), author.as_str(), committer.as_str())
        })),
        None => {
            let log = gitio::git(
                cwd,
                &[
                    "--no-replace-objects",
                    "log",
                    "--no-show-signature",
                    "--no-notes",
                    "-z",
                    "--format=%H%x00%ae%x00%ce",
                    head,
                    "--not",
                    base,
                ],
            )?;
            let mut fields = log.split('\0');
            let mut commits = Vec::new();
            while let Some(commit) = fields.next().filter(|commit| !commit.is_empty()) {
                let author = fields.next().context("Git log omitted author email")?;
                let committer = fields.next().context("Git log omitted committer email")?;
                commits.push((commit, author, committer));
            }
            first_reserved(commits)
        }
    };
    if let (None, Some(key)) = (&found, key) {
        checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key);
    }
    Ok(found)
}

/// Patchset commits must name people, including commits authored before a
/// guard was installed.
pub fn guard_patchset(cwd: &Path, base: &str, head: &str) -> Result<()> {
    match reserved_identity(cwd, base, head)? {
        Some(reserved) => anyhow::bail!("{reserved}"),
        None => Ok(()),
    }
}

/// The reserved-domain commit a patchset would ship onto `target`: one in
/// `base..head`, else in `target..head`. Integration refuses on it and
/// readiness reports it, both through this one reading.
pub fn shipped_reserved(
    cwd: &Path,
    base: &str,
    head: &str,
    target: Option<&str>,
) -> Result<Option<ReservedCommit>> {
    if let Some(reserved) = reserved_identity(cwd, base, head)? {
        return Ok(Some(reserved));
    }
    match target.filter(|target| *target != base) {
        Some(target) => reserved_identity(cwd, target, head),
        None => Ok(None),
    }
}

/// Refuse a patchset [`shipped_reserved`] finds a reserved-domain commit in.
pub fn guard_shipped_patchset(cwd: &Path, base: &str, head: &str, target: &str) -> Result<()> {
    match shipped_reserved(cwd, base, head, Some(target))? {
        Some(reserved) => anyhow::bail!("{reserved}"),
        None => Ok(()),
    }
}
