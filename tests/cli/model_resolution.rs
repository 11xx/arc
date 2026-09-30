use crate::common::*;
use serde_json::{json, Value};

const SESSION: &str = "11111111-2222-3333-4444-555555555555";

fn recording(repo: &Repo, rows: &[Value]) -> PathBuf {
    let project = repo.home.join(".claude/projects/-fixture");
    fs::create_dir_all(&project).unwrap();
    let path = project.join(format!("{SESSION}.jsonl"));
    fs::write(
        &path,
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    path
}

fn prompt(id: &str, ts: &str) -> Value {
    json!({"type":"user", "promptSource":"typed", "uuid":id, "timestamp":ts,
        "message":{"role":"user", "content":"operator prompt"}})
}

fn assistant(id: &str, ts: &str, effort: &str) -> Value {
    json!({"type":"assistant", "uuid":id, "timestamp":ts, "perTurnEffort":effort,
        "message":{"role":"assistant", "model":"m", "content":"answer"}})
}

fn acting(repo: &Repo) -> AssertCommand {
    let mut cmd = repo.arc(&repo.root);
    cmd.env("ARC_HARNESS", "claude")
        .env("ARC_SESSION", SESSION)
        .env("CLAUDE_SESSION_ID", SESSION)
        .env("CLAUDE_CONFIG_DIR", repo.home.join(".claude"));
    cmd
}

fn begin(repo: &Repo) -> String {
    let out = stdout(acting(repo).args(["begin", "model-evidence"]));
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

#[test]
fn writes_resolve_the_newest_selection_each_time_and_render_its_coordinate() {
    let repo = Repo::new();
    let mut rows = vec![
        prompt("u1", "2026-01-01T10:00:00Z"),
        assistant("a1", "2026-01-01T10:00:01Z", "low"),
    ];
    recording(&repo, &rows);
    let change = begin(&repo);
    rows.push(assistant("a2", "2026-01-01T10:00:02Z", "high"));
    recording(&repo, &rows);
    acting(&repo)
        .args(["comment", &change, "--body", "model switched"])
        .assert()
        .success();
    let events: Vec<Value> = stdout(acting(&repo).args(["events", "--change", &change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["model"], "m#low");
    let event = events.last().unwrap();
    assert_eq!(event["schema_version"], 7);
    assert_eq!(event["model"], "m#high");
    assert_eq!(event["model_source"], "resolved");
    assert_eq!(
        event["model_observation"]["timestamp"],
        "2026-01-01T10:00:02+00:00"
    );
    assert_eq!(event["model_observation"]["native_id"], "a2");
    assert_eq!(event["model_observation"]["turn_relation"], "inside");
    assert_eq!(event["model_observation"]["head_read"], true);
    let shown: Value =
        serde_json::from_str(&stdout(acting(&repo).args(["show", &change, "--json"]))).unwrap();
    assert_eq!(shown["schema"], "arc-state/3");
    assert_eq!(
        shown["model_attributions"][event["event_id"].as_str().unwrap()]["model_observation"],
        event["model_observation"]
    );
    let explained = stdout(acting(&repo).args(["log", &change]));
    assert!(explained.contains("source: resolved"), "{explained}");
    assert!(
        explained.contains("id a2 (inside the acting turn)"),
        "{explained}"
    );
    let config: Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 7);
}

#[test]
fn env_compares_selection_with_the_operator_prompt_and_ignores_tool_results() {
    let repo = Repo::new();
    let mut rows = vec![
        prompt("u1", "2026-01-01T10:00:00Z"),
        assistant("a1", "2026-01-01T10:00:01Z", "high"),
        prompt("u2", "2026-01-01T10:01:00Z"),
    ];
    recording(&repo, &rows);
    let earlier = stdout(acting(&repo).arg("env"));
    assert!(
        earlier.contains("from an earlier turn; effort may have changed since"),
        "{earlier}"
    );
    assert!(earlier.contains("10:00:01+00:00 id a1"), "{earlier}");
    rows.push(assistant("a2", "2026-01-01T10:01:01Z", "high"));
    rows.push(json!({"type":"user", "uuid":"tool-result", "timestamp":"2026-01-01T10:01:02Z", "message":{"role":"user", "content":[{"type":"tool_result", "tool_use_id":"tool", "content":"done"}]}}));
    recording(&repo, &rows);
    let inside = stdout(acting(&repo).arg("env"));
    assert!(
        inside.contains("id a2 (inside the acting turn)"),
        "{inside}"
    );
}

#[test]
fn declarations_keep_both_values_in_ledger_journal_and_replica_events() {
    let repo = Repo::new();
    recording(
        &repo,
        &[
            prompt("u1", "2026-01-01T10:00:00Z"),
            assistant("a1", "2026-01-01T10:00:01Z", "high"),
        ],
    );
    let change = begin(&repo);
    acting(&repo)
        .env("ARC_MODEL", "m#low")
        .args(["comment", &change, "--body", "declared"])
        .assert()
        .success();
    let events: Vec<Value> = stdout(acting(&repo).args(["events", "--change", &change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let declared = events.last().unwrap();
    assert_eq!(declared["model_source"], "env");
    assert_eq!(declared["model"], "m#low");
    assert_eq!(
        declared["model_disagreement"],
        json!({"declared":"m#low", "observed":"m#high"})
    );
    acting(&repo)
        .env("ARC_MODEL", "m#low")
        .args([
            "comment",
            &change,
            "--body",
            "flag wins",
            "--model",
            "m#high",
        ])
        .assert()
        .success();
    let events: Vec<Value> = stdout(acting(&repo).args(["events", "--change", &change]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["model_source"], "flag");
    assert_eq!(events.last().unwrap()["model"], "m#high");
    assert!(events.last().unwrap().get("model_disagreement").is_none());
    let explained = stdout(acting(&repo).args(["log", &change]));
    assert!(
        explained.contains("declared: m#low, observed: m#high"),
        "{explained}"
    );
    let body = repo.home.join("body.md");
    fs::write(&body, "fixture note").unwrap();
    acting(&repo)
        .env("ARC_MODEL", "m#low")
        .args([
            "journal",
            "note",
            "declared",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();
    acting(&repo)
        .args([
            "journal",
            "note",
            "resolved",
            "--body-file",
            body.to_str().unwrap(),
        ])
        .assert()
        .success();
    let journal: Vec<Value> = stdout(acting(&repo).args(["journal", "events"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let declared = journal
        .iter()
        .find(|event| event["topic"] == "declared")
        .unwrap();
    let resolved = journal
        .iter()
        .find(|event| event["topic"] == "resolved")
        .unwrap();
    assert_eq!(declared["schema"], "journal-events/1");
    assert_eq!(declared["model_source"], "env");
    assert_eq!(
        declared["model_disagreement"],
        json!({"declared":"m#low", "observed":"m#high"})
    );
    assert_eq!(resolved["model_source"], "resolved");
    assert_eq!(resolved["model_observation"]["native_id"], "a1");
    acting(&repo)
        .env("ARC_MODEL", "m#low")
        .args(["replica", "init", "fixture"])
        .assert()
        .success();
    let replica: Value = serde_json::from_str(&stdout(
        acting(&repo).args(["replica", "export", "--output", "-"]),
    ))
    .unwrap();
    assert_eq!(replica["schema"], "arc-replica-bundle/3");
    assert_eq!(replica["events"][0]["schema"], "arc-replica-event/3");
    assert_eq!(replica["events"][0]["model_source"], "env");
    assert_eq!(
        replica["events"][0]["model_disagreement"],
        json!({"declared":"m#low", "observed":"m#high"})
    );
}

#[test]
fn a_selection_outside_the_source_bound_is_not_borrowed_from_the_listing() {
    let repo = Repo::new();
    recording(
        &repo,
        &[
            prompt("u1", "2026-01-01T10:00:00Z"),
            assistant("a1", "2026-01-01T10:00:01Z", "high"),
            json!({"type":"progress", "padding":"x".repeat(5 * 1024 * 1024)}),
            json!({"type":"progress", "timestamp":"2026-01-01T10:00:02Z"}),
        ],
    );
    let env = stdout(acting(&repo).arg("env"));
    assert!(env.contains("unset ARC_MODEL"), "{env}");
    assert!(
        env.contains("recording head not read; an earlier model selection may be outside the read"),
        "{env}"
    );
}

#[test]
fn the_operator_inside_the_source_window_survives_a_long_turn() {
    let repo = Repo::new();
    let mut rows = vec![prompt("u1", "2026-01-01T10:00:00Z")];
    rows.extend((0..4200).map(|n| assistant(&format!("a{n}"), "2026-01-01T10:00:01Z", "high")));
    recording(&repo, &rows);
    let env = stdout(acting(&repo).arg("env"));
    assert!(env.contains("id a4199 (inside the acting turn)"), "{env}");
}

#[test]
fn a_boundary_outside_the_source_window_stays_unknown() {
    let repo = Repo::new();
    recording(
        &repo,
        &[
            prompt("u1", "2026-01-01T10:00:00Z"),
            json!({"type":"progress", "padding":"x".repeat(5 * 1024 * 1024)}),
            assistant("a2", "2026-01-01T10:00:02Z", "high"),
        ],
    );
    let env = stdout(acting(&repo).arg("env"));
    assert!(
        env.contains("acting turn boundary unavailable in the bounded read"),
        "{env}"
    );
    assert!(
        env.contains("recording head not read; earlier selections may be outside the read"),
        "{env}"
    );
}

#[test]
fn pi_live_selection_keeps_the_stores_observation_for_declaration_comparison() {
    let repo = Repo::new();
    let session = "019f0000-0000-7000-8000-000000000001";
    let sessions = repo.home.join("pi-sessions");
    fs::create_dir_all(&sessions).unwrap();
    let rows = [
        json!({"type":"session", "version":3, "id":session, "timestamp":"2026-01-01T10:00:00Z", "cwd":"/fixture"}),
        json!({"type":"thinking_level_change", "id":"thinking-1", "parentId":null, "timestamp":"2026-01-01T10:00:01Z", "thinkingLevel":"low"}),
        json!({"type":"message", "id":"assistant-1", "parentId":"thinking-1", "timestamp":"2026-01-01T10:00:02Z", "message":{"role":"assistant", "model":"m", "provider":"openai", "content":[{"type":"text", "text":"answer"}]}}),
    ];
    fs::write(
        sessions.join(format!("2026-01-01T10-00-00Z_{session}.jsonl")),
        rows.map(|row| format!("{row}\n")).concat(),
    )
    .unwrap();
    let mut cmd = repo.arc(&repo.root);
    cmd.env("ARC_HARNESS", "pi")
        .env("ARC_SESSION", session)
        .env("PI_SESSION_ID", session)
        .env("PI_MODEL", "m")
        .env("PI_REASONING_LEVEL", "high")
        .env("PI_CODING_AGENT_SESSION_DIR", &sessions)
        .env("ARC_MODEL", "m#high")
        .args(["begin", "pi-observed"])
        .assert()
        .success();
    let events: Vec<Value> = stdout(repo.arc(&repo.root).args(["events"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["model"], "m#high");
    assert_eq!(
        events[0]["model_disagreement"],
        json!({"declared":"m#high", "observed":"m#low"})
    );
    assert_eq!(events[0]["model_observation"]["native_id"], "assistant-1");
}

#[test]
fn reattribution_keeps_model_provenance_consistent_with_the_repaired_identity() {
    let repo = Repo::new();
    recording(
        &repo,
        &[
            prompt("u1", "2026-01-01T10:00:00Z"),
            assistant("a1", "2026-01-01T10:00:01Z", "high"),
        ],
    );
    let body = repo.home.join("body.md");
    fs::write(&body, "fixture note").unwrap();
    let path = stdout(acting(&repo).args([
        "journal",
        "note",
        "repair",
        "--body-file",
        body.to_str().unwrap(),
    ]));
    let filename = Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    acting(&repo)
        .args(["journal", "reattribute", filename, "--set-model", "m#low"])
        .assert()
        .success();
    let events: Vec<Value> = stdout(acting(&repo).args(["journal", "events"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event = events
        .iter()
        .find(|event| event["topic"] == "repair")
        .unwrap();
    assert_eq!(event["model_source"], "flag");
    assert_eq!(event["model_observation"]["native_id"], "a1");
    assert_eq!(
        event["model_disagreement"],
        json!({"declared":"m#low", "observed":"m#high"})
    );
    acting(&repo)
        .args([
            "journal",
            "reattribute",
            filename,
            "--set-session",
            "different-session",
        ])
        .assert()
        .success();
    let events: Vec<Value> = stdout(acting(&repo).args(["journal", "events"]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event = events
        .iter()
        .find(|event| event["topic"] == "repair")
        .unwrap();
    assert!(event.get("model_observation").is_none());
    assert!(event.get("model_disagreement").is_none());
}

#[test]
fn explain_is_an_unknown_command() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .arg("explain")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "unrecognized subcommand 'explain'",
        ));
}

#[test]
fn env_leaves_model_resolution_to_writes_after_shell_evaluation() {
    let repo = Repo::new();
    recording(
        &repo,
        &[
            prompt("u1", "2026-01-01T10:00:00Z"),
            assistant("a1", "2026-01-01T10:00:01Z", "high"),
        ],
    );
    let env = stdout(acting(&repo).env("ARC_MODEL", "stale-model").arg("env"));
    assert!(env.contains("# model: m#high\n# observed:"), "{env}");
    assert!(
        !env.lines()
            .any(|line| line.starts_with("export ") && line.contains("ARC_MODEL")),
        "{env}"
    );
    AssertCommand::new("sh")
        .args([
            "-c",
            "eval \"$1\"; test \"${ARC_MODEL+x}\" != x",
            "fixture",
            &env,
        ])
        .env("ARC_MODEL", "stale-model")
        .env("GIT_CONFIG_GLOBAL", repo.home.join(".gitconfig"))
        .env("XDG_CONFIG_HOME", repo.home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .assert()
        .success();
}
