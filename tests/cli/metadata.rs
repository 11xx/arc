use super::common::*;

#[test]
fn reading_empty_metadata_prints_empty_fields() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "empty-metadata", "--no-worktree"]),
    );

    let output = stdout(repo.arc(&repo.root).args(["metadata", "empty-metadata"]));
    assert!(output.contains("change: "));
    assert!(output.contains("blocked-by: \n"));
    assert!(output.contains("tags: \n"));
    assert!(output.contains("assigned-to: \n"));
    assert!(output.contains("priority: 0\n"));
}

#[test]
fn populated_metadata_reads_back_as_text() {
    let repo = Repo::new();
    let dependency = stdout(
        repo.arc(&repo.root)
            .args(["begin", "dependency", "--no-worktree"]),
    );
    let dependency_id = opened_change_id(&dependency);
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "populated", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args([
            "metadata",
            "populated",
            "--blocked-by",
            "dependency",
            "--tag",
            "alpha",
            "--tag",
            "beta",
            "--assign",
            "codex",
            "--priority",
            "7",
        ])
        .assert()
        .success();

    let output = stdout(repo.arc(&repo.root).args(["metadata", "populated"]));
    assert!(output.contains(&format!("blocked-by: {dependency_id}")));
    assert!(output.contains("tags: alpha, beta"));
    assert!(output.contains("assigned-to: codex"));
    assert!(output.contains("priority: 7"));
}

#[test]
fn json_metadata_has_exact_schema_and_values() {
    let repo = Repo::new();
    let dependency = stdout(
        repo.arc(&repo.root)
            .args(["begin", "json-dep", "--no-worktree"]),
    );
    let dependency_id = opened_change_id(&dependency);
    let target = stdout(
        repo.arc(&repo.root)
            .args(["begin", "json-target", "--no-worktree"]),
    );
    let target_id = opened_change_id(&target);
    repo.arc(&repo.root)
        .args([
            "metadata",
            "json-target",
            "--blocked-by",
            "json-dep",
            "--tag",
            "json",
            "--assign",
            "codex",
            "--priority",
            "9",
        ])
        .assert()
        .success();

    let value = json_stdout(
        repo.arc(&repo.root)
            .args(["metadata", "json-target", "--json"]),
    );
    let keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys.len(),
        6,
        "metadata JSON should contain exactly the six schema fields"
    );
    assert_eq!(value["schema"], "arc-metadata/1");
    assert_eq!(value["change_id"], target_id);
    assert_eq!(value["blocked_by"], serde_json::json!([dependency_id]));
    assert_eq!(value["tags"], serde_json::json!(["json"]));
    assert_eq!(value["assigned_to"], "codex");
    assert_eq!(value["priority"], 9);
}

#[test]
fn removals_and_assignment_clear_are_reflected_in_reads() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "removals", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args([
            "metadata", "removals", "--tag", "keep", "--tag", "remove", "--assign", "codex",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "metadata",
            "removals",
            "--remove-tag",
            "remove",
            "--assign",
            "",
        ])
        .assert()
        .success();

    let value = json_stdout(
        repo.arc(&repo.root)
            .args(["metadata", "removals", "--json"]),
    );
    assert_eq!(value["tags"], serde_json::json!(["keep"]));
    assert_eq!(value["assigned_to"], serde_json::Value::Null);
}

#[test]
fn json_cannot_be_combined_with_mutation_flags() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "mixed-mode", "--no-worktree"]),
    );

    repo.arc(&repo.root)
        .args(["metadata", "mixed-mode", "--json", "--tag", "invalid"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "--json cannot be combined with metadata mutation flags",
        ));
}

#[test]
fn closed_change_metadata_remains_readable() {
    let repo = Repo::new();
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "closed-metadata", "--no-worktree"]),
    );
    repo.arc(&repo.root)
        .args(["close", "closed-metadata", "--abandoned"])
        .assert()
        .success();

    repo.arc(&repo.root)
        .args(["metadata", "closed-metadata"])
        .assert()
        .success()
        .stdout(predicates::str::contains("change: "));
}
