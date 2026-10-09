use crate::common::*;
use std::os::unix::fs::PermissionsExt;

const CODEX_SESSION: &str = "019f0000-0000-7000-8000-000000000002";
const UNRESOLVED_MODEL: &str =
    "# export ARC_MODEL=<model[#effort]>  # unavailable: the session store could not resolve or read this id: ";
const UNRESOLVED_SESSION: &str = "# session unresolved: the codex session store could not establish whether this id has a readable recording: ";

/// `arc env` in a process whose Codex thread is `session`.
fn codex_env(repo: &Repo, session: &str) -> String {
    stdout(
        repo.arc(&repo.root)
            .arg("env")
            .env("CODEX_HOME", repo.home.join("codex-state"))
            .env("CODEX_THREAD_ID", session),
    )
}

/// The text after `prefix` on the one `arc env` line that starts with it.
fn reason_after<'a>(output: &'a str, prefix: &str) -> &'a str {
    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .collect();
    assert_eq!(lines.len(), 1, "one line starts with {prefix:?}:\n{output}");
    lines[0]
}

#[test]
fn an_unreadable_recording_names_its_permission_error_in_env_and_on_events() {
    let repo = Repo::new();
    let path = codex_recording(&repo, CODEX_SESSION, "gpt-fixture");

    let readable = codex_env(&repo, CODEX_SESSION);
    assert!(readable.contains("# session corroborated"), "{readable}");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = codex_env(&repo, CODEX_SESSION);
    let reason = reason_after(&unreadable, UNRESOLVED_SESSION);
    assert!(
        reason.ends_with("open file failed (PermissionDenied)"),
        "{unreadable}"
    );
    assert_eq!(reason_after(&unreadable, UNRESOLVED_MODEL), reason);
    assert!(!unreadable.contains("gpt-fixture"), "{unreadable}");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn a_prefix_match_is_unresolved_and_names_both_ids() {
    let repo = Repo::new();
    let prefix = "019f0000";
    codex_recording(&repo, CODEX_SESSION, "gpt-fixture");
    let expected =
        format!("session {prefix} resolves to the recording of a different id, {CODEX_SESSION}");

    let output = codex_env(&repo, prefix);
    assert_eq!(reason_after(&output, UNRESOLVED_SESSION), expected);
    assert_eq!(reason_after(&output, UNRESOLVED_MODEL), expected);
    assert!(!output.contains("uncorroborated"), "{output}");
    assert!(!output.contains("gpt-fixture"), "{output}");
}
