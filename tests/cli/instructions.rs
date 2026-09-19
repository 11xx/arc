//! `arc instructions git` is a portable, read-only entry point: one canonical
//! contribution-trailer specification, printed from outside any repository
//! and checked without rewriting a message.

use crate::common::*;

const SPEC: &str = include_str!("../../docs/contribution-trailers.md");

#[test]
fn instructions_git_prints_the_canonical_spec_outside_a_repository() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(tmp.path())
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["instructions", "git"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        SPEC,
        "the printed guide is the canonical page, not a copy"
    );

    // Guidance only: no arc lifecycle command reaches the commit.
    let text = String::from_utf8_lossy(&output.stdout);
    for forbidden in [
        "arc begin",
        "arc integrate",
        "arc close",
        "arc claim",
        "arc snapshot",
        "arc review",
    ] {
        assert!(
            !text.contains(forbidden),
            "lifecycle text {forbidden:?} in the Git guide: {text}"
        );
    }
    // No configuration, and no file created.
    let entries: Vec<_> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(entries.is_empty(), "{entries:?}");
}

#[test]
fn instructions_git_check_separates_malformed_from_unsupported() {
    let repo = Repo::new();
    let path = repo.home.join("message.txt");
    let message = "feat: one\n\n\
        Planned-by: codex:planner-model#high\n\
        Implemented-by: codex:\n\
        Reviewed-by: Ada Example <ada@example.invalid>\n\
        Assisted-by: pi:deepseek-flash#max\n\
        Signed-off-by: Ada Example <ada@example.invalid>\n";
    fs::write(&path, message).unwrap();

    let output = repo
        .arc(&repo.root)
        .args(["instructions", "git", "--check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("malformed: Implemented-by"), "{report}");
    assert!(report.contains("unsupported: Signed-off-by"), "{report}");
    assert!(
        !report.contains("Assisted-by"),
        "legacy disclosure is defined, not unsupported: {report}"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        message,
        "the check never rewrites the message"
    );

    // An unsupported key alone is a report, not a failure.
    fs::write(&path, "feat: one\n\nCustom-by: someone\n").unwrap();
    let output = repo
        .arc(&repo.root)
        .args(["instructions", "git", "--check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("unsupported: Custom-by"), "{report}");
    assert!(!report.contains("malformed"), "{report}");
}

/// The specification's example spellings parse with no configuration, which
/// is the interoperability the convention claims.
#[test]
fn documented_trailer_examples_parse_with_plain_git() {
    let message = "feat: one\n\n\
        Planned-by: codex:planner-model#high\n\
        Implemented-by: codex:executor-model#medium\n\
        Reviewed-by: codex:reviewer-model#high\n\
        Orchestrated-by: claude:lead-model#high\n\
        Reviewed-by: Ada Example <ada@example.invalid>\n";
    let mut command = Command::new("git");
    command
        .args(["interpret-trailers", "--parse"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(message.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = String::from_utf8_lossy(&output.stdout);
    for line in [
        "Planned-by: codex:planner-model#high",
        "Implemented-by: codex:executor-model#medium",
        "Reviewed-by: codex:reviewer-model#high",
        "Orchestrated-by: claude:lead-model#high",
        "Reviewed-by: Ada Example <ada@example.invalid>",
    ] {
        assert!(parsed.contains(line), "{line} missing from: {parsed}");
    }
}
