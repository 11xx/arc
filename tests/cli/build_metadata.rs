use super::common::*;
use std::time::SystemTime;

/// This checkout's `build.rs`, compiled into `repo`'s home as a program.
fn build_script(repo: &Repo) -> PathBuf {
    let program = repo.home.join("build-metadata");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs");
    let built = Command::new("rustc")
        .arg("--edition=2021")
        .arg(source)
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    program
}

/// Claim: build metadata reads preserve the index while reporting the commit
/// and actual cleanliness. Rejects an index refresh that invalidates Cargo's
/// watched input and repeats compilation, or a read that hides dirty content.
#[test]
fn metadata_reads_preserve_the_index_and_report_actual_content() {
    let repo = Repo::new();
    let program = build_script(&repo);
    fs::write(
        repo.root.join("Cargo.toml"),
        "agent-tapes-core = { version = \"2000.1.1\", rev = \"fixture\" }\n",
    )
    .unwrap();
    let index = repo.root.join(".git/index");
    let before = fs::read(&index).unwrap();
    let readme = repo.root.join("README.md");
    fs::File::open(&readme)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    let metadata = stdout(&mut repo.fixture_env(AssertCommand::new(&program), &repo.root));
    assert!(metadata.contains(&format!("ARC_BUILD_COMMIT={}", repo.head(&repo.root))));
    assert!(metadata.contains("ARC_BUILD_DIRTY=clean"));
    assert_eq!(
        fs::read(&index).unwrap(),
        before,
        "metadata reads must not rewrite the index"
    );
    fs::write(&readme, "changed content\n").unwrap();
    let metadata = stdout(&mut repo.fixture_env(AssertCommand::new(&program), &repo.root));
    assert!(metadata.contains("ARC_BUILD_DIRTY=dirty"));
    assert_eq!(fs::read(index).unwrap(), before);
}

/// Claim: the build script starts Git with a Git environment of its own: run
/// under an inherited `GIT_DIR` naming another repository and a
/// `GIT_TRACE2_EVENT` target, it reports the commit of the checkout it runs
/// in and no trace is written. Rejects a build phase that an inherited
/// selector steers to another repository or a trace target makes write.
#[test]
fn the_build_script_runs_git_without_the_inherited_git_environment() {
    let repo = Repo::new();
    let other = Repo::new();
    other.commit(&other.root, "other.txt", "other\n", "test: another head");
    let program = build_script(&repo);
    fs::write(
        repo.root.join("Cargo.toml"),
        "agent-tapes-core = { version = \"2000.1.1\", rev = \"fixture\" }\n",
    )
    .unwrap();
    let trace = repo.home.join("trace2.json");
    let metadata = stdout(
        repo.fixture_env(AssertCommand::new(&program), &repo.root)
            .env("GIT_DIR", other.root.join(".git"))
            .env("GIT_TRACE2_EVENT", &trace),
    );
    assert!(
        metadata.contains(&format!("ARC_BUILD_COMMIT={}", repo.head(&repo.root))),
        "{metadata}"
    );
    assert!(
        !trace.exists(),
        "the build script's Git wrote the inherited trace target"
    );
}
