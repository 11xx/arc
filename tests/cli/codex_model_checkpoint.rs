use crate::common::*;
use serde_json::json;
use std::io::Write;

#[test]
fn env_and_writes_resolve_a_codex_selection_beyond_the_four_mib_tail() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000020";
    let codex_home = repo.home.join("codex-state");
    let day = codex_home.join("sessions/2026/01/01");
    fs::create_dir_all(&day).unwrap();
    let path = day.join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"));
    let mut file = fs::File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        json!({"type":"session_meta", "payload":{"id":session}})
    )
    .unwrap();
    writeln!(file, "{}", json!({"type":"turn_context", "timestamp":"2026-01-01T00:00:01Z", "payload":{"model":"fixture-model", "effort":"high"}})).unwrap();
    let padding =
        json!({"type":"event_msg", "payload":{"type":"agent_message", "message":"x".repeat(4096)}});
    for _ in 0..1536 {
        writeln!(file, "{padding}").unwrap();
    }
    drop(file);
    let acting = || {
        let mut command = repo.arc(&repo.root);
        command
            .env("CODEX_HOME", &codex_home)
            .env("CODEX_THREAD_ID", session)
            .env("ARC_HARNESS", "codex")
            .env("ARC_SESSION", session);
        command
    };
    let env = stdout(acting().arg("env"));
    assert!(env.contains("# model: fixture-model#high"), "{env}");
    assert!(env.contains("unset ARC_MODEL"));
    stdout(acting().args(["begin", "long-model", "--no-worktree"]));
    let events = stdout(acting().args(["events", "--change", "long-model"]));
    let opening: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(opening["model"], "fixture-model#high");
    assert_eq!(opening["model_observation"]["head_read"], false);
}
