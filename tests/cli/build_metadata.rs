use super::common::*;
use std::time::SystemTime;

/// Claim: build metadata reads preserve the index while reporting the commit
/// and actual cleanliness. Rejects an index refresh that invalidates Cargo's
/// watched input and repeats compilation, or a read that hides dirty content.
#[test]
fn metadata_reads_preserve_the_index_and_report_actual_content() {
    let repo = Repo::new();
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
