use super::common::*;

const FORMATS: [&str; 2] = ["sha1", "sha256"];

/// Every file under each of `dirs`, by path, with a digest of its bytes.
fn files(dirs: &[&Path]) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                let digest = hex::encode(Sha256::digest(fs::read(&path).unwrap()));
                found.push((path, digest));
            }
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, &mut found);
    }
    found.sort();
    found
}

/// Rejects a fixture that runs Git itself, or rebuilds its seed: the Git
/// commands spent on fixtures stay at the seed's initializer, once per object
/// format, however many fixtures the suite creates.
#[test]
fn fixtures_run_no_git_beyond_one_seed_per_object_format() {
    for format in FORMATS {
        drop(Repo::with_object_format(format));
        let (spawns, fixtures) = git_spawns_during(|| {
            (0..3)
                .map(|_| Repo::with_object_format(format))
                .collect::<Vec<_>>()
        });
        assert_eq!(spawns, 0, "{format} fixtures ran Git");
        assert_eq!(seed(format).initializer_spawns, 6, "{format} seed");
        assert_eq!(
            git_out(&fixtures[0].root, &["rev-parse", "--show-object-format"]),
            format
        );
    }
}

/// Rejects a copied index whose stat data or seal still describes the seed: a
/// fresh fixture is clean to plumbing that trusts stat data, and a status
/// leaves its index as it found it, as a fresh commit's index is.
#[test]
fn a_copied_fixture_is_clean_without_a_refresh() {
    for format in FORMATS {
        let repo = Repo::with_object_format(format);
        let git_files = files(&[&repo.root.join(".git")]);
        assert_eq!(git_out(&repo.root, &["diff-files", "--name-only"]), "");
        assert_eq!(git_out(&repo.root, &["status", "--porcelain"]), "");
        assert_eq!(
            files(&[&repo.root.join(".git")]),
            git_files,
            "{format} status rewrote the copied repository"
        );
        git(&repo.root, &["fsck", "--no-progress"]);
    }
}

/// Rejects fixtures that share mutable state with each other or the seed:
/// changing one fixture's config, refs, index, worktree administration and
/// ledger changes no file of another fixture or of the seed.
#[test]
fn mutating_one_fixture_leaves_another_and_the_seed_unchanged() {
    for format in FORMATS {
        let mutated = Repo::with_object_format(format);
        let bystander = Repo::with_object_format(format);
        let bystander_dirs = [bystander.root.as_path(), bystander.home.as_path()];
        let bystander_files = files(&bystander_dirs);
        let seed_files = files(&[&seed(format).root]);

        git(&mutated.root, &["config", "user.name", "Mutated"]);
        mutated.commit(&mutated.root, "README.md", "mutated\n", "test: mutate");
        let side = mutated.home.join("side");
        git(
            &mutated.root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                side.to_str().unwrap(),
            ],
        );
        begin_change(&mutated, "mutation", None);

        let mutated_git = mutated.root.join(".git");
        for file in ["config", "index", "refs/heads/master", "refs/heads/side"] {
            assert!(
                fs::read(mutated_git.join(file)).ok()
                    != fs::read(bystander.root.join(".git").join(file)).ok(),
                "{format} mutation did not reach {file}"
            );
        }
        assert!(mutated_git.join("worktrees").is_dir());
        assert!(mutated_git.join("arc/changes").is_dir());
        assert_eq!(
            files(&bystander_dirs),
            bystander_files,
            "{format} bystander"
        );
        assert_eq!(files(&[&seed(format).root]), seed_files, "{format} seed");
    }
}
