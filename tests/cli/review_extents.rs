//! Typed review extents: an obligation names what the owed review must
//! cover, a read names what it covered, and a read discharges only the
//! obligation it covers.

use super::common::*;

const AUDITOR_MODEL: &str = "fixture-other#high";

/// A change that adds `sdk/a.txt`, `sdk/b.txt` and `core/c.txt` and deletes
/// `sdk/old.txt`, snapshotted and not yet integrated.
fn extent_change(repo: &Repo, slug: &str) {
    fs::create_dir_all(repo.root.join("sdk")).unwrap();
    fs::write(repo.root.join("sdk/old.txt"), format!("{slug} old\n")).unwrap();
    git(&repo.root, &["add", "."]);
    git(
        &repo.root,
        &["commit", "-m", &format!("chore: {slug} base")],
    );
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    fs::create_dir_all(worktree.join("core")).unwrap();
    fs::write(worktree.join("sdk/a.txt"), "a\n").unwrap();
    fs::write(worktree.join("sdk/b.txt"), "b\n").unwrap();
    fs::write(worktree.join("core/c.txt"), "c\n").unwrap();
    fs::remove_file(worktree.join("sdk/old.txt")).unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-m", &format!("feat: {slug}")]);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
}

/// `extent_change`, owing a review declared with `extent`, then integrated.
fn owed_with(repo: &Repo, slug: &str, extent: &[&str]) {
    extent_change(repo, slug);
    stdout(
        repo.arc(&repo.root)
            .args(["debt", slug, "--reason", "nobody read it"])
            .args(extent),
    );
    stdout(repo.arc(&repo.root).args(["integrate", slug]));
}

/// An approving audit of `slug` by an independent auditor, reading `extent`.
fn audit(repo: &Repo, slug: &str, extent: &[&str]) -> String {
    stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "Auditor")
            .args(["--model", AUDITOR_MODEL])
            .args(["audit", slug, "--verdict", "approved"])
            .args(extent),
    )
}

fn debt_of(repo: &Repo, slug: &str) -> serde_json::Value {
    json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]))["debt"].clone()
}

fn event_of(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap_or_else(|| panic!("no event line in {output}"))
        .trim()
        .to_string()
}

/// The debt is owed and lists `event` as partial coverage for `reason`.
fn assert_partial(repo: &Repo, slug: &str, event: &str, reason: &str) -> serde_json::Value {
    let debt = debt_of(repo, slug);
    assert_eq!(debt["standing"], "owed", "{debt}");
    assert!(debt["discharged_by"].is_null(), "{debt}");
    let partial = debt["partial_coverage"]
        .as_array()
        .unwrap_or_else(|| panic!("no partial coverage: {debt}"))
        .iter()
        .find(|read| read["event_id"] == event)
        .unwrap_or_else(|| panic!("{event} is not listed as partial coverage: {debt}"))
        .clone();
    assert_eq!(partial["kind"], "partial", "{debt}");
    assert_eq!(partial["reason"], reason, "{debt}");
    partial
}

fn assert_discharged_by(repo: &Repo, slug: &str, event: &str) -> serde_json::Value {
    let debt = debt_of(repo, slug);
    assert_eq!(debt["standing"], "discharged", "{debt}");
    assert_eq!(debt["discharged_by_event_id"], event, "{debt}");
    debt
}

/// Claim: a scoped audit covering only `sdk/` stays visible as partial
/// coverage in `status`, `show` and `check` while a whole-change obligation
/// stays owed, and a whole audit at the same subject discharges it. Rejects
/// a scoped read discharging a whole obligation because it shares the head.
#[test]
fn a_scoped_audit_is_partial_coverage_of_a_whole_obligation() {
    let repo = Repo::new();
    owed_with(&repo, "whole", &["--extent", "whole"]);
    let debt = debt_of(&repo, "whole");
    assert_eq!(debt["extent"]["domain"], "diff", "{debt}");
    assert!(debt["extent"].get("scope").is_none(), "{debt}");

    let output = audit(&repo, "whole", &["--extent-paths", "sdk/"]);
    assert!(output.contains("partial coverage"), "{output}");
    let scoped = event_of(&output);
    let partial = assert_partial(&repo, "whole", &scoped, "scoped-read");
    let paths: Vec<&str> = partial["extent"]["scope"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["sdk/a.txt", "sdk/b.txt", "sdk/old.txt"],
        "{partial}"
    );

    let show = json_stdout(repo.arc(&repo.root).args(["show", "whole", "--json"]));
    assert_eq!(
        show["debt"]["partial_coverage"][0]["event_id"], scoped,
        "{show}"
    );
    let text = stdout(repo.arc(&repo.root).args(["show", "whole"]));
    assert!(
        text.contains(&format!("partial coverage {scoped}")),
        "{text}"
    );
    let check = json_stdout_any_status(repo.arc(&repo.root).args(["check", "whole", "--json"]));
    assert_eq!(
        check["debt_partial_coverage"][0]["event_id"], scoped,
        "{check}"
    );

    let whole = event_of(&audit(&repo, "whole", &["--extent", "whole"]));
    let debt = assert_discharged_by(&repo, "whole", &whole);
    assert_eq!(debt["discharge_extent"], "covered", "{debt}");
}

/// Claim: a scoped obligation over `sdk/` is discharged by a scoped audit
/// listing every one of its entries at the same blobs, and not by one that
/// excludes a file in it. Rejects an exclusion counting as read.
#[test]
fn a_scoped_obligation_is_discharged_entry_for_entry() {
    let repo = Repo::new();
    owed_with(&repo, "scoped", &["--extent-paths", "sdk/"]);
    let excluding = event_of(&audit(
        &repo,
        "scoped",
        &["--extent-paths", "sdk/", "--extent-exclude", "sdk/b.txt"],
    ));
    let partial = assert_partial(&repo, "scoped", &excluding, "entries-excluded");
    assert_eq!(
        partial["paths"],
        serde_json::json!(["sdk/b.txt"]),
        "{partial}"
    );

    let covering = event_of(&audit(&repo, "scoped", &["--extent-paths", "sdk/"]));
    assert_discharged_by(&repo, "scoped", &covering);
}

/// Claim: a deletion in the obligation is covered only by a read listing
/// that deletion. Rejects comparing only the blobs present at the revision.
#[test]
fn a_deletion_is_covered_only_by_a_read_listing_it() {
    let repo = Repo::new();
    owed_with(&repo, "deleted", &["--extent-paths", "sdk/"]);
    let entries = debt_of(&repo, "deleted")["extent"]["scope"]["entries"].clone();
    let deletion = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "sdk/old.txt")
        .unwrap_or_else(|| panic!("no deletion entry: {entries}"));
    assert!(deletion["deleted"].is_string(), "{entries}");
    assert!(deletion.get("blob").is_none(), "{entries}");

    let without = event_of(&audit(
        &repo,
        "deleted",
        &["--extent-paths", "sdk/a.txt", "sdk/b.txt"],
    ));
    let partial = assert_partial(&repo, "deleted", &without, "entries-missing");
    assert_eq!(
        partial["paths"],
        serde_json::json!(["sdk/old.txt"]),
        "{partial}"
    );

    let with = event_of(&audit(
        &repo,
        "deleted",
        &["--extent-paths", "sdk/a.txt", "sdk/b.txt", "sdk/old.txt"],
    ));
    assert_discharged_by(&repo, "deleted", &with);
}

/// Claim: a tree read never covers a diff obligation, even whole. Rejects a
/// whole read covering across domains.
#[test]
fn a_tree_read_never_covers_a_diff_obligation() {
    let repo = Repo::new();
    owed_with(&repo, "domains", &["--extent", "whole"]);
    let tree = event_of(&audit(
        &repo,
        "domains",
        &["--extent", "whole", "--extent-domain", "tree"],
    ));
    assert_partial(&repo, "domains", &tree, "domain-mismatch");
}

/// Claim: an obligation with an extent is not discharged by an audit
/// without one, and an obligation and audit that both lack one discharge as
/// they do without extents, shown as `extent: not established`. Rejects an
/// absent extent reading as whole, and a regression in the legacy rule.
#[test]
fn an_unestablished_extent_discharges_only_an_unestablished_obligation() {
    let repo = Repo::new();
    owed_with(&repo, "typed", &["--extent", "whole"]);
    let legacy = event_of(&audit(&repo, "typed", &[]));
    assert_partial(&repo, "typed", &legacy, "read-extent-not-established");

    extent_change(&repo, "legacy");
    stdout(
        repo.arc(&repo.root)
            .args(["integrate", "legacy", "--debt", "nobody read it"]),
    );
    let debt = debt_of(&repo, "legacy");
    assert!(debt.get("extent").is_none(), "{debt}");
    let read = event_of(&audit(&repo, "legacy", &[]));
    let debt = assert_discharged_by(&repo, "legacy", &read);
    assert_eq!(debt["discharge_extent"], "not-established", "{debt}");
    let text = stdout(repo.arc(&repo.root).args(["show", "legacy"]));
    assert!(text.contains("Extent: not established"), "{text}");
    assert!(text.contains("extent: not established"), "{text}");
}

/// Claim: a glob that matches nothing is refused and writes nothing. Rejects
/// recording a claim about entries that do not exist.
#[test]
fn an_extent_glob_matching_nothing_is_refused() {
    let repo = Repo::new();
    owed_with(&repo, "nomatch", &["--extent", "whole"]);
    let before = event_count_of(&repo, "nomatch");
    let output = repo
        .arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args(["--model", AUDITOR_MODEL])
        .args(["audit", "nomatch", "--verdict", "approved"])
        .args(["--extent-paths", "docs/"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("matches no entry"), "{stderr}");
    assert_eq!(event_count_of(&repo, "nomatch"), before);
}

fn event_count_of(repo: &Repo, slug: &str) -> usize {
    stdout(repo.arc(&repo.root).args(["events", "--change", slug]))
        .lines()
        .count()
}

const REVIEWER_SESSION: &str = "01a12b77-6c25-7c01-b325-6e5e44523cc4";

/// The audit template for `slugs`, filled in as approvals by the fixture
/// reviewer.
fn audit_receipt(repo: &Repo, slugs: &[&str]) -> serde_json::Value {
    let mut receipt = json_stdout(
        repo.arc(&repo.root)
            .args(["receipt", "template", "--audit"])
            .args(slugs),
    );
    receipt["reviewer"] = serde_json::json!({
        "harness": "codex",
        "session": REVIEWER_SESSION,
        "model": AUDITOR_MODEL,
    });
    for item in receipt["items"].as_array_mut().unwrap() {
        assert!(item["extent"].is_null(), "{item}");
        item["verdict"] = serde_json::json!("approved");
    }
    receipt
}

fn ingest_audits(repo: &Repo, receipt: &serde_json::Value) -> (i32, serde_json::Value) {
    let path = repo.home.join("receipt.json");
    fs::write(&path, json_file_bytes(receipt)).unwrap();
    let output = repo
        .arc(&repo.root)
        .args(["audit", "--receipt"])
        .arg(&path)
        .arg("--json")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let value = serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!(
            "not JSON ({error}): {text}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap(), value)
}

/// Claim: a receipt audit item's `extent` is resolved against what shipped
/// and recorded on the audit, and its coverage judges discharge as a
/// hand-recorded audit's does: a scoped item leaves a whole obligation owed
/// as partial coverage, a whole item discharges, and a glob matching nothing
/// refuses only its item. Rejects dropping the reader's stated extent.
#[test]
fn a_receipt_item_extent_judges_discharge_as_an_audit_does() {
    let repo = Repo::new();
    for slug in ["scopedrc", "wholerc", "nomatchrc"] {
        owed_with(&repo, slug, &["--extent", "whole"]);
    }
    let mut receipt = audit_receipt(&repo, &["scopedrc", "wholerc", "nomatchrc"]);
    receipt["items"][0]["extent"] = serde_json::json!({"paths": ["sdk/"]});
    receipt["items"][1]["extent"] = serde_json::json!({"whole": true});
    receipt["items"][2]["extent"] = serde_json::json!({"paths": ["docs/"]});

    let (code, result) = ingest_audits(&repo, &receipt);
    assert_eq!(code, 1, "{result:#}");
    let scoped = &result["items"][0];
    assert_eq!(scoped["outcome"], "recorded", "{result:#}");
    assert_eq!(scoped["debt_discharged"], false, "{result:#}");
    assert_eq!(scoped["bar"]["code"], "extent-uncovered", "{result:#}");
    let scoped_event = scoped["event"].as_str().unwrap();
    assert_partial(&repo, "scopedrc", scoped_event, "scoped-read");
    let recorded = stdout(
        repo.arc(&repo.root)
            .args(["events", "--change", "scopedrc"]),
    )
    .lines()
    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
    .find(|event| event["event_id"] == scoped_event)
    .unwrap();
    assert_eq!(recorded["extent"]["domain"], "diff", "{recorded}");
    assert_eq!(
        recorded["extent"]["scope"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "{recorded}"
    );
    assert!(!recorded.to_string().contains("sdk/\""), "{recorded}");

    let whole = &result["items"][1];
    assert_eq!(whole["debt_discharged"], true, "{result:#}");
    assert_discharged_by(&repo, "wholerc", whole["event"].as_str().unwrap());

    let nomatch = &result["items"][2];
    assert_eq!(nomatch["outcome"], "refused", "{result:#}");
    assert_eq!(nomatch["code"], "extent-unresolved", "{result:#}");
    assert_eq!(debt_of(&repo, "nomatchrc")["standing"], "owed");
}
