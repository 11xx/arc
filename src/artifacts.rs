//! Recognizes ignored build-artifact directories from their contents: a valid
//! CACHEDIR.TAG, a built-in recognizer, or a resolved rule. Every recognizer
//! names a file its tool writes inside the directory, never a directory name
//! alone. Recognition is independent of checkout ownership, change state, and
//! who created the checkout; it grants no authority to remove a checkout.

use serde::Deserialize;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const CACHE_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// A recognizer every checkout gets. Its marker is a file the tool writes at a
/// fixed place inside the directory, beginning with the bytes its format fixes.
/// A marker shows only that the tool ran there, so `layout` must also exclude
/// every place the tool's own workflow puts sources: a built-in exists only
/// for a directory whose every entry the tool writes and regenerates.
struct Builtin {
    tool: &'static str,
    path: &'static str,
    marker: &'static str,
    signature: &'static [u8],
    layout: fn(&Path) -> bool,
}

const BUILTIN: &[Builtin] = &[
    Builtin {
        tool: "Cabal",
        path: "**/dist-newstyle",
        marker: "cache/plan.json",
        signature: b"{\"cabal-version\":",
        layout: no_dependency_checkouts,
    },
    Builtin {
        tool: "Python package metadata",
        path: "**/*.egg-info",
        marker: "PKG-INFO",
        signature: b"Metadata-Version: ",
        layout: |_| true,
    },
];

/// Cabal clones source-repository-package dependencies under
/// `dist-newstyle/src`, where they can carry edits no plan regenerates.
fn no_dependency_checkouts(directory: &Path) -> bool {
    matches!(
        fs::symlink_metadata(directory.join("src")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

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
            } else if let Some(builtin) = BUILTIN.iter().find(|builtin| {
                path_matches(builtin.path, path)
                    && marker_matches(&directory, builtin.marker, Some(builtin.signature))
                    && (builtin.layout)(&directory)
            }) {
                format!("{}: {}", builtin.tool, builtin.marker)
            } else if python_bytecode_cache(&directory, path) {
                "Python bytecode: __pycache__".to_string()
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

/// Python's `__pycache__` holds only `<module>.<implementation tag>.pyc` files,
/// each opening with a magic number whose last two bytes are `\r\n`. Any other
/// entry keeps the directory.
fn python_bytecode_cache(directory: &Path, path: &str) -> bool {
    if path.rsplit('/').next() != Some("__pycache__") {
        return false;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    let mut found = false;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let is_pyc = entry.file_name().to_str().is_some_and(|name| {
            name.ends_with(".pyc") && (name.contains(".cpython-") || name.contains(".pypy"))
        });
        if !is_pyc || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            return false;
        }
        let mut magic = [0; 4];
        let read = fs::File::open(entry.path()).and_then(|mut file| file.read_exact(&mut magic));
        if read.is_err() || magic[2..] != *b"\r\n" {
            return false;
        }
        found = true;
    }
    found
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

    fn recognized(root: &Path, directory: &str) -> Option<String> {
        let entry = vec![format!("!! {directory}/")];
        recognize(root, &entry, &[])
            .pop()
            .map(|artifact| artifact.identified_by)
    }

    #[test]
    fn tool_state_markers_need_a_configured_rule() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let write = |path: &str, content: &[u8]| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        };
        // An out-of-source CMake build holds FetchContent dependency sources.
        write("build/CMakeCache.txt", b"# This is the CMakeCache file.\n");
        write("build/_deps/dep-src/dep.c", b"int patched(void);");
        // A virtual environment created over an application keeps its
        // sources, and an installed package can carry a local patch.
        write("app/pyvenv.cfg", b"home = /usr/bin\n");
        write("app/bin/application.py", b"print('kept')");
        write(".venv/pyvenv.cfg", b"home = /usr/bin\n");
        write(".venv/lib/python3/site-packages/pip/patched.py", b"");
        for directory in ["build", "app", ".venv"] {
            assert_eq!(recognized(root, directory), None, "{directory}");
        }
        let rule = Rule {
            path: "build".into(),
            marker: "CMakeCache.txt".into(),
            signature: Some("# This is the CMakeCache file.".into()),
        };
        let declared = recognize(root, &["!! build/".into()], &[rule]);
        assert_eq!(declared[0].identified_by, "rule 1: marker CMakeCache.txt");
    }

    #[test]
    fn builtin_recognizers_need_marker_and_layout() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let write = |path: &str, content: &[u8]| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        };
        write(
            "pkg/dist-newstyle/cache/plan.json",
            b"{\"cabal-version\":\"3.16.1.0\"}",
        );
        write("pkg/dist-newstyle/build/output", b"object");
        assert_eq!(
            recognized(root, "pkg/dist-newstyle").as_deref(),
            Some("Cabal: cache/plan.json")
        );
        // Dependency checkouts can carry edits no plan regenerates.
        write("pkg/dist-newstyle/src/dep/Lib.hs", b"module Lib where");
        assert_eq!(recognized(root, "pkg/dist-newstyle"), None);

        write(
            "src/pkg.egg-info/PKG-INFO",
            b"Metadata-Version: 2.4\nName: pkg\n",
        );
        assert_eq!(
            recognized(root, "src/pkg.egg-info").as_deref(),
            Some("Python package metadata: PKG-INFO")
        );
        write("src/pkg.egg-info/PKG-INFO", b"Name: pkg\n");
        assert_eq!(recognized(root, "src/pkg.egg-info"), None);

        // Markers that cannot rule out hand-written or unsaved work.
        write("out/.ninja_log", b"# ninja log v7\n");
        write("node_modules/.package-lock.json", b"{}");
        write("web/node_modules/.modules.yaml", b"{}");
        write("ui/node_modules/.yarn-integrity", b"{}");
        for directory in ["out", "node_modules", "web/node_modules", "ui/node_modules"] {
            assert_eq!(recognized(root, directory), None, "{directory}");
        }
        fs::create_dir_all(root.join("dist-newstyles/cache")).unwrap();
        fs::write(
            root.join("dist-newstyles/cache/plan.json"),
            b"{\"cabal-version\":\"3.16.1.0\"}",
        )
        .unwrap();
        assert_eq!(recognized(root, "dist-newstyles"), None);
    }

    #[test]
    fn python_bytecode_cache_holds_only_compiled_modules() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        let cache = root.join("pkg/__pycache__");
        fs::create_dir_all(&cache).unwrap();
        let entry = vec!["!! pkg/__pycache__/".to_string()];
        assert!(recognize(root, &entry, &[]).is_empty(), "empty cache");
        fs::write(cache.join("m.cpython-314.pyc"), b"\x2b\x0e\r\n\0\0\0\0").unwrap();
        fs::write(
            cache.join("m.cpython-314.opt-1.pyc"),
            b"\x2b\x0e\r\n\0\0\0\0",
        )
        .unwrap();
        let found = recognize(root, &entry, &[]);
        assert_eq!(found[0].identified_by, "Python bytecode: __pycache__");
        fs::write(cache.join("notes.txt"), b"kept").unwrap();
        assert!(recognize(root, &entry, &[]).is_empty(), "foreign file");
        fs::remove_file(cache.join("notes.txt")).unwrap();
        fs::write(cache.join("bad.cpython-314.pyc"), b"not bytecode").unwrap();
        assert!(recognize(root, &entry, &[]).is_empty(), "wrong magic");
        fs::remove_file(cache.join("bad.cpython-314.pyc")).unwrap();
        std::os::unix::fs::symlink("m.cpython-314.pyc", cache.join("l.cpython-314.pyc")).unwrap();
        assert!(recognize(root, &entry, &[]).is_empty(), "symlinked module");
        fs::remove_file(cache.join("l.cpython-314.pyc")).unwrap();
        fs::write(cache.join("plain.pyc"), b"\x2b\x0e\r\n\0\0\0\0").unwrap();
        assert!(
            recognize(root, &entry, &[]).is_empty(),
            "no implementation tag"
        );
        fs::remove_file(cache.join("plain.pyc")).unwrap();
        fs::create_dir_all(root.join("pkg/cache")).unwrap();
        fs::write(root.join("pkg/cache/m.cpython-314.pyc"), b"\x2b\x0e\r\n").unwrap();
        assert!(
            recognize(root, &["!! pkg/cache/".into()], &[]).is_empty(),
            "name"
        );
    }
}
