use super::common::*;

fn begin(repo: &Repo, slug: &str, extra: &[&str]) -> String {
    let mut args = vec!["begin", slug, "--no-worktree"];
    args.extend_from_slice(extra);
    opened_change_id(&stdout(repo.arc(&repo.root).args(args)))
}

fn chain_json(repo: &Repo, tag: &str) -> serde_json::Value {
    serde_json::from_str(&stdout(repo.arc(&repo.root).args(["chain", tag, "--json"]))).unwrap()
}

fn plan(repo: &Repo, topic: &str) -> String {
    let path = stdout(
        repo.arc(&repo.root)
            .args([
                "journal",
                "note",
                topic,
                "--kind",
                "plan",
                "--body-file",
                "-",
            ])
            .write_stdin("# Plan\n"),
    );
    Path::new(path.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn chain_lists_each_tagged_member_once_and_excludes_untagged_changes() {
    let repo = Repo::new();
    begin(&repo, "chain-one", &["--tag", "program"]);
    begin(&repo, "chain-two", &["--tag", "program"]);
    begin(&repo, "chain-outside", &[]);

    let output = chain_json(&repo, "program");
    let members = output["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(
        members
            .iter()
            .filter(|member| member["slug"] == "chain-one")
            .count(),
        1
    );
    assert_eq!(
        members
            .iter()
            .filter(|member| member["slug"] == "chain-two")
            .count(),
        1
    );
    assert!(!members
        .iter()
        .any(|member| member["slug"] == "chain-outside"));
}

#[test]
fn chain_includes_closed_members() {
    let repo = Repo::new();
    begin(&repo, "chain-closed", &["--tag", "program"]);
    repo.arc(&repo.root)
        .args(["close", "chain-closed", "--abandoned"])
        .assert()
        .success();

    let output = chain_json(&repo, "program");
    assert_eq!(output["members"][0]["slug"], "chain-closed");
    assert_eq!(output["members"][0]["state"], "closed");
}

#[test]
fn chain_orders_blockers_before_dependents() {
    let repo = Repo::new();
    let blocker = begin(&repo, "chain-blocker", &["--tag", "program"]);
    begin(
        &repo,
        "chain-dependent",
        &["--tag", "program", "--blocked-by", &blocker],
    );

    let output = chain_json(&repo, "program");
    let slugs = output["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["slug"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(slugs, ["chain-blocker", "chain-dependent"]);
}

#[test]
fn chain_keeps_plan_history_and_marks_the_newest_current() {
    let repo = Repo::new();
    let first = plan(&repo, "chain-plan-one");
    let second = plan(&repo, "chain-plan-two");
    begin(
        &repo,
        "chain-plan-first",
        &["--tag", "program", "--from-journal", &first],
    );
    begin(
        &repo,
        "chain-plan-second",
        &["--tag", "program", "--from-journal", &second],
    );

    let output = chain_json(&repo, "program");
    assert_eq!(output["plans"][0]["plan_ref"], first);
    assert_eq!(output["plans"][0]["current"], false);
    assert_eq!(output["plans"][1]["plan_ref"], second);
    assert_eq!(output["plans"][1]["current"], true);
}

#[test]
fn chain_json_has_versioned_schema_member_state_and_no_stored_aggregate_state() {
    let repo = Repo::new();
    begin(&repo, "chain-shape", &["--tag", "program"]);

    let output = chain_json(&repo, "program");
    assert_eq!(output["schema"], "arc-chain/1");
    assert_eq!(output["members"][0]["state"], "open");
    assert!(output.get("complete").is_none());
    assert!(output.get("paused").is_none());
}

#[test]
fn chain_unknown_tag_is_an_empty_view() {
    let repo = Repo::new();
    begin(&repo, "chain-known", &["--tag", "known"]);

    let output = chain_json(&repo, "unknown");
    assert_eq!(output["members"].as_array().unwrap().len(), 0);
    assert!(output["next_ready"].is_null());
}
