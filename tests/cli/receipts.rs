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

/// An assessment of a reviewer answering to `identity` at high effort.
fn assessment_of(repo: &Repo, name: &str, identity: &str) -> PathBuf {
    let mut assessment = clearing_assessment(true);
    assessment["reviewer"]["identities"] = serde_json::json!([identity]);
    assessment["authors"] = serde_json::json!([]);
    let path = repo.home.join(name);
    fs::write(&path, json_file_bytes(&assessment)).unwrap();
    path
}

/// The binding and recorded disagreement of the newest read on `slug`.
fn bound_read(repo: &Repo, slug: &str, event_type: &str) -> (serde_json::Value, String) {
    let read = events(repo, slug)
        .into_iter()
        .rev()
        .find(|event| event["event_type"] == event_type)
        .unwrap_or_else(|| panic!("no {event_type} on {slug}"));
    let observed = read["model_disagreement"]["observed"]
        .as_str()
        .unwrap_or_else(|| panic!("no disagreement recorded: {read:#}"))
        .to_string();
    assert_eq!(read["model_disagreement"]["declared"], REVIEWER_MODEL);
    (read["assessment"]["binding"].clone(), observed)
}

/// Claim: wherever the reviewer's session store observed a model that
/// differs from the declared one, the read records the disagreement and its
/// assessment binds the observed model, whether the read was recorded by
/// hand with `--on-behalf-of harness:session:model` or from a receipt, and
/// for a verdict as for an audit. Rejects a hand-recorded read that binds
/// the declared model the store contradicts, so an assessment of the
/// declared reviewer clears a read the store says another model made.
#[test]
fn a_read_binds_the_model_its_session_store_observed_on_every_path() {
    let repo = assessed_repo();
    codex_recording(&repo, REVIEWER_SESSION, "fixture-observed");
    let codex_home = repo.home.join("codex-state");
    let declared = assessment_of(&repo, "declared.json", "fixture-other");
    let observed = assessment_of(&repo, "observed.json", "fixture-observed");
    let cases = [
        (&declared, serde_json::json!("model-mismatch")),
        (&observed, serde_json::json!("bound")),
    ];
    for (kind, command, event_type) in [
        ("verdict", "review", "verdict-recorded"),
        ("audit", "audit", "audit-verdict-recorded"),
    ] {
        for (index, (assessment, binding)) in cases.iter().enumerate() {
            let hand = format!("{command}-hand-{index}");
            let carried = format!("{command}-receipt-{index}");
            for slug in [&hand, &carried] {
                match kind {
                    "verdict" => snapshotted(&repo, slug),
                    _ => owed(&repo, slug, None),
                }
            }
            let output = repo
                .arc(&repo.root)
                .env("CODEX_HOME", &codex_home)
                .args([command, &hand, "--verdict", "approved"])
                .args(["--on-behalf-of", &reviewer_subject(), "--assessment"])
                .arg(assessment)
                .output()
                .unwrap();
            assert!(output.status.success(), "{hand}: {output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("fixture-observed"), "{hand}: {stderr}");
            let (hand_binding, hand_observed) = bound_read(&repo, &hand, event_type);
            assert!(
                hand_observed.starts_with("fixture-observed"),
                "{hand_observed}"
            );
            assert_eq!(&hand_binding, binding, "{hand}");

            let mut receipt = filled(template(&repo, kind, &[&carried]), "approved");
            receipt["items"][0]["assessment"] =
                serde_json::from_slice(&fs::read(assessment).unwrap()).unwrap();
            let path = write_receipt(&repo, &format!("{carried}.json"), &receipt);
            let output = repo
                .arc(&repo.root)
                .env("CODEX_HOME", &codex_home)
                .args([command, "--receipt"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{carried}: {output:?}");
            let (carried_binding, carried_observed) = bound_read(&repo, &carried, event_type);
            assert_eq!(carried_observed, hand_observed);
            assert_eq!(&carried_binding, binding, "{carried}");
        }
    }
}

/// `arc import` of `bundle` into `repo` from a publisher admitted for the
/// facts and reads the bundle carries, with the sidecar at `custody`.
fn import_with_custody(repo: &Repo, bundle: &Path, custody: &Path) -> std::process::Output {
    let bytes = fs::read(bundle).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let source = parsed["source_replica_id"].as_str().unwrap();
    repo.declare_gates_locally(&format!(
        "[[replica.publishers]]\nprincipal='fixture-change'\nsource_replica_id='{source}'\n\
         [[replica.publishers.origins]]\nreplica_id='{source}'\n\
         roles=['facts','rewrite','review-input']\nactors=[]\n"
    ));
    repo.arc(&repo.root)
        .arg("import")
        .arg(bundle)
        .args(["--publisher", "fixture-change", "--received-sha256"])
        .arg(hex_digest(&bytes))
        .arg("--custody-dir")
        .arg(custody)
        .output()
        .unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn doctor_findings(repo: &Repo, code: &str) -> Vec<String> {
    let report = json_stdout_any_status(repo.arc(&repo.root).args(["doctor", "--json"]));
    ["problems", "advice"]
        .iter()
        .flat_map(|kind| report[kind].as_array().cloned().unwrap_or_default())
        .filter(|finding| finding["code"] == code)
        .map(|finding| finding["detail"].as_str().unwrap().to_string())
        .collect()
}

/// Claim: the receipt a read cites travels with the read. The read records
/// the receipt's byte length; an export with a custody sidecar carries the
/// receipt's bytes; an import elsewhere verifies and keeps them; an import
/// whose sidecar lacks them or holds other bytes is refused as for any
/// custody bytes; and `arc doctor` names a cited receipt whose bytes the
/// ledger does not hold. Rejects an export that leaves the receipt behind,
/// so the importing ledger cites bytes it never received, and a doctor
/// silent about a cited receipt that is gone.
#[test]
fn a_cited_receipt_travels_in_custody_and_doctor_names_it_missing() {
    let source = Repo::new();
    snapshotted(&source, "carried");
    let receipt = filled(template(&source, "verdict", &["carried"]), "approved");
    let path = write_receipt(&source, "carried.json", &receipt);
    let bytes = fs::read(&path).unwrap();
    let digest = hex_digest(&bytes);
    let (code, result) = ingest(&source, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    let read = &receipt_reads(&source, "carried")[0];
    assert_eq!(read["receipt"]["receipt_digest"], digest);
    assert_eq!(read["receipt"]["receipt_byte_length"], bytes.len());
    let key = read["receipt"]["key"].as_str().unwrap().to_string();

    let bundle = source.home.join("carried.bundle.json");
    let sidecar = source.home.join("carried.custody");
    let exported = stdout(
        source
            .arc(&source.root)
            .args(["export", "carried", "--output"])
            .arg(&bundle)
            .arg("--custody-dir")
            .arg(&sidecar),
    );
    assert!(
        exported.contains("custody: 1 entries, 1 available, 0 unavailable"),
        "{exported}"
    );
    assert_eq!(
        fs::read(sidecar.join("sha256").join(&digest)).unwrap(),
        bytes
    );

    let refused = |label: &str, edit: &dyn Fn(&Path)| {
        let replica = Repo::new();
        deliver_git_objects(&source, &replica);
        let copy = replica.home.join(label);
        copy_tree(&sidecar, &copy);
        edit(&copy.join("sha256").join(&digest));
        let output = import_with_custody(&replica, &bundle, &copy);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{label}: {output:?}");
        assert!(stderr.contains("custody sidecar"), "{label}: {stderr}");
        assert!(stderr.contains(&digest[..12]), "{label}: {stderr}");
        assert!(
            !replica.root.join(".git/arc/custody").exists(),
            "{label}: custody was published"
        );
        assert!(
            !replica.root.join(".git/arc/imports").exists(),
            "{label}: the read was imported"
        );
    };
    refused("absent", &|held| fs::remove_file(held).unwrap());
    refused("mismatched", &|held| {
        let mut other = fs::read(held).unwrap();
        other[0] ^= 0x20;
        fs::write(held, other).unwrap();
    });

    let replica = Repo::new();
    deliver_git_objects(&source, &replica);
    let output = import_with_custody(&replica, &bundle, &sidecar);
    assert!(output.status.success(), "{output:?}");
    assert!(replica.root.join(".git/arc/imports").exists());
    let imported = String::from_utf8_lossy(&output.stdout);
    assert!(
        imported.contains("custody: 1 installed, 0 held, 0 unavailable"),
        "{imported}"
    );
    let held = replica.root.join(".git/arc/custody/sha256").join(&digest);
    assert_eq!(fs::read(&held).unwrap(), bytes);
    assert!(doctor_findings(&replica, "receipt-unavailable").is_empty());

    fs::remove_file(&held).unwrap();
    let missing = doctor_findings(&replica, "receipt-unavailable");
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains(&key), "{missing:?}");
    assert!(missing[0].contains(&digest), "{missing:?}");
    assert!(missing[0].contains("carried"), "{missing:?}");
}

/// Claim: a read whose receipt reference records no byte length still
/// reads, `arc doctor` says its bytes cannot travel, and an export lists no
/// custody entry for it. Rejects refusing a ledger that holds such a read,
/// and an export claiming to carry bytes it cannot verify.
#[test]
fn a_receipt_reference_without_a_length_reads_and_is_named() {
    let repo = Repo::new();
    snapshotted(&repo, "lengthless");
    let receipt = filled(template(&repo, "verdict", &["lengthless"]), "approved");
    let path = write_receipt(&repo, "lengthless.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    let event = item(&result, 0)["event"].as_str().unwrap().to_string();
    let change = item(&result, 0)["change"].as_str().unwrap().to_string();
    let file = repo
        .root
        .join(".git/arc/changes")
        .join(&change)
        .join("events")
        .join(format!("{event}.json"));
    let mut stored: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    stored["receipt"]
        .as_object_mut()
        .unwrap()
        .remove("receipt_byte_length")
        .expect("the read records the receipt's length");
    fs::write(&file, json_file_bytes(&stored)).unwrap();

    let state = json_stdout(repo.arc(&repo.root).args(["show", "lengthless", "--json"]));
    let key = receipt["items"][0]["key"].as_str().unwrap();
    assert_eq!(state["verdicts"][0]["receipt"]["key"], key, "{state:#}");
    let named = doctor_findings(&repo, "receipt-length-unrecorded");
    assert_eq!(named.len(), 1, "{named:?}");
    assert!(
        named[0].contains(key) && named[0].contains(&event),
        "{named:?}"
    );

    let sidecar = repo.home.join("lengthless.custody");
    let exported = stdout(
        repo.arc(&repo.root)
            .args(["export", "lengthless", "--output"])
            .arg(repo.home.join("lengthless.bundle.json"))
            .arg("--custody-dir")
            .arg(&sidecar),
    );
    assert!(
        exported.contains("custody: 0 entries, 0 available, 0 unavailable"),
        "{exported}"
    );
}

/// The finding `output` of a hand-recorded read reports.
fn raised_finding(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("finding: "))
        .unwrap_or_else(|| panic!("no finding in {output}"))
        .to_string()
}

/// Claim: `arc show` text names the receipt each read was recorded from
/// and the dispositions it published, for the verdict as for each audit,
/// as `arc show --json` carries them. Rejects text that renders a receipt
/// read exactly like a hand-recorded one, so a reader of the text cannot
/// tell which receipt to fetch or which findings the read disposed.
#[test]
fn show_text_names_each_reads_receipt_and_dispositions() {
    let repo = Repo::new();
    let findings = repo.home.join("finding.json");
    fs::write(
        &findings,
        r#"[{"severity": "major", "blocking": true, "summary": "races the lock"}]"#,
    )
    .unwrap();

    snapshotted(&repo, "shown");
    let finding = raised_finding(&stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "First Reviewer <first@example.com>")
            .args(["review", "shown", "--verdict", "changes-requested"])
            .args(["--cause", "executor", "--findings-json"])
            .arg(&findings),
    ));
    let mut receipt = filled(template(&repo, "verdict", &["shown"]), "changes-requested");
    receipt["items"][0]["causes"] = serde_json::json!(["executor"]);
    receipt["items"][0]["dispositions"] = serde_json::json!([
        {"finding": finding, "disposition": "still-open", "evidence": "the race is still there"},
    ]);
    let path = write_receipt(&repo, "shown.json", &receipt);
    let (code, result) = ingest(&repo, "review", &path);
    assert_eq!(code, 0, "{result:#}");
    let key = receipt["items"][0]["key"].as_str().unwrap();
    let text = stdout(repo.arc(&repo.root).args(["show", "shown"]));
    let verdict = text
        .split("\n## Verdict\n")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap_or_else(|| panic!("no verdict section: {text}"));
    assert!(verdict.contains(&format!("receipt `{key}`")), "{verdict}");
    assert!(
        verdict.contains(&format!("`{finding}` still-open")),
        "{verdict}"
    );
    assert!(verdict.contains("the race is still there"), "{verdict}");

    owed(&repo, "audited", None);
    let audit_finding = raised_finding(&stdout(
        repo.arc(&repo.root)
            .env("ARC_ACTOR", "First Reviewer <first@example.com>")
            .args(["audit", "audited", "--verdict", "changes-requested"])
            .arg("--findings-json")
            .arg(&findings),
    ));
    let mut receipt = filled(template(&repo, "audit", &["audited"]), "changes-requested");
    receipt["items"][0]["dispositions"] = serde_json::json!([
        {"finding": audit_finding, "disposition": "fixed"},
    ]);
    let path = write_receipt(&repo, "audited.json", &receipt);
    let (code, result) = ingest(&repo, "audit", &path);
    assert_eq!(code, 0, "{result:#}");
    let key = receipt["items"][0]["key"].as_str().unwrap();
    let text = stdout(repo.arc(&repo.root).args(["show", "audited"]));
    let line = text
        .lines()
        .find(|line| line.contains(&format!("receipt `{key}`")))
        .unwrap_or_else(|| panic!("no audit line names the receipt: {text}"));
    assert!(line.starts_with("- ChangesRequested at "), "{line}");
    assert!(
        line.contains(&format!("`{audit_finding}` resolved")),
        "{line}"
    );
}
