use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn watch(path: PathBuf) {
    println!("cargo:rerun-if-changed={}", path.display());
}

fn main() {
    watch(PathBuf::from("build.rs"));
    watch(PathBuf::from("Cargo.toml"));
    watch(PathBuf::from("src"));
    let commit = git(&["rev-parse", "HEAD"]);
    let dirty = commit.as_ref().and_then(|_| {
        git(&["status", "--porcelain", "--untracked-files=no"]).map(|status| {
            if status.is_empty() {
                "clean"
            } else {
                "dirty"
            }
        })
    });
    if let (Some(git_dir), Some(common_dir)) = (
        git(&["rev-parse", "--git-dir"]),
        git(&["rev-parse", "--git-common-dir"]),
    ) {
        let git_dir = PathBuf::from(git_dir);
        let common_dir = PathBuf::from(common_dir);
        watch(git_dir.join("HEAD"));
        watch(git_dir.join("index"));
        watch(common_dir.join("packed-refs"));
        if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch(common_dir.join(branch));
        }
    }
    println!(
        "cargo:rustc-env=ARC_BUILD_COMMIT={}",
        commit.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=ARC_BUILD_DIRTY={}",
        dirty.unwrap_or("unknown")
    );
}
