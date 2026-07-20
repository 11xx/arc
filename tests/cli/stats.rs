use crate::common::*;

#[test]
fn stats_json_carries_schema_and_reports_selected_change() {
    let repo = Repo::new();
    let (_id, wt, _head) = change_with_patchset(&repo, "feat-x");
    repo.arc(&wt)
        .args(["review", "feat-x", "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", "feat-x"])
        .assert()
        .success();

    let report = json_stdout(repo.arc(&repo.root).args(["stats", "--all", "--json"]));
    assert_eq!(report["schema"], "arc-stats/1");

    let changes = report["changes"].as_array().unwrap();
    let feat = changes
        .iter()
        .find(|change| change["slug"] == "feat-x")
        .expect("completed change should appear in stats");
    assert_eq!(feat["state"], "closed");
    // An integrated change has a measured open→integrated wall time.
    assert!(feat["wall_time_seconds"].is_number());
    assert_eq!(feat["patchset_count"], 1);
    assert!(report["aggregate"]["changes"].as_u64().unwrap() >= 1);
}
