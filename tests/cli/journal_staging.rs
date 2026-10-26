use crate::common::*;

fn artifact(repo: &Repo) -> (PathBuf, String) {
    let output = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                "staging",
                "--kind",
                "todo",
                "--body-file",
                "-",
            ])
            .write_stdin("a private passage\n"),
    );
    let name = Path::new(output.lines().last().unwrap().trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let dir = PathBuf::from(stdout(repo.arc(&repo.root).args(["journal", "dir"])).trim());
    (dir, name)
}

/// Claim: redaction changes only the artifact and its owned staging file.
/// Rejects: following a pre-existing staging symlink and truncating its target.
#[test]
fn redaction_preserves_an_unrelated_staging_symlink_and_its_target() {
    let repo = Repo::new();
    let (journal, name) = artifact(&repo);
    let unrelated = repo.home.join("unrelated.txt");
    fs::write(&unrelated, "retained unrelated bytes\n").unwrap();
    let staging = journal.join(format!(".{name}.redacting"));
    std::os::unix::fs::symlink(&unrelated, &staging).unwrap();

    repo.arc(&repo.root)
        .args([
            "journal",
            "redact",
            &name,
            "--replace",
            "private",
            "--with",
            "public",
            "--reason",
            "remove a passage",
        ])
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&unrelated).unwrap(),
        "retained unrelated bytes\n",
        "redaction overwrote the unowned staging symlink's target"
    );
    assert!(fs::symlink_metadata(&staging).unwrap().is_symlink());
    let path = journal.join(&name);
    assert!(fs::symlink_metadata(&path).unwrap().is_file());
    assert!(fs::read_to_string(path)
        .unwrap()
        .contains("a public passage"));
}

/// Claim: relocation preserves files that the caller already holds beside
/// its destination. Rejects: deleting an unowned file to reuse its name.
#[test]
fn relocation_preserves_an_unrelated_staging_file() {
    let repo = Repo::new();
    let (journal, name) = artifact(&repo);
    let before = fs::read(journal.join(&name)).unwrap();
    let outside = repo.home.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let staging = outside.join(format!(".{name}.relocating"));
    fs::write(&staging, "retained unrelated bytes\n").unwrap();

    repo.arc(&repo.root)
        .args(["journal", "relocate", &name, "--to"])
        .arg(&outside)
        .args(["--reason", "move an artifact"])
        .assert()
        .success();

    assert!(
        staging.is_file(),
        "relocation removed an unowned staging file"
    );
    assert_eq!(
        fs::read_to_string(staging).unwrap(),
        "retained unrelated bytes\n"
    );
    assert_eq!(fs::read(outside.join(&name)).unwrap(), before);
    assert!(!journal.join(&name).exists());
}
