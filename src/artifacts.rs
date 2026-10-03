//! Recognizes ignored build-artifact directories from their contents and resolved
//! rules. Recognition is independent of checkout ownership, change state, and
//! who created the checkout; it grants no authority to remove a checkout.

use serde::Deserialize;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const CACHE_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub path: String,
    pub marker: String,
    pub signature: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Artifact {
    pub path: String,
    pub identified_by: String,
}

pub fn valid_relative_path(path: &str) -> bool {
    !Path::new(path).is_absolute() && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Only Git's `!! path/` entries are eligible. Files, links, untracked entries,
/// and tracked changes remain outside recognition, regardless of rule matches.
pub fn recognize(root: &Path, entries: &[String], rules: &[Rule]) -> Vec<Artifact> {
    entries
        .iter()
        .filter_map(|entry| {
            let path = entry.strip_prefix("!! ")?.strip_suffix('/')?;
            if !valid_relative_path(path) {
                return None;
            }
            let directory = real_directory(root, path)?;
            let identified_by = if marker_matches(&directory, "CACHEDIR.TAG", Some(CACHE_SIGNATURE))
            {
                "CACHEDIR.TAG".to_string()
            } else {
                let (index, rule) = rules.iter().enumerate().find(|(_, rule)| {
                    path_matches(&rule.path, path)
                        && marker_matches(
                            &directory,
                            &rule.marker,
                            rule.signature.as_deref().map(str::as_bytes),
                        )
                })?;
                format!("rule {}: marker {}", index + 1, rule.marker)
            };
            Some(Artifact {
                path: format!("{path}/"),
                identified_by,
            })
        })
        .collect()
}

fn real_directory(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut directory = root.to_path_buf();
    for component in relative.split('/') {
        directory.push(component);
        if !fs::symlink_metadata(&directory).ok()?.is_dir() {
            return None;
        }
    }
    Some(directory)
}

fn marker_matches(directory: &Path, marker: &str, signature: Option<&[u8]>) -> bool {
    if !valid_relative_path(marker) {
        return false;
    }
    let mut components = marker.rsplitn(2, '/');
    let name = components.next().unwrap();
    let parent = match components.next() {
        Some(parent) => match real_directory(directory, parent) {
            Some(parent) => parent,
            None => return false,
        },
        None => directory.to_path_buf(),
    };
    let marker = parent.join(name);
    if !fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Some(signature) = signature else {
        return true;
    };
    let Ok(file) = fs::File::open(marker) else {
        return false;
    };
    let mut prefix = Vec::new();
    file.take(signature.len() as u64)
        .read_to_end(&mut prefix)
        .is_ok()
        && prefix == signature
}

fn path_matches(pattern: &str, path: &str) -> bool {
    fn components_match(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => {
                components_match(rest, path)
                    || (!path.is_empty() && components_match(pattern, &path[1..]))
            }
            Some((component, rest)) => path.first().is_some_and(|name| {
                component_matches(component, name) && components_match(rest, &path[1..])
            }),
        }
    }
    components_match(
        &pattern.split('/').collect::<Vec<_>>(),
        &path.split('/').collect::<Vec<_>>(),
    )
}

fn component_matches(pattern: &str, name: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == name;
    }
    let Some(mut remaining) = name.strip_prefix(parts[0]) else {
        return false;
    };
    for part in &parts[1..parts.len() - 1] {
        let Some(offset) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[offset + part.len()..];
    }
    remaining.ends_with(parts[parts.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_respects_whole_components() {
        for (pattern, path, expected) in [
            ("target", "targets", false),
            ("target", "nested/target", false),
            ("tar*", "target", true),
            ("tar*", "tar/get", false),
            ("*", "target", true),
            ("a*b*c", "axbyc", true),
            ("a*b", "ab", true),
            ("a*b", "a", false),
            ("*a*", "ba", true),
            ("**/target", "target", true),
            ("**/target", "a/b/target", true),
            ("a/**/target", "a/target", true),
            ("a/**/target", "a/b/c/target", true),
            ("a/**", "a", true),
            ("a/**", "a/b/c", true),
            ("**", "a/b", true),
            ("a/*/target", "a/target", false),
            ("a/*/target", "a/b/c/target", false),
            ("a/*/target", "a/b/target", true),
            ("**/dist-newstyle", "dist-newstyles", false),
            ("t?rget", "target", false),
            ("[t]arget", "target", false),
        ] {
            assert_eq!(path_matches(pattern, path), expected, "{pattern}: {path}");
        }
    }

    #[test]
    fn recognition_on_primary_fixture_needs_no_ownership() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        crate::gitio::git(root, &["init"]).unwrap();
        fs::write(root.join(".gitignore"), "target/\ndist-newstyle/\n.env\n").unwrap();
        fs::create_dir_all(root.join("nested/target/debug")).unwrap();
        fs::write(root.join("nested/keep"), "tracked").unwrap();
        crate::gitio::git(root, &["add", ".gitignore", "nested/keep"]).unwrap();
        fs::write(
            root.join("nested/target/CACHEDIR.TAG"),
            [CACHE_SIGNATURE, b"\n# cache"].concat(),
        )
        .unwrap();
        fs::write(root.join("nested/target/debug/output"), b"artifact").unwrap();
        fs::create_dir_all(root.join("dist-newstyle/cache")).unwrap();
        fs::write(root.join("dist-newstyle/cache/plan.json"), b"plan: bytes").unwrap();
        fs::write(root.join(".env"), b"local").unwrap();
        let rules = vec![Rule {
            path: "**/dist-newstyle".into(),
            marker: "cache/plan.json".into(),
            signature: Some("plan:".into()),
        }];
        let status = crate::gitio::git(
            root,
            &["status", "--porcelain", "-z", "--ignored=traditional"],
        )
        .unwrap();
        let entries = status.split('\0').map(str::to_string).collect::<Vec<_>>();
        let artifacts = recognize(root, &entries, &rules);
        assert_eq!(artifacts.len(), 2, "{status:?}");
        assert!(artifacts
            .iter()
            .any(|a| a.path == "nested/target/" && a.identified_by == "CACHEDIR.TAG"));
        assert!(artifacts
            .iter()
            .any(|a| a.path == "dist-newstyle/"
                && a.identified_by == "rule 1: marker cache/plan.json"));
        assert_eq!(recognize(root, &entries, &[]).len(), 1);
    }

    #[test]
    fn recognition_rejects_non_directories_and_wrong_signatures() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::create_dir_all(root.join("target/cache")).unwrap();
        fs::write(root.join("target/cache/tag"), b"prefix: content").unwrap();
        let entries = vec!["!! target/".into()];
        let mut rule = Rule {
            path: "target".into(),
            marker: "cache/tag".into(),
            signature: Some("prefix:".into()),
        };
        assert_eq!(recognize(root, &entries, &[rule.clone()]).len(), 1);
        rule.signature = Some("prefix: content extra".into());
        assert!(recognize(root, &entries, &[rule.clone()]).is_empty());
        rule.signature = None;
        for entry in [
            "?? target/",
            " M target/",
            "!! target/cache/tag",
            "!! ../target/",
        ] {
            assert!(recognize(root, &[entry.into()], &[rule.clone()]).is_empty());
        }
        std::os::unix::fs::symlink("target", root.join("linked")).unwrap();
        rule.path = "linked".into();
        assert!(recognize(root, &["!! linked/".into()], &[rule.clone()]).is_empty());
        std::os::unix::fs::symlink("cache/tag", root.join("target/CACHEDIR.TAG")).unwrap();
        assert!(recognize(root, &entries, &[]).is_empty());
        rule.path = "target".into();
        rule.marker = "CACHEDIR.TAG".into();
        assert!(recognize(root, &entries, &[rule.clone()]).is_empty());
        std::os::unix::fs::symlink("cache", root.join("target/link")).unwrap();
        rule.marker = "link/tag".into();
        assert!(recognize(root, &entries, &[rule]).is_empty());
    }
}
