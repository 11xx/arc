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

/// Regression tests covering contributor attribution, session-subject
/// normalization, verbatim label whitespace preservation, and first-patchset
/// approval after rework.
#[test]
fn stats_by_model_attribution_edges_and_post_approval_rework() {
    let repo = Repo::new();
    let uuid1 = "01a1d771-d401-7bd0-8b41-dd9ce2d9410a";
    let uuid2 = "01a1d771-d401-7bd0-8b41-dd9ce2d9410b";
    let uuid3 = "01a1d771-d401-7bd0-8b41-dd9ce2d9410c";
    let uuid4 = "01a1d771-d401-7bd0-8b41-dd9ce2d9410d";

    // 1. Unrelated bare-session contributor with --on-behalf-of codex:gpt-guess#high
    //    must keep the bare-session label and never credit codex:gpt-guess#high.
    let (_c1, wt1) = begin_with_worktree(&repo, "edge-unrelated-contrib", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt1, "a.txt", "1\n", "feat: a");
    let bare_session_1 = format!("codex:{uuid1}");
    repo.arc(&wt1)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "codex:gpt-guess#high",
            "snapshot",
            "--contributors",
            &bare_session_1,
        ])
        .assert()
        .success();

    // 2. Shared patchset with --contributors codex:<uuid2>,other,
    //    --on-behalf-of codex:<uuid2>, and --model gpt-flag#high must credit
    //    codex:gpt-flag#high for the named session and keep `other` as a label.
    let (_c2, wt2) = begin_with_worktree(&repo, "edge-shared-flag-model", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt2, "b.txt", "1\n", "feat: b");
    let bare_session_2 = format!("codex:{uuid2}");
    repo.arc(&wt2)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            &bare_session_2,
            "--model",
            "gpt-flag#high",
            "snapshot",
            "--contributors",
            &format!("{bare_session_2},other-shared"),
        ])
        .assert()
        .success();

    // 3. Shared patchset with --actor lead --harness claude
    //    --on-behalf-of codex:<uuid3>:gpt-test#high --contributors lead,other
    //    must NOT emit a bogus claude:gpt-test#high coordinate.
    let (_c3, wt3) = begin_with_worktree(&repo, "edge-delegated-recorder", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt3, "c.txt", "1\n", "feat: c");
    let delegated_subject = format!("codex:{uuid3}:gpt-test#high");
    repo.arc(&wt3)
        .args([
            "--actor",
            "lead",
            "--harness",
            "claude",
            "--on-behalf-of",
            &delegated_subject,
            "snapshot",
            "--contributors",
            "lead,other-delegated",
        ])
        .assert()
        .success();

    // 4. Session-form contributor `codex:<uuid4>:gpt-contributor#high` must
    //    normalize to `codex:gpt-contributor#high` without retaining the UUID.
    let (_c4, wt4) = begin_with_worktree(&repo, "edge-session-contrib", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt4, "d.txt", "1\n", "feat: d");
    let session_contrib = format!("codex:{uuid4}:gpt-contributor#high");
    repo.arc(&wt4)
        .args([
            "--actor",
            "lead",
            "snapshot",
            "--contributors",
            &session_contrib,
        ])
        .assert()
        .success();

    // 5. ps-01 approved, then changes requested on ps-01, then revised ps-02
    //    approved, then abandoned: must report rework_rounds_caused == 1 AND
    //    first_patchset_approval == 0/1 (NOT 1/1).
    let (c5, wt5) = begin_with_worktree(&repo, "edge-post-approval-rework", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", "codex:gpt-rework#high"]);
    });
    commit_file(&repo, &wt5, "e.txt", "v1\n", "feat: e v1");
    repo.arc(&wt5)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "codex:gpt-rework#high",
            "snapshot",
        ])
        .assert()
        .success();
    repo.arc(&wt5)
        .args([
            "--actor",
            "rev-a",
            "--on-behalf-of",
            "claude:claude-opus-5-5#high",
            "review",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&wt5)
        .args([
            "--actor",
            "rev-b",
            "--on-behalf-of",
            "claude:claude-opus-5-5#xhigh",
            "review",
            "--verdict",
            "changes-requested",
            "--cause",
            "executor",
        ])
        .assert()
        .success();
    commit_file(&repo, &wt5, "e.txt", "v2\n", "fix: e v2");
    repo.arc(&wt5)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "codex:gpt-rework#high",
            "snapshot",
        ])
        .assert()
        .success();
    repo.arc(&wt5)
        .args([
            "--actor",
            "rev-b",
            "--on-behalf-of",
            "claude:claude-opus-5-5#xhigh",
            "review",
            "--verdict",
            "approved",
        ])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            "codex:gpt-rework#high",
            "close",
            &c5,
            "--abandoned",
        ])
        .assert()
        .success();

    // 6. Whitespace-distinct labels `'  Label  '` and `'Label'` stay distinct
    //    and preserve verbatim spelling.
    let (_c6, wt6) = begin_with_worktree(&repo, "edge-whitespace-labels", |cmd| {
        cmd.args(["--actor", "lead", "--on-behalf-of", "  Label  "]);
    });
    commit_file(&repo, &wt6, "f.txt", "1\n", "feat: f");
    repo.arc(&wt6)
        .args(["--actor", "lead", "--on-behalf-of", "Label", "snapshot"])
        .assert()
        .success();

    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    let models = stats["models"].as_array().expect("models array");
    let has = |id: &str| models.iter().any(|row| row["identity"] == id);

    // Edge 1 assertions
    assert!(!has("codex:gpt-guess#high"), "{stats}");
    let row1 = find_row(&stats, &bare_session_1);
    assert_eq!(row1["identity_kind"], "label");
    assert_eq!(row1["patchsets"], 1);

    // Edge 2 assertions
    let row2 = find_row(&stats, "codex:gpt-flag#high");
    assert_eq!(row2["identity_kind"], "coordinate");
    assert_eq!(row2["patchsets"], 1);
    assert!(!has(&bare_session_2), "{stats}");
    let row2_other = find_row(&stats, "other-shared");
    assert_eq!(row2_other["identity_kind"], "label");
    assert_eq!(row2_other["patchsets"], 1);

    // Edge 3 assertions
    assert!(!has("claude:gpt-test#high"), "{stats}");
    let row3_lead = find_row(&stats, "lead");
    assert_eq!(row3_lead["identity_kind"], "label");
    assert_eq!(row3_lead["patchsets"], 1);
    let row3_other = find_row(&stats, "other-delegated");
    assert_eq!(row3_other["identity_kind"], "label");
    assert_eq!(row3_other["patchsets"], 1);

    // Edge 4 assertions
    assert!(!has(&session_contrib), "{stats}");
    let row4 = find_row(&stats, "codex:gpt-contributor#high");
    assert_eq!(row4["identity_kind"], "coordinate");
    assert_eq!(row4["patchsets"], 1);

    // Edge 5 assertions
    let row5 = find_row(&stats, "codex:gpt-rework#high");
    assert_eq!(row5["rework_rounds_caused"], 1);
    assert_eq!(row5["first_patchset_approval"]["count"], 0);
    assert_eq!(row5["first_patchset_approval"]["of"], 1);
    assert_eq!(row5["first_patchset_approval"]["rate"], 0.0);

    // Edge 6 assertions
    let row6_padded = find_row(&stats, "  Label  ");
    assert_eq!(row6_padded["identity_kind"], "label");
    assert_eq!(row6_padded["patchsets"], 0);
    let row6_plain = find_row(&stats, "Label");
    assert_eq!(row6_plain["identity_kind"], "label");
    assert_eq!(row6_plain["patchsets"], 1);
}

/// Regression test for clean rebase replay: when `ps-01` was recorded with a
/// delegated session subject and shared contributors including the recorder,
/// advancing `master` and running `arc rebase` must preserve the source
/// attribution on `lead` without emitting a phantom `<recorder-harness>:<delegated-model>`
/// coordinate.
#[test]
fn stats_by_model_clean_rebase_preserves_source_attribution_without_phantom_coordinate() {
    let repo = Repo::new();
    let uuid = "01a1d771-d401-7bd0-8b41-dd9ce2d9410e";
    let delegated = format!("codex:{uuid}:gpt-carry#high");

    let (change_id, wt) = begin_with_worktree(&repo, "rebase-carry", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt, "feature.txt", "v1\n", "feat: feature");
    repo.arc(&wt)
        .args([
            "--actor",
            "lead",
            "--harness",
            "claude",
            "--on-behalf-of",
            &delegated,
            "snapshot",
            "--contributors",
            "lead,helper",
        ])
        .assert()
        .success();

    // Advance master with a disjoint file, then run a clean `arc rebase`.
    commit_file(&repo, &repo.root, "upstream.txt", "u1\n", "chore: upstream");
    repo.arc(&wt)
        .args([
            "--actor",
            "lead",
            "--harness",
            "claude",
            "rebase",
            &change_id,
        ])
        .assert()
        .success();

    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    let models = stats["models"].as_array().expect("models array");
    let has = |id: &str| models.iter().any(|row| row["identity"] == id);

    assert!(!has("claude:gpt-carry#high"), "{stats}");
    let lead = find_row(&stats, "lead");
    assert_eq!(lead["identity_kind"], "label");
    assert_eq!(lead["changes"], 1);
    assert_eq!(lead["patchsets"], 2);
    let helper = find_row(&stats, "helper");
    assert_eq!(helper["identity_kind"], "label");
    assert_eq!(helper["changes"], 1);
    assert_eq!(helper["patchsets"], 2);
}

/// Regression test for two-snapshot attribution: when `ps-01` establishes a
/// model for an exact `harness:<uuid>` contributor and `ps-02` names the same
/// bare `harness:<uuid>` contributor without `--on-behalf-of` or `--model`,
/// stats must reuse the established coordinate instead of splitting the change
/// across the coordinate and the bare-session label.
#[test]
fn stats_by_model_later_snapshot_reuses_exact_session_coordinate() {
    let repo = Repo::new();
    let uuid = "01a1d771-d401-7bd0-8b41-dd9ce2d9410f";
    let bare = format!("codex:{uuid}");
    let delegated = format!("codex:{uuid}:gpt-repeat#high");
    let contributors = format!("{bare},helper");

    let (_change_id, wt) = begin_with_worktree(&repo, "repeat-session", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt, "work.txt", "v1\n", "feat: v1");
    repo.arc(&wt)
        .args([
            "--actor",
            "lead",
            "--on-behalf-of",
            &delegated,
            "snapshot",
            "--contributors",
            &contributors,
        ])
        .assert()
        .success();

    commit_file(&repo, &wt, "work.txt", "v2\n", "feat: v2");
    repo.arc(&wt)
        .args([
            "--actor",
            "lead",
            "snapshot",
            "--contributors",
            &contributors,
        ])
        .assert()
        .success();

    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    let models = stats["models"].as_array().expect("models array");
    let has = |id: &str| models.iter().any(|row| row["identity"] == id);

    assert!(!has(&bare), "{stats}");
    let coord = find_row(&stats, "codex:gpt-repeat#high");
    assert_eq!(coord["identity_kind"], "coordinate");
    assert_eq!(coord["changes"], 1);
    assert_eq!(coord["patchsets"], 2);
    let helper = find_row(&stats, "helper");
    assert_eq!(helper["identity_kind"], "label");
    assert_eq!(helper["changes"], 1);
    assert_eq!(helper["patchsets"], 2);
}

/// Regression test for same-session alias lookup: explicit contributor models
/// sharing one session UUID must both be credited, and a bare session paired
/// with an explicit session-qualified model must resolve to that coordinate
/// rather than emitting raw labels.
#[test]
fn stats_by_model_same_session_contributors_preserve_explicit_models() {
    let repo = Repo::new();
    let uuid1 = "01a1d771-d401-7bd0-8b41-dd9ce2d94110";
    let uuid2 = "01a1d771-d401-7bd0-8b41-dd9ce2d94111";
    let probe1 = format!("codex:{uuid1}:gpt-probe#high");
    let newer1 = format!("codex:{uuid1}:gpt-new#medium");
    let bare2 = format!("codex:{uuid2}");
    let probe2 = format!("codex:{uuid2}:gpt-pair#high");

    // Case 1: two explicit session-qualified models on one snapshot.
    let (_c1, wt1) = begin_with_worktree(&repo, "same-session-two-models", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt1, "a.txt", "1\n", "feat: a");
    repo.arc(&wt1)
        .args([
            "--actor",
            "lead",
            "--harness",
            "claude",
            "snapshot",
            "--contributors",
            &format!("{probe1},{newer1}"),
        ])
        .assert()
        .success();

    // Case 2: bare session followed by explicit session-qualified model.
    let (_c2, wt2) = begin_with_worktree(&repo, "same-session-bare-and-explicit", |cmd| {
        cmd.args(["--actor", "lead"]);
    });
    commit_file(&repo, &wt2, "b.txt", "1\n", "feat: b");
    repo.arc(&wt2)
        .args([
            "--actor",
            "lead",
            "--harness",
            "claude",
            "snapshot",
            "--contributors",
            &format!("{bare2},{probe2}"),
        ])
        .assert()
        .success();

    let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));
    let models = stats["models"].as_array().expect("models array");
    let has = |id: &str| models.iter().any(|row| row["identity"] == id);

    let r_probe = find_row(&stats, "codex:gpt-probe#high");
    assert_eq!(r_probe["identity_kind"], "coordinate");
    assert_eq!(r_probe["patchsets"], 1);
    let r_new = find_row(&stats, "codex:gpt-new#medium");
    assert_eq!(r_new["identity_kind"], "coordinate");
    assert_eq!(r_new["patchsets"], 1);
    assert!(!has(&probe1), "{stats}");
    assert!(!has(&newer1), "{stats}");

    let r_pair = find_row(&stats, "codex:gpt-pair#high");
    assert_eq!(r_pair["identity_kind"], "coordinate");
    assert_eq!(r_pair["patchsets"], 1);
    assert!(!has(&bare2), "{stats}");
    assert!(!has(&probe2), "{stats}");
}

/// Regression test for multi-model session ambiguity: when `ps-01` establishes
/// two distinct models for one session UUID and `ps-02` names only the bare
/// session UUID, `ps-02` must remain a bare-session label rather than
/// arbitrarily inheriting the lexically last model (regardless of contributor
/// argument order).
#[test]
fn stats_by_model_bare_session_stays_label_after_multi_model_snapshot() {
    let uuid = "01a1d771-d401-7bd0-8b41-dd9ce2d94199";
    let bare = format!("codex:{uuid}");
    let alpha = format!("codex:{uuid}:gpt-alpha#high");
    let beta = format!("codex:{uuid}:gpt-beta#medium");

    for contributors_arg in [format!("{alpha},{beta}"), format!("{beta},{alpha}")] {
        let repo = Repo::new();
        let (_change_id, wt) = begin_with_worktree(&repo, "ambiguous-session", |cmd| {
            cmd.args(["--actor", "lead"]);
        });
        commit_file(&repo, &wt, "work.txt", "v1\n", "feat: v1");
        repo.arc(&wt)
            .args([
                "--actor",
                "lead",
                "--on-behalf-of",
                "observer",
                "snapshot",
                "--contributors",
                &contributors_arg,
            ])
            .assert()
            .success();

        commit_file(&repo, &wt, "work.txt", "v2\n", "feat: v2");
        repo.arc(&wt)
            .args([
                "--actor",
                "lead",
                "--on-behalf-of",
                "observer",
                "snapshot",
                "--contributors",
                &bare,
            ])
            .assert()
            .success();

        let stats = json_stdout(repo.arc(&repo.root).args(["stats", "--by-model", "--json"]));

        let r_alpha = find_row(&stats, "codex:gpt-alpha#high");
        assert_eq!(r_alpha["identity_kind"], "coordinate", "{contributors_arg}");
        assert_eq!(r_alpha["patchsets"], 1, "{contributors_arg}");

        let r_beta = find_row(&stats, "codex:gpt-beta#medium");
        assert_eq!(r_beta["identity_kind"], "coordinate", "{contributors_arg}");
        assert_eq!(r_beta["patchsets"], 1, "{contributors_arg}");

        let r_bare = find_row(&stats, &bare);
        assert_eq!(r_bare["identity_kind"], "label", "{contributors_arg}");
        assert_eq!(r_bare["patchsets"], 1, "{contributors_arg}");
    }
}
