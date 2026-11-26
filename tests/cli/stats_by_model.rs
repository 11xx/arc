use super::common::*;

fn find_row<'a>(output: &'a serde_json::Value, identity: &str) -> &'a serde_json::Value {
    output["models"]
        .as_array()
        .expect("models array")
        .iter()
        .find(|row| row["identity"] == identity)
        .unwrap_or_else(|| panic!("missing row for {identity:?} in {output}"))
}

/// Claim: `arc stats --by-model` groups rows by canonical `harness:model#effort`
/// coordinate across direct and delegated records, keeps label-only rows
/// separate without guessing, computes land rate strictly over `integrated` and
/// `abandoned` (excluding `open`, `closed`, and `superseded`), computes
/// first-patchset approval over closed-out changes only, and exposes all counts
/// in `arc-stats-by-model/2` JSON and text.
#[test]
fn stats_by_model_groups_coordinates_separates_labels_and_computes_rates() {
    let repo = Repo::new();
    let sol = "codex:gpt-6.1-sol#xhigh";
    let opus = "claude:claude-opus-5-5#high";
    let session_reviewer = "claude:9f6db7f4-103d-48ef-be2a-c9948ff02c46:claude-opus-5-5#high";

    // Change 1: integrated, ps-01 approved on first pass.
    // Recorded directly with --harness/--model (not --on-behalf-of) to verify
    // canonical grouping with delegated records of the same coordinate.
    let (c1, wt1) = begin_with_worktree(&repo, "land-first-pass", |cmd| {
        cmd.args([
            "--actor",
            "sol-exec",
            "--harness",
            "codex",
            "--model",
            "gpt-6.1-sol#xhigh",
        ]);
    });
    commit_file(&repo, &wt1, "c1.txt", "v1\n", "feat: c1");
    repo.arc(&wt1)
        .args([
            "--actor",
            "sol-exec",
            "--harness",
            "codex",
            "--model",
            "gpt-6.1-sol#xhigh",
            "snapshot",
            "--solo",
        ])
        .assert()
        .success();
    repo.arc(&wt1)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            session_reviewer,
            "review",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "--actor",
            "sol-exec",
            "--harness",
            "codex",
            "--model",
            "gpt-6.1-sol#xhigh",
            "integrate",
            &c1,
        ])
        .assert()
        .success();

    // Change 2: integrated after rework (ps-01 changes-requested, ps-02 approved).
    // Uses --on-behalf-of codex:gpt-6.1-sol#xhigh so it must group with c1.
    // Reviewer uses a free-form label (`codex-sol`) that must stay separate.
    let (c2, wt2) = begin_with_worktree(&repo, "land-after-rework", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", sol]);
    });
    commit_file(&repo, &wt2, "c2.txt", "v1\n", "feat: c2 v1");
    repo.arc(&wt2)
        .args(["--actor", "lead", "--on-behalf-of", sol, "snapshot"])
        .assert()
        .success();
    repo.arc(&wt2)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "codex-sol",
            "review",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    commit_file(&repo, &wt2, "c2.txt", "v2\n", "fix: c2 v2");
    repo.arc(&wt2)
        .args(["--actor", "lead", "--on-behalf-of", sol, "snapshot"])
        .assert()
        .success();
    repo.arc(&wt2)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "codex-sol",
            "review",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["--actor", "lead", "--on-behalf-of", sol, "integrate", &c2])
        .assert()
        .success();

    // Change 3: abandoned after review requested changes.
    let (c3, wt3) = begin_with_worktree(&repo, "abandoned-work", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", sol]);
    });
    commit_file(&repo, &wt3, "c3.txt", "v1\n", "feat: c3");
    repo.arc(&wt3)
        .args(["--actor", "lead", "--on-behalf-of", sol, "snapshot"])
        .assert()
        .success();
    repo.arc(&wt3)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "Codex Sol Reviewer",
            "review",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            sol,
            "close",
            &c3,
            "--abandoned",
        ])
        .assert()
        .success();

    // Change 4: closed via --assert-integrated (no arc merge), ps-01 approved.
    // Must stay out of land_rate denominator, and count in first_patchset_approval.
    let (c4, wt4) = begin_with_worktree(&repo, "asserted-close", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", sol]);
    });
    commit_file(&repo, &wt4, "c4.txt", "v1\n", "feat: c4");
    let c4_head = git_out(&wt4, &["rev-parse", "HEAD"]);
    repo.arc(&wt4)
        .args(["--actor", "lead", "--on-behalf-of", sol, "snapshot"])
        .assert()
        .success();
    repo.arc(&wt4)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            opus,
            "review",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    git_out(
        &repo.root,
        &["merge", "--no-ff", "-m", "external merge", &c4_head],
    );
    let merged_rev = git_out(&repo.root, &["rev-parse", "HEAD"]);
    repo.arc(&repo.root)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            sol,
            "close",
            &c4,
            "--assert-integrated",
            &merged_rev,
        ])
        .assert()
        .success();

    // Change 5: superseded by c1. Must stay out of land_rate denominator.
    let (c5, wt5) = begin_with_worktree(&repo, "superseded-work", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", sol]);
    });
    commit_file(&repo, &wt5, "c5.txt", "v1\n", "feat: c5");
    repo.arc(&wt5)
        .args(["--actor", "lead", "--on-behalf-of", sol, "snapshot"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            sol,
            "close",
            &c5,
            "--superseded",
            &c1,
        ])
        .assert()
        .success();

    // Change 6: still open, with ps-01 approved. Excluded from both rates.
    let (_c6, wt6) = begin_with_worktree(&repo, "open-work", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", sol]);
    });
    commit_file(&repo, &wt6, "c6.txt", "v1\n", "feat: c6");
    repo.arc(&wt6)
        .args(["--actor", "lead", "--on-behalf-of", sol, "snapshot"])
        .assert()
        .success();
    repo.arc(&wt6)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "GPT-5.6 Sol",
            "review",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();

    // Verify JSON output (`arc-stats-by-model/2`).
    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    assert_eq!(stats["schema"], "arc-stats-by-model/2", "{stats}");

    let sol_row = find_row(&stats, sol);
    assert_eq!(sol_row["identity_kind"], "coordinate");
    assert_eq!(sol_row["changes"], 6);
    assert_eq!(
        sol_row["changes_by_state"],
        serde_json::json!({
            "integrated": 2,
            "abandoned": 1,
            "closed": 1,
            "superseded": 1,
            "open": 1,
        })
    );
    assert_eq!(sol_row["land_rate"]["count"], 2);
    assert_eq!(sol_row["land_rate"]["of"], 3);
    assert!((sol_row["land_rate"]["rate"].as_f64().unwrap() - (2.0 / 3.0)).abs() < 1e-12);
    assert_eq!(sol_row["first_patchset_approval"]["count"], 2);
    assert_eq!(sol_row["first_patchset_approval"]["of"], 5);
    assert!((sol_row["first_patchset_approval"]["rate"].as_f64().unwrap() - 0.4).abs() < 1e-12);
    assert_eq!(sol_row["patchsets"], 7);
    assert_eq!(sol_row["rework_rounds_caused"], 1);
    assert_eq!(sol_row["verdicts"], 0);

    // Session-subject reviewer (`claude:<uuid>:claude-opus-5-5#high`) merged
    // with direct coordinate `claude:claude-opus-5-5#high`.
    let opus_row = find_row(&stats, opus);
    assert_eq!(opus_row["identity_kind"], "coordinate");
    assert_eq!(opus_row["changes"], 2);
    assert_eq!(
        opus_row["changes_by_state"],
        serde_json::json!({
            "integrated": 1,
            "abandoned": 0,
            "closed": 1,
            "superseded": 0,
            "open": 0,
        })
    );
    assert_eq!(opus_row["land_rate"]["count"], 1);
    assert_eq!(opus_row["land_rate"]["of"], 1);
    assert_eq!(opus_row["land_rate"]["rate"], 1.0);
    assert_eq!(opus_row["first_patchset_approval"]["count"], 2);
    assert_eq!(opus_row["first_patchset_approval"]["of"], 2);
    assert_eq!(opus_row["first_patchset_approval"]["rate"], 1.0);
    assert_eq!(opus_row["verdicts"], 2);

    // Label-only rows stay separate and are never merged into `codex:gpt-6.1-sol#xhigh`.
    let label_rework = find_row(&stats, "codex-sol");
    assert_eq!(label_rework["identity_kind"], "label");
    assert_eq!(label_rework["changes"], 1);
    assert_eq!(label_rework["changes_by_state"]["integrated"], 1);
    assert_eq!(label_rework["verdicts"], 2);

    let label_abandoned = find_row(&stats, "Codex Sol Reviewer");
    assert_eq!(label_abandoned["identity_kind"], "label");
    assert_eq!(label_abandoned["changes_by_state"]["abandoned"], 1);
    assert_eq!(label_abandoned["land_rate"]["count"], 0);
    assert_eq!(label_abandoned["land_rate"]["of"], 1);
    assert_eq!(label_abandoned["land_rate"]["rate"], 0.0);

    // Open-only label has 0/0 denominator -> null rates.
    let label_open = find_row(&stats, "GPT-5.6 Sol");
    assert_eq!(label_open["identity_kind"], "label");
    assert_eq!(label_open["changes_by_state"]["open"], 1);
    assert_eq!(label_open["land_rate"]["count"], 0);
    assert_eq!(label_open["land_rate"]["of"], 0);
    assert!(label_open["land_rate"]["rate"].is_null());
    assert_eq!(label_open["first_patchset_approval"]["count"], 0);
    assert_eq!(label_open["first_patchset_approval"]["of"], 0);
    assert!(label_open["first_patchset_approval"]["rate"].is_null());

    // Verify text output shows rates with counts beside them and n/a when denominator is 0.
    let text = stdout(repo.arc(&repo.root).args(["stats", "--by-model"]));
    assert!(text.contains("land-rate"), "{text}");
    assert!(text.contains("first-pass"), "{text}");
    assert!(text.contains("66.7% (2/3)"), "{text}");
    assert!(text.contains("40.0% (2/5)"), "{text}");
    assert!(text.contains("n/a (0/0)"), "{text}");
    assert!(text.contains("codex-sol"), "{text}");
    assert!(text.contains("Codex Sol Reviewer"), "{text}");
    assert!(text.contains("GPT-5.6 Sol"), "{text}");
    let _ = c2;
}

fn begin_with_worktree(
    repo: &Repo,
    slug: &str,
    configure: impl FnOnce(&mut AssertCommand),
) -> (String, PathBuf) {
    let mut cmd = repo.arc(&repo.root);
    configure(&mut cmd);
    let out = stdout(cmd.args(["begin", slug]));
    let change_id = out
        .lines()
        .find_map(|line| line.strip_prefix("change: "))
        .expect("change line")
        .trim()
        .to_string();
    let worktree = out
        .lines()
        .find_map(|line| line.strip_prefix("worktree: "))
        .expect("worktree line")
        .trim()
        .to_string();
    (change_id, PathBuf::from(worktree))
}

fn commit_file(repo: &Repo, dir: &Path, name: &str, content: &str, msg: &str) {
    let _ = repo;
    fs::write(dir.join(name), content).unwrap();
    git_out(dir, &["add", name]);
    git_out(dir, &["commit", "-m", msg]);
}
