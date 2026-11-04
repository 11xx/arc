//! `arc-review-receipt/1`: a delegated reader's receipt recorded as the
//! ordinary verdicts and audits it reports, item by item, through the guards a
//! hand-recorded read passes.

use super::common::*;
use super::ineligible_review::snapshotted;
use super::review_clearance::assessed_repo;

const REVIEWER_SESSION: &str = "01a12b77-6c25-7c01-b325-6e5e44523cc4";
const REVIEWER_MODEL: &str = "fixture-other#high";

fn reviewer_subject() -> String {
    format!("codex:{REVIEWER_SESSION}:{REVIEWER_MODEL}")
}

fn template(repo: &Repo, kind: &str, slugs: &[&str]) -> serde_json::Value {
    json_stdout(
        repo.arc(&repo.root)
            .args(["receipt", "template", &format!("--{kind}")])
            .args(slugs),
    )
}

/// The template with its reviewer filled in and every item given `verdict`.
fn filled(mut receipt: serde_json::Value, verdict: &str) -> serde_json::Value {
    receipt["reviewer"] = serde_json::json!({
        "harness": "codex",
        "session": REVIEWER_SESSION,
        "model": REVIEWER_MODEL,
    });
    for item in receipt["items"].as_array_mut().unwrap() {
        item["verdict"] = serde_json::json!(verdict);
        item["body"] = serde_json::json!("read the whole change");
    }
    receipt
}

fn write_receipt(repo: &Repo, name: &str, receipt: &serde_json::Value) -> PathBuf {
    let path = repo.home.join(name);
    fs::write(&path, json_file_bytes(receipt)).unwrap();
    path
}

/// Record the receipt at `path` with `arc <command> --receipt --json`.
fn ingest(repo: &Repo, command: &str, path: &Path) -> (i32, serde_json::Value) {
    let output = repo
        .arc(&repo.root)
        .args([command, "--receipt"])
        .arg(path)
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

fn events(repo: &Repo, slug: &str) -> Vec<serde_json::Value> {
    stdout(repo.arc(&repo.root).args(["events", "--change", slug]))
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn receipt_reads(repo: &Repo, slug: &str) -> Vec<serde_json::Value> {
    events(repo, slug)
        .into_iter()
        .filter(|event| !event["receipt"].is_null())
        .collect()
}

fn item(result: &serde_json::Value, index: usize) -> &serde_json::Value {
    let item = &result["items"][index];
    assert_eq!(item["index"], index, "{result:#}");
    item
}

fn slugs(prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{prefix}{index:02}"))
        .collect()
}

/// Claim: one command records a two-item verdict receipt, each item as an
/// ordinary verdict on its own change that names the receipt's reviewer as
/// the represented subject, the invoker as writer, and the receipt it came
/// from, whose bytes the ledger keeps. Rejects transcription: without
/// receipts there is no such command, and a lead writes each verdict by hand.
#[test]
fn one_command_records_a_two_item_verdict_receipt() {
    let repo = Repo::new();
    snapshotted(&repo, "one");
    snapshotted(&repo, "two");
    let receipt = filled(template(&repo, "verdict", &["one", "two"]), "approved");
    let path = write_receipt(&repo, "two.json", &receipt);
    let digest = hex_digest(&fs::read(&path).unwrap());

    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(result["schema"], "arc-receipt-result/1");
    assert_eq!(result["receipt_digest"], digest);
    for (index, slug) in ["one", "two"].into_iter().enumerate() {
        let entry = item(&result, index);
        assert_eq!(entry["outcome"], "recorded", "{result:#}");
        let reads = receipt_reads(&repo, slug);
        assert_eq!(reads.len(), 1, "{reads:#?}");
        let read = &reads[0];
        assert_eq!(read["event_type"], "verdict-recorded");
        assert_eq!(read["event_id"], entry["event"]);
        assert_eq!(read["verdict"], "approved");
        assert_eq!(read["actor"], "tester");
        assert_eq!(read["on_behalf_of"], reviewer_subject());
        assert_eq!(read["model"], REVIEWER_MODEL);
        assert_eq!(read["receipt"]["key"], receipt["items"][index]["key"]);
        assert_eq!(read["receipt"]["item_digest"], entry["item_digest"]);
        assert_eq!(read["receipt"]["receipt_digest"], digest);
    }
    let held = repo.root.join(".git/arc/custody/sha256").join(&digest);
    assert_eq!(fs::read(held).unwrap(), fs::read(&path).unwrap());
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Claim: an item whose head is not its patchset's is refused, and its valid
/// siblings still record, whether there is one sibling or nineteen; the exit
/// is nonzero. Rejects a receipt that records a verdict on a head the
/// reviewer did not read, and one whose single bad item suppresses the rest.
#[test]
fn a_wrong_head_item_is_refused_and_its_siblings_record() {
    for siblings in [1, 19] {
        let repo = Repo::new();
        let names = slugs("head", siblings + 1);
        for slug in &names {
            snapshotted(&repo, slug);
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut receipt = filled(template(&repo, "verdict", &refs), "approved");
        let base = receipt["items"][0]["subject"]["base"].clone();
        receipt["items"][0]["subject"]["head"] = base;
        let path = write_receipt(&repo, "heads.json", &receipt);

        let (code, result) = ingest(&repo, "review", &path);
        assert_eq!(code, 1, "{result:#}");
        assert_eq!(result["refused"], 1, "{result:#}");
        let wrong = item(&result, 0);
        assert_eq!(wrong["outcome"], "refused", "{result:#}");
        assert_eq!(wrong["code"], "subject-mismatch", "{result:#}");
        assert!(
            wrong["reason"].as_str().unwrap().contains("head"),
            "{result:#}"
        );
        assert!(receipt_reads(&repo, &names[0]).is_empty());
        for (index, slug) in names.iter().enumerate().skip(1) {
            assert_eq!(item(&result, index)["outcome"], "recorded", "{result:#}");
            assert_eq!(receipt_reads(&repo, slug).len(), 1, "{slug}");
        }
    }
}

/// Claim: an item is bound to the brief its patchset was read under. The
/// same head under another brief digest is refused, and so is a head two
/// patchsets record after the brief was renegotiated. Rejects matching the
/// subject by head alone.
#[test]
fn the_same_head_under_another_brief_is_refused() {
    let repo = Repo::new();
    stdout(repo.arc(&repo.root).args(["begin", "briefed"]));
    let worktree = repo.home.join(".worktrees/repo-briefed");
    repo.commit(&worktree, "briefed.txt", "work\n", "feat: briefed");
    repo.arc(&worktree)
        .args(["brief", "briefed", "--body-file", "-"])
        .write_stdin("contract v1\n")
        .assert()
        .success();
    stdout(repo.arc(&worktree).args(["snapshot", "briefed"]));

    let mut receipt = filled(template(&repo, "verdict", &["briefed"]), "approved");
    let pinned = receipt["items"][0]["subject"]["brief_digest"].clone();
    assert!(
        pinned.as_str().unwrap().starts_with("sha256:"),
        "{receipt:#}"
    );
    receipt["items"][0]["subject"]["brief_digest"] =
        serde_json::json!(format!("sha256:{}", hex_digest(b"contract v2\n")));
    let path = write_receipt(&repo, "digest.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{result:#}");
    assert_eq!(item(&result, 0)["code"], "subject-mismatch", "{result:#}");
    assert!(
        item(&result, 0)["reason"]
            .as_str()
            .unwrap()
            .contains("brief digest"),
        "{result:#}"
    );

    receipt["items"][0]["subject"]["brief_digest"] = pinned;
    repo.arc(&worktree)
        .args(["brief", "briefed", "--body-file", "-"])
        .args(["--cause-note", "fixture revision"])
        .write_stdin("contract v2\n")
        .assert()
        .success();
    stdout(repo.arc(&worktree).args(["snapshot", "briefed"]));
    let path = write_receipt(&repo, "renegotiated.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{result:#}");
    assert_eq!(item(&result, 0)["code"], "ambiguous-head", "{result:#}");
    assert!(receipt_reads(&repo, "briefed").is_empty());
}

/// Claim: an approval carrying a blocking finding is refused as a
/// contradiction whose reason keeps the finding's flag. Rejects normalising
/// the report by clearing the flag, or an error that loses which finding
/// blocked.
#[test]
fn an_approval_with_a_blocking_finding_is_refused_with_its_flag() {
    let repo = Repo::new();
    snapshotted(&repo, "blocked");
    let mut receipt = filled(template(&repo, "verdict", &["blocked"]), "approved");
    receipt["items"][0]["findings"] = serde_json::json!([
        {"severity": "major", "blocking": true, "summary": "loses the flag"},
        {"severity": "note", "blocking": false, "summary": "fine"},
    ]);
    let path = write_receipt(&repo, "blocked.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{result:#}");
    let refused = item(&result, 0);
    assert_eq!(refused["code"], "contradiction", "{result:#}");
    let reason = refused["reason"].as_str().unwrap();
    assert!(reason.contains("\"loses the flag\""), "{reason}");
    assert!(reason.contains("blocking: true"), "{reason}");
    assert!(!reason.contains("\"fine\""), "{reason}");
    assert!(receipt_reads(&repo, "blocked").is_empty());
}

/// Claim: a verdict recorded after the template and not named in its
/// frontier refuses the item as `stale-frontier`, naming the verdict.
/// Rejects an approval recorded over a refusal its reviewer never saw.
#[test]
fn a_verdict_after_the_template_makes_the_item_stale() {
    let repo = Repo::new();
    snapshotted(&repo, "raced");
    let receipt = filled(template(&repo, "verdict", &["raced"]), "approved");
    let refusal = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "Other Reviewer <other@example.com>")
            .args(["review", "raced", "--verdict", "changes-requested"])
            .args(["--cause", "executor"]),
    );
    let refusal = refusal
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .unwrap()
        .to_string();
    let path = write_receipt(&repo, "raced.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{result:#}");
    let stale = item(&result, 0);
    assert_eq!(stale["code"], "stale-frontier", "{result:#}");
    assert_eq!(
        stale["unobserved"],
        serde_json::json!([refusal]),
        "{result:#}"
    );
    assert!(stale["reason"].as_str().unwrap().contains(&refusal));
    assert!(receipt_reads(&repo, "raced").is_empty());
}

/// Claim: an item is published once. Recording the same receipt again
/// writes nothing and returns the event; a batch interrupted after its first
/// item, rerun, publishes only the rest; an item changed under its key is
/// refused naming both digests, while its unchanged sibling in the same file
/// is not. Rejects a retry that fails its own frontier check, a second
/// attribution of one reading, and a conflict raised by a sibling's edit.
#[test]
fn an_item_is_published_once_across_retries_and_interruption() {
    let repo = Repo::new();
    snapshotted(&repo, "first");
    snapshotted(&repo, "second");
    let receipt = filled(template(&repo, "verdict", &["first", "second"]), "approved");
    let path = write_receipt(&repo, "batch.json", &receipt);

    let release = repo.home.join("never-released");
    let mut child = spawn_arc_with_env(
        &repo,
        &repo.root,
        &["review", "--receipt", path.to_str().unwrap(), "--json"],
        &[("ARC_RECEIPT_PAUSE", release.as_path())],
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while receipt_reads(&repo, "first").is_empty() {
        assert!(Instant::now() < deadline, "the first item never published");
        thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(receipt_reads(&repo, "second").is_empty());
    let published = receipt_reads(&repo, "first")[0]["event_id"].clone();

    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(
        item(&result, 0)["outcome"],
        "already-recorded",
        "{result:#}"
    );
    assert_eq!(item(&result, 0)["event"], published, "{result:#}");
    assert_eq!(item(&result, 1)["outcome"], "recorded", "{result:#}");
    let counts = [events(&repo, "first").len(), events(&repo, "second").len()];

    let (code, again) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{again:#}");
    for index in 0..2 {
        assert_eq!(item(&again, index)["outcome"], "already-recorded");
        assert_eq!(item(&again, index)["event"], item(&result, index)["event"]);
    }
    assert_eq!(
        [events(&repo, "first").len(), events(&repo, "second").len()],
        counts
    );

    let mut edited = receipt.clone();
    edited["items"][0]["body"] = serde_json::json!("a different reading");
    let path = write_receipt(&repo, "edited.json", &edited);
    let (code, conflict) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{conflict:#}");
    let refused = item(&conflict, 0);
    assert_eq!(refused["code"], "key-conflict", "{conflict:#}");
    let reason = refused["reason"].as_str().unwrap();
    assert!(
        reason.contains(item(&result, 0)["item_digest"].as_str().unwrap()),
        "{reason}"
    );
    assert!(
        reason.contains(refused["item_digest"].as_str().unwrap()),
        "{reason}"
    );
    assert_eq!(item(&conflict, 1)["outcome"], "already-recorded");
    assert_eq!(
        [events(&repo, "first").len(), events(&repo, "second").len()],
        counts
    );
    for slug in ["first", "second"] {
        assert_eq!(receipt_reads(&repo, slug).len(), 1, "{slug}");
    }
}

/// Claim: a receipt's `still-open` disposition reopens a resolved blocking
/// finding inside the verdict's own event, so a later ordinary approval
/// leaves integration blocked, and the store is stamped with the format that
/// makes an older reader refuse it. Rejects a reader that ignores the inline
/// disposition and integrates on the finding's earlier resolution.
#[test]
fn a_receipt_reopening_a_finding_keeps_integration_blocked() {
    let repo = Repo::new();
    snapshotted(&repo, "reopen");
    let findings = repo.home.join("finding.json");
    fs::write(
        &findings,
        r#"[{"severity": "major", "blocking": true, "summary": "races the lock"}]"#,
    )
    .unwrap();
    let raised = stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "First Reviewer <first@example.com>")
            .args(["review", "reopen", "--verdict", "changes-requested"])
            .args(["--cause", "executor", "--findings-json"])
            .arg(&findings),
    );
    let finding = raised
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap()
        .to_string();
    repo.arc(&repo.root)
        .args(["resolve", "reopen", &finding, "--status", "resolved"])
        .assert()
        .success();

    let mut receipt = filled(template(&repo, "verdict", &["reopen"]), "changes-requested");
    receipt["items"][0]["causes"] = serde_json::json!(["executor"]);
    receipt["items"][0]["dispositions"] = serde_json::json!([
        {"finding": finding, "disposition": "still-open", "evidence": "the race is still there"},
    ]);
    let path = write_receipt(&repo, "reopen.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    let read = &receipt_reads(&repo, "reopen")[0];
    assert_eq!(read["dispositions"][0]["finding_id"], finding);
    assert_eq!(read["dispositions"][0]["status"], "still-open");

    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Approver <approver@example.com>")
        .args(["review", "reopen", "--verdict", "approved"])
        .assert()
        .success();
    let refused = repo
        .arc(&repo.root)
        .args(["integrate", "reopen"])
        .output()
        .unwrap();
    assert!(!refused.status.success(), "integration was not blocked");
    let said = String::from_utf8_lossy(&refused.stderr).into_owned()
        + &String::from_utf8_lossy(&refused.stdout);
    assert!(said.contains(&finding), "{said}");

    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.root.join(".git/arc/config.json")).unwrap()).unwrap();
    assert_eq!(config["schema_version"], 7, "{config}");
}

/// A change that integrated owing a review, whose commit names `trailer`
/// as its implementer when one is given.
fn owed(repo: &Repo, slug: &str, trailer: Option<&str>) {
    stdout(repo.arc(&repo.root).args(["begin", slug]));
    let worktree = repo.home.join(".worktrees").join(format!("repo-{slug}"));
    let message = match trailer {
        Some(trailer) => format!("feat: {slug}\n\nImplemented-by: {trailer}"),
        None => format!("feat: {slug}"),
    };
    repo.commit(&worktree, &format!("{slug}.txt"), "work\n", &message);
    stdout(repo.arc(&worktree).args(["snapshot", slug]));
    repo.arc(&repo.root)
        .args(["integrate", slug, "--debt", "nobody read it"])
        .assert()
        .success();
}

fn clearing_assessment(clears: bool) -> serde_json::Value {
    serde_json::json!({
        "schema": "agent-routing-assessment/2",
        "independence": "eligible",
        "findings": "eligible",
        "clears": clears,
        "qualifiers": if clears { serde_json::json!([]) } else { serde_json::json!(["below-clearance"]) },
        "route_version": "2026.11.3",
        "reviewer": {"identities": ["fixture-other"], "effort": "high"},
        "authors": [{"identities": ["fixture-opus"], "effort": null}],
    })
}

/// Claim: each audit item of a twenty-item receipt is judged by the
/// predicate a hand-recorded audit meets. Seventeen bound, clearing reads
/// discharge their debts; the read whose assessment omits an author is
/// recorded with `authors-uncovered` and discharges nothing; the
/// non-clearing read is recorded as a findings read; the item with a wrong
/// subject is refused; the exit is nonzero. Rejects a receipt that clears
/// what a findings read should not, and one whose bad item refuses the rest.
#[test]
fn a_twenty_item_audit_receipt_judges_each_read_on_its_own() {
    let repo = assessed_repo();
    let names = slugs("audit", 20);
    for (index, slug) in names.iter().enumerate() {
        let trailer = (index == 1).then_some("codex:gpt-6-astra#high");
        owed(&repo, slug, trailer);
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut receipt = filled(template(&repo, "audit", &refs), "approved");
    for (index, item) in receipt["items"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        item["assessment"] = clearing_assessment(index != 2);
    }
    receipt["items"][0]["subject"]["revision"] = serde_json::json!("0".repeat(40));
    let path = write_receipt(&repo, "audits.json", &receipt);

    let (code, result) = ingest(&repo, "audit", &path);
    assert_eq!(code, 1, "{result:#}");
    assert_eq!(result["refused"], 1, "{result:#}");
    let wrong = item(&result, 0);
    assert_eq!(wrong["outcome"], "refused", "{result:#}");
    assert_eq!(wrong["code"], "subject-mismatch", "{result:#}");
    assert!(receipt_reads(&repo, &names[0]).is_empty());

    let uncovered = item(&result, 1);
    assert_eq!(uncovered["outcome"], "recorded", "{result:#}");
    assert_eq!(uncovered["binding"], "authors-uncovered", "{result:#}");
    assert_eq!(
        uncovered["uncovered_authors"],
        serde_json::json!(["gpt-6-astra#high"]),
        "{result:#}"
    );
    assert_eq!(uncovered["debt_discharged"], false, "{result:#}");

    let findings = item(&result, 2);
    assert_eq!(findings["outcome"], "recorded", "{result:#}");
    assert_eq!(findings["debt_discharged"], false, "{result:#}");
    assert_eq!(findings["bar"]["code"], "findings-read", "{result:#}");
    let debt = &json_stdout(repo.arc(&repo.root).args(["status", &names[2], "--json"]))["debt"];
    assert_eq!(debt["findings_reads"][0]["kind"], "findings", "{debt}");

    let mut discharged = 0;
    for (index, slug) in names.iter().enumerate().skip(3) {
        let entry = item(&result, index);
        assert_eq!(entry["outcome"], "recorded", "{result:#}");
        assert_eq!(entry["binding"], "bound", "{result:#}");
        assert_eq!(entry["debt_discharged"], true, "{result:#}");
        let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
        assert_eq!(status["debt_outstanding"], false, "{slug}: {status}");
        discharged += 1;
    }
    assert_eq!(discharged, 17);
    for slug in &names[1..3] {
        let status = json_stdout(repo.arc(&repo.root).args(["status", slug, "--json"]));
        assert_eq!(status["debt_outstanding"], true, "{slug}: {status}");
    }
}

/// Claim: whole-receipt errors refuse everything and write nothing: a mixed
/// receipt, a receipt for the other command, a reviewer session that is a
/// prefix or a delegate handle, and a verdict item carrying an extent is
/// refused on its own. Rejects recording a reviewer arc cannot corroborate,
/// and silently dropping a coverage claim a verdict cannot carry.
#[test]
fn envelope_errors_refuse_the_whole_receipt() {
    let repo = Repo::new();
    snapshotted(&repo, "env");
    let receipt = filled(template(&repo, "verdict", &["env"]), "approved");
    let refuse = |name: &str, receipt: &serde_json::Value, command: &str, needle: &str| {
        let path = write_receipt(&repo, name, receipt);
        let output = repo
            .arc(&repo.root)
            .args([command, "--receipt"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{name}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(needle), "{name}: {stderr}");
        assert!(receipt_reads(&repo, "env").is_empty(), "{name}");
        assert!(!repo.root.join(".git/arc/custody").exists(), "{name}");
    };
    let mut prefix = receipt.clone();
    prefix["reviewer"]["session"] = serde_json::json!(&REVIEWER_SESSION[..8]);
    refuse(
        "prefix.json",
        &prefix,
        "review",
        "not a full native codex session",
    );
    let mut handle = receipt.clone();
    handle["reviewer"]["session"] =
        serde_json::json!("series-opus-3b-receipts-20261103T182958Z-da75");
    refuse(
        "handle.json",
        &handle,
        "review",
        "not a full native codex session",
    );
    let mut effortless = receipt.clone();
    effortless["reviewer"]["model"] = serde_json::json!("fixture-other");
    refuse("effort.json", &effortless, "review", "model#effort");
    refuse("other.json", &receipt, "audit", "arc review --receipt");
    let mut mixed = receipt.clone();
    let mut audit = mixed["items"][0].clone();
    audit["kind"] = serde_json::json!("audit");
    mixed["items"].as_array_mut().unwrap().push(audit);
    refuse(
        "mixed.json",
        &mixed,
        "review",
        "mixes verdict and audit items",
    );

    let mut extent = receipt.clone();
    extent["items"][0]["extent"] = serde_json::json!({"whole": true});
    let path = write_receipt(&repo, "extent.json", &extent);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{result:#}");
    assert_eq!(
        item(&result, 0)["code"],
        "extent-not-accepted",
        "{result:#}"
    );
    assert!(receipt_reads(&repo, "env").is_empty());
}

/// Claim: where the reviewer's session store holds the session, a model the
/// store observed that differs from the receipt's is recorded on the read
/// as a disagreement, and the session is recorded as given, never
/// substituted. Rejects recording the declared model as uncontested.
#[test]
fn a_reviewer_model_the_session_store_contradicts_is_recorded() {
    let repo = Repo::new();
    snapshotted(&repo, "corroborated");
    codex_recording(&repo, REVIEWER_SESSION, "fixture-observed");
    let receipt = filled(template(&repo, "verdict", &["corroborated"]), "approved");
    let path = write_receipt(&repo, "corroborated.json", &receipt);
    let output = repo
        .arc(&repo.root)
        .env("CODEX_HOME", repo.home.join("codex-state"))
        .args(["review", "--receipt"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fixture-observed"), "{stderr}");
    let read = &receipt_reads(&repo, "corroborated")[0];
    assert_eq!(read["on_behalf_of"], reviewer_subject());
    assert_eq!(read["model"], REVIEWER_MODEL);
    assert_eq!(
        read["model_disagreement"]["observed"]
            .as_str()
            .map(|m| m.starts_with("fixture-observed")),
        Some(true),
        "{read:#}"
    );
}

const SEVERITIES: [&str; 4] = ["critical", "major", "minor", "note"];

/// Claim: the severities a receipt finding accepts are named where a reader
/// fills one in, in `arc receipt --help` beside `severity` and in the
/// template's vocabulary, and a severity outside them is refused with a
/// message listing them. Rejects a reader left to guess the vocabulary and
/// writing `high`, which the receipt then refuses.
#[test]
fn the_severity_vocabulary_is_named_where_a_reader_fills_a_finding() {
    let repo = Repo::new();
    let help = stdout(repo.arc(&repo.root).args(["receipt", "--help"]));
    let named = "`severity` (`critical`, `major`, `minor` or `note`)";
    assert!(help.contains(named), "{help}");

    snapshotted(&repo, "severity");
    let issued = template(&repo, "verdict", &["severity"]);
    assert_eq!(
        issued["vocabulary"]["findings[].severity"],
        serde_json::json!(SEVERITIES),
        "{issued:#}"
    );

    let mut high = filled(issued.clone(), "changes-requested");
    high["items"][0]["causes"] = serde_json::json!(["executor"]);
    high["items"][0]["findings"] =
        serde_json::json!([{"severity": "high", "blocking": true, "summary": "loses the flag"}]);
    let path = write_receipt(&repo, "high.json", &high);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 1, "{result:#}");
    let refused = item(&result, 0);
    assert_eq!(refused["code"], "invalid", "{result:#}");
    let reason = refused["reason"].as_str().unwrap();
    for severity in SEVERITIES {
        assert!(reason.contains(&format!("`{severity}`")), "{reason}");
    }
    assert!(receipt_reads(&repo, "severity").is_empty());

    let mut major = high.clone();
    major["items"][0]["findings"][0]["severity"] = serde_json::json!("major");
    let path = write_receipt(&repo, "major.json", &major);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(receipt_reads(&repo, "severity").len(), 1);

    let mut edited = filled(issued, "approved");
    edited["vocabulary"]["findings[].severity"] = serde_json::json!(["high", "low"]);
    let path = write_receipt(&repo, "edited.json", &edited);
    let output = repo
        .arc(&repo.root)
        .args(["review", "--receipt"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("vocabulary"), "{stderr}");
    assert_eq!(receipt_reads(&repo, "severity").len(), 1);
}
