use super::common::*;

fn begin(repo: &Repo, slug: &str, extra: &[&str]) -> String {
    let mut args = vec!["begin", slug, "--no-worktree"];
    args.extend_from_slice(extra);
    opened_change_id(&stdout(repo.arc(&repo.root).args(args)))
}

fn chain_json(repo: &Repo, tag: &str) -> serde_json::Value {
    serde_json::from_str(&stdout(repo.arc(&repo.root).args(["chain", tag, "--json"]))).unwrap()
}

fn chain_review_json(repo: &Repo, tag: &str) -> serde_json::Value {
    serde_json::from_str(&stdout(
        repo.arc(&repo.root)
            .args(["chain", tag, "--review", "--json"]),
    ))
    .unwrap()
}

fn tagged_patchset(repo: &Repo, slug: &str) -> PathBuf {
    stdout(
        repo.arc(&repo.root)
            .args(["begin", slug, "--tag", "program"]),
    );
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    repo.commit(
        &worktree,
        &format!("{slug}.txt"),
        "reviewed\n",
        &format!("test: add {slug}"),
    );
    worktree
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
    assert_eq!(output["schema"], "arc-chain/3");
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

#[test]
fn chain_review_is_opt_in_and_keeps_the_existing_schema() {
    let repo = Repo::new();
    begin(&repo, "review-opt-in", &["--tag", "program"]);

    let without = chain_json(&repo, "program");
    let with = chain_review_json(&repo, "program");
    assert!(without["members"][0].get("review").is_none());
    assert!(with["members"][0].get("review").is_some());
}

#[test]
fn chain_review_reports_verdict_identities_without_inferring_independence() {
    let repo = Repo::new();
    let self_worktree = tagged_patchset(&repo, "review-self");
    stdout(
        repo.arc(&self_worktree)
            .env("ARC_ACTOR", "Alice")
            .args(["snapshot", "review-self"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Alice").args([
        "review",
        "review-self",
        "--verdict",
        "approved",
    ]));

    let other_worktree = tagged_patchset(&repo, "review-other");
    stdout(
        repo.arc(&other_worktree)
            .env("ARC_ACTOR", "Bob")
            .args(["snapshot", "review-other"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Carol").args([
        "review",
        "review-other",
        "--verdict",
        "approved",
    ]));

    let output = chain_review_json(&repo, "program");
    let self_review = output["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["slug"] == "review-self")
        .unwrap();
    let other_review = output["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["slug"] == "review-other")
        .unwrap();
    assert_eq!(self_review["review"]["subject"], "Alice");
    assert_eq!(
        self_review["review"]["lifetime"]["identities"],
        serde_json::json!(["Alice"])
    );
    // No brief was recorded, so there is no author to attribute a verdict to
    // and arc says so rather than inferring independence from identities.
    assert!(self_review["review"]["brief_author"].is_null());
    assert!(self_review["review"]["reviewed_only_by_brief_author"].is_null());
    assert_eq!(other_review["review"]["subject"], "Bob");
    assert_eq!(
        other_review["review"]["lifetime"]["identities"],
        serde_json::json!(["Carol"])
    );
    assert!(other_review["review"]["reviewed_only_by_brief_author"].is_null());
}

/// Identity inequality is not independence. In an orchestrated chain the
/// patchset subject is the executor and the verdict comes from the lead, so
/// the identities always differ and a boolean built on that comparison reports
/// independent review for work the lead directed and then approved. The map
/// reports the fact the ledger holds — who wrote the brief, and whether only
/// that identity recorded a verdict — and infers nothing further.
#[test]
fn chain_review_reports_brief_author_only_review_without_inferring_independence() {
    let repo = Repo::new();

    // The lead briefs the work, an executor implements it, and the lead
    // approves. Every identity differs from the patchset subject.
    let led = tagged_patchset(&repo, "review-led");
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "brief",
        "review-led",
        "--body-file",
        "-",
    ]));
    stdout(
        repo.arc(&led)
            .env("ARC_ACTOR", "Executor")
            .args(["snapshot", "review-led"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "review",
        "review-led",
        "--verdict",
        "approved",
    ]));

    // The same shape, except somebody other than the brief's author also
    // recorded a verdict.
    let panel = tagged_patchset(&repo, "review-panel");
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "brief",
        "review-panel",
        "--body-file",
        "-",
    ]));
    stdout(
        repo.arc(&panel)
            .env("ARC_ACTOR", "Executor")
            .args(["snapshot", "review-panel"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Reviewer").args([
        "review",
        "review-panel",
        "--verdict",
        "approved",
    ]));

    let output = chain_review_json(&repo, "program");
    let member = |slug: &str| {
        output["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|member| member["slug"] == slug)
            .unwrap()
            .clone()
    };

    let led = member("review-led");
    assert_eq!(led["review"]["subject"], "Executor", "{led}");
    assert_eq!(led["review"]["brief_author"], "Lead", "{led}");
    assert_eq!(
        led["review"]["reviewed_only_by_brief_author"], true,
        "{led}"
    );

    let panel = member("review-panel");
    assert_eq!(panel["review"]["brief_author"], "Lead", "{panel}");
    assert_eq!(
        panel["review"]["reviewed_only_by_brief_author"], false,
        "{panel}"
    );

    // The removed boolean is not merely renamed: nothing in the map answers
    // whether review was independent, because the ledger cannot know it.
    assert!(led["review"].get("non_self_verdict").is_none(), "{led}");
    assert!(panel["review"].get("non_self_verdict").is_none(), "{panel}");
}

/// Two ways the attribution can be wrong: reading the newest brief rather than
/// the one the patchset was built from, and reading a brief's author more
/// literally than a verdict's. A lead acting for an executor is that executor
/// on both sides, or the comparison compares different things.
#[test]
fn brief_authorship_follows_the_patchset_and_the_delegated_subject() {
    let repo = Repo::new();

    // The work is briefed, snapshotted, and reviewed. Only afterwards does a
    // second brief version land, from a different author.
    let rebriefed = tagged_patchset(&repo, "review-rebriefed");
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "brief",
        "review-rebriefed",
        "--body-file",
        "-",
    ]));
    stdout(
        repo.arc(&rebriefed)
            .env("ARC_ACTOR", "Executor")
            .args(["snapshot", "review-rebriefed"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "review",
        "review-rebriefed",
        "--verdict",
        "approved",
    ]));
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Someone Else").args([
        "brief",
        "review-rebriefed",
        "--body-file",
        "-",
        "--cause-note",
        "a later correction nobody re-snapshotted against",
    ]));

    // A lead recording both the brief and the verdict for an executor: the
    // effective author is the executor on both sides.
    let delegated = tagged_patchset(&repo, "review-delegated");
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "brief",
        "review-delegated",
        "--body-file",
        "-",
        "--on-behalf-of",
        "Executor",
    ]));
    stdout(
        repo.arc(&delegated)
            .env("ARC_ACTOR", "Executor")
            .args(["snapshot", "review-delegated"]),
    );
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Lead").args([
        "review",
        "review-delegated",
        "--verdict",
        "approved",
        "--on-behalf-of",
        "Executor",
    ]));

    let output = chain_review_json(&repo, "program");
    let member = |slug: &str| {
        output["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|member| member["slug"] == slug)
            .unwrap()
            .clone()
    };

    let rebriefed = member("review-rebriefed");
    assert_eq!(
        rebriefed["review"]["brief_author"], "Lead",
        "the patchset was built from Lead's brief, whatever landed later: {rebriefed}"
    );
    assert_eq!(
        rebriefed["review"]["reviewed_only_by_brief_author"], true,
        "{rebriefed}"
    );

    let delegated = member("review-delegated");
    assert_eq!(
        delegated["review"]["brief_author"], "Executor",
        "{delegated}"
    );
    assert_eq!(
        delegated["review"]["reviewed_only_by_brief_author"], true,
        "{delegated}"
    );
}

#[test]
fn chain_review_counts_recorded_final_patchset_evidence_exactly() {
    let repo = Repo::new();
    let worktree = tagged_patchset(&repo, "review-counts");
    stdout(repo.arc(&worktree).args(["snapshot", "review-counts"]));
    stdout(repo.arc(&repo.root).args([
        "finding",
        "review-counts",
        "--summary",
        "recorded problem",
    ]));
    stdout(
        repo.arc(&repo.root)
            .args(["review", "review-counts", "--verdict", "comment-only"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["review", "review-counts", "--verdict", "approved"]),
    );
    stdout(
        repo.arc(&worktree)
            .args(["verify", "review-counts", "--command", "true"]),
    );
    begin(&repo, "review-zero", &["--tag", "program"]);

    let output = chain_review_json(&repo, "program");
    let counted = output["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["slug"] == "review-counts")
        .unwrap();
    let zero = output["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["slug"] == "review-zero")
        .unwrap();
    assert_eq!(counted["review"]["at_final"]["verdicts"], 2);
    assert_eq!(counted["review"]["at_final"]["findings"], 1);
    assert_eq!(counted["review"]["at_final"]["ad_hoc_verifications"], 1);
    assert_eq!(counted["review"]["lifetime"], counted["review"]["at_final"]);
    assert_eq!(zero["review"]["at_final"]["verdicts"], 0);
    assert_eq!(zero["review"]["at_final"]["findings"], 0);
    assert_eq!(zero["review"]["at_final"]["ad_hoc_verifications"], 0);
    assert_eq!(zero["review"]["lifetime"], zero["review"]["at_final"]);
}

#[test]
fn chain_review_ignores_superseded_patchsets() {
    let repo = Repo::new();
    let worktree = tagged_patchset(&repo, "review-stale");
    stdout(repo.arc(&worktree).args(["snapshot", "review-stale"]));
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "Carol").args([
        "review",
        "review-stale",
        "--verdict",
        "approved",
    ]));
    stdout(repo.arc(&repo.root).args([
        "finding",
        "review-stale",
        "--summary",
        "superseded problem",
    ]));
    repo.commit(
        &worktree,
        "review-stale.txt",
        "reviewed again\n",
        "test: update review-stale",
    );
    stdout(repo.arc(&worktree).args(["snapshot", "review-stale"]));

    let output = chain_review_json(&repo, "program");
    assert_eq!(output["members"][0]["review"]["at_final"]["verdicts"], 0);
    assert_eq!(
        output["members"][0]["review"]["at_final"]["identities"],
        serde_json::json!([])
    );
    assert_eq!(output["members"][0]["review"]["at_final"]["findings"], 0);
    assert_eq!(output["members"][0]["review"]["lifetime"]["verdicts"], 1);
    assert_eq!(
        output["members"][0]["review"]["lifetime"]["identities"],
        serde_json::json!(["Carol"])
    );
    assert_eq!(output["members"][0]["review"]["lifetime"]["findings"], 1);
    assert!(output["members"][0]["review"]["reviewed_only_by_brief_author"].is_null());
}

#[test]
fn chain_review_json_carries_its_schema_version() {
    let repo = Repo::new();
    begin(&repo, "review-schema", &["--tag", "program"]);

    let output = chain_review_json(&repo, "program");
    assert_eq!(output["schema"], "arc-chain/3");
}
