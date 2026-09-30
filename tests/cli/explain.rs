use super::common::*;

const WORK: &str = "shipped";

/// A journal artifact written through `arc journal`, as its path.
fn artifact(repo: &Repo, verb: &str, topic: &str, body: &str) -> PathBuf {
    let source = repo.home.join(format!("{topic}-body.md"));
    fs::write(&source, body).unwrap();
    let printed = stdout(repo.arc(&repo.root).args([
        "journal",
        verb,
        topic,
        "--body-file",
        source.to_str().unwrap(),
    ]));
    PathBuf::from(printed.trim())
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().to_string()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn event_ids(repo: &Repo, event_type: &str) -> Vec<String> {
    stdout(
        repo.arc(&repo.root)
            .args(["events", "--change", WORK, "--type", event_type]),
    )
    .lines()
    .map(|line| {
        serde_json::from_str::<serde_json::Value>(line).unwrap()["event_id"]
            .as_str()
            .unwrap()
            .to_string()
    })
    .collect()
}

/// Record a brief version on `slug` with `body`.
fn brief(repo: &Repo, cwd: &Path, slug: &str, body: &str, extra: &[&str]) {
    repo.arc(cwd)
        .args(["brief", slug, "--body-file", "-"])
        .args(extra)
        .write_stdin(body)
        .assert()
        .success();
}

struct Fixture {
    repo: Repo,
    /// The artifact the patchset was snapshotted with.
    reference: PathBuf,
    reference_digest: String,
    /// The artifact the change was opened from.
    opening: String,
}

/// A change opened from a journal artifact, briefed, snapshotted with a
/// `--journal-ref`, gated with a declared falsification, self-approved,
/// and integrated under a debt waiver. Nothing is recorded after the
/// integration.
fn integrated_under_waiver() -> Fixture {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[policy]\nforbid_self_approval = true\n",
    )
    .unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.fixable]\ncommand = \"test -f marker\"\ntimeout = \"1m\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", "."]);
    git(&repo.root, &["commit", "-m", "policy and gates"]);

    let opening = file_name(&artifact(
        &repo,
        "todo",
        "framing",
        "# Framing\n\nopen this\n",
    ));
    let reference_body = "# Decision\n\nwhat the work stands on\n";
    let reference = artifact(&repo, "todo", "decision", reference_body);

    stdout(
        repo.arc(&repo.root)
            .args(["begin", WORK, "--from-journal", &opening]),
    );
    let worktree = repo.home.join(".worktrees").join(format!("repo-{WORK}"));
    brief(
        &repo,
        &worktree,
        WORK,
        "build the shipped thing",
        &["--cause-note", "narrowed the framing"],
    );
    repo.arc(&worktree)
        .args([
            "keep",
            WORK,
            "--kind",
            "verified",
            "--body",
            "the gate reads the marker",
            "--evidence",
            "ran test -f marker",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .args([
            "keep",
            WORK,
            "--kind",
            "rejected",
            "--body",
            "a gate that always passes",
        ])
        .assert()
        .success();

    repo.commit(&worktree, "work.txt", "work\n", "feat: work");
    stdout(repo.arc(&worktree).env("ARC_ACTOR", "Solo").args([
        "snapshot",
        WORK,
        "--journal-ref",
        &file_name(&reference),
    ]));
    repo.arc(&worktree)
        .args(["verify", WORK, "--gate", "fixable"])
        .assert()
        .code(1);
    let failing = event_ids(&repo, "verification-recorded").pop().unwrap();
    repo.commit(&worktree, "marker", "", "fix: add marker");
    stdout(repo.arc(&worktree).env("ARC_ACTOR", "Solo").args([
        "snapshot",
        WORK,
        "--journal-ref",
        &file_name(&reference),
    ]));
    repo.arc(&worktree)
        .args([
            "verify",
            WORK,
            "--gate",
            "fixable",
            "--falsified-by",
            &failing,
            "--predicted",
            "marker absent",
        ])
        .assert()
        .success();
    repo.arc(&worktree)
        .env("ARC_ACTOR", "Solo")
        .args(["review", WORK, "--verdict", "approved"])
        .assert()
        .success();
    repo.arc(&repo.root)
        .args(["integrate", WORK, "--debt", "no second reviewer reachable"])
        .assert()
        .success();

    Fixture {
        repo,
        reference_digest: digest(reference_body.as_bytes()),
        reference,
        opening,
    }
}

/// A changes-requested audit by somebody who did not write the change.
fn audit_changes_requested(repo: &Repo, reason: &str) -> String {
    repo.arc(&repo.root)
        .env("ARC_ACTOR", "Auditor")
        .args([
            "audit",
            WORK,
            "--verdict",
            "changes-requested",
            "--body",
            reason,
        ])
        .assert()
        .success();
    event_ids(repo, "audit-verdict-recorded").pop().unwrap()
}

fn explain_json(repo: &Repo, extra: &[&str]) -> serde_json::Value {
    let mut command = repo.arc(&repo.root);
    command.args(["explain", WORK, "--json"]).args(extra);
    json_stdout(&mut command)
}

fn explain_text(repo: &Repo, extra: &[&str]) -> String {
    let mut command = repo.arc(&repo.root);
    command.args(["explain", WORK]).args(extra);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn rows<'a>(explanation: &'a serde_json::Value, slot: &str) -> &'a Vec<serde_json::Value> {
    explanation[slot]["rows"].as_array().unwrap()
}

fn items<'a>(
    explanation: &'a serde_json::Value,
    slot: &str,
    item: &str,
) -> Vec<&'a serde_json::Value> {
    rows(explanation, slot)
        .iter()
        .filter(|row| row["item"] == item)
        .collect()
}

fn reference_rows(explanation: &serde_json::Value) -> Vec<&serde_json::Value> {
    items(explanation, "supplied_context", "patchset-reference")
}

const SLOTS: [(&str, &str); 8] = [
    ("contract", "## Contract"),
    ("supplied_context", "## Supplied context"),
    ("declared_facts", "## Declared facts"),
    ("observed_reads", "## Observed reads"),
    ("rejected_alternatives", "## Rejected alternatives"),
    ("evaluation", "## Evaluation"),
    ("coverage_at_acceptance", "## Coverage at acceptance"),
    ("later_knowledge", "## Later knowledge"),
];

#[test]
fn integrated_change_renders_every_slot() {
    let fixture = integrated_under_waiver();
    let repo = &fixture.repo;
    let audit = audit_changes_requested(repo, "the marker is not the behaviour");
    let integration = event_ids(repo, "change-integrated").pop().unwrap();
    let debt = event_ids(repo, "debt-declared").pop().unwrap();

    let explanation = explain_json(repo, &[]);
    let change_id = explanation["change_id"].as_str().unwrap().to_string();
    let journal_dir = fixture.reference.parent().unwrap().to_path_buf();
    let before = (
        event_count(repo, &change_id),
        journal_event_log(&journal_dir).len(),
    );
    assert_eq!(explanation["schema"], "arc-explain/1");
    assert_eq!(explanation["outcome"], "integrated");
    assert_eq!(explanation["integration_event_id"], integration.as_str());
    for (slot, _) in SLOTS {
        assert!(
            explanation[slot]["rows"].is_array(),
            "slot {slot} is missing: {explanation}"
        );
    }

    // Contract: the brief in force, with its body digest.
    let brief = items(&explanation, "contract", "brief");
    assert_eq!(brief.len(), 1, "{explanation}");
    assert_eq!(brief[0]["standing"], "recorded");
    assert_eq!(brief[0]["version"], 2);
    assert_eq!(
        brief[0]["body_digest"],
        digest(b"build the shipped thing").as_str()
    );
    let cause = items(&explanation, "contract", "cause");
    assert_eq!(cause[0]["standing"], "declared", "{explanation}");
    assert_eq!(cause[0]["cause"]["summary"], "narrowed the framing");
    let base = items(&explanation, "contract", "base-revision");
    assert_eq!(base[0]["standing"], "recorded", "{explanation}");

    // Supplied context: the opening artifact is declared and shown as
    // current; the patchset reference is recorded and unchanged.
    let opening = items(&explanation, "supplied_context", "opening-reference");
    assert_eq!(opening.len(), 1, "{explanation}");
    assert_eq!(opening[0]["file"], fixture.opening.as_str());
    assert_eq!(opening[0]["standing"], "declared");
    assert!(opening[0]["reason"].as_str().unwrap().contains("no digest"));
    assert_eq!(opening[0]["resolution"], "current");
    let references = reference_rows(&explanation);
    assert_eq!(references.len(), 2, "one per patchset: {explanation}");
    for reference in &references {
        assert_eq!(reference["standing"], "recorded");
        assert_eq!(reference["resolution"], "same");
        assert_eq!(
            reference["recorded_digest"],
            fixture.reference_digest.as_str()
        );
    }

    // Declared facts: the fact with evidence names it; the one without is a
    // claim. Nothing kept reads as recorded.
    let facts = rows(&explanation, "declared_facts");
    assert_eq!(facts.len(), 2, "{explanation}");
    assert!(facts.iter().all(|fact| fact["standing"] == "declared"));
    let verified = facts
        .iter()
        .find(|fact| fact["kind"] == "verified")
        .unwrap();
    assert_eq!(verified["basis"], "evidence");
    assert_eq!(verified["evidence"], "ran test -f marker");
    let rejected = facts
        .iter()
        .find(|fact| fact["kind"] == "rejected")
        .unwrap();
    assert_eq!(rejected["basis"], "claim");

    assert!(rows(&explanation, "observed_reads").is_empty());
    assert_eq!(
        explanation["observed_reads"]["absence"]["standing"],
        "absent"
    );
    assert_eq!(
        explanation["observed_reads"]["absence"]["reason"],
        "no tool record"
    );

    let alternatives = rows(&explanation, "rejected_alternatives");
    assert_eq!(alternatives.len(), 1, "{explanation}");
    assert_eq!(alternatives[0]["body"], "a gate that always passes");

    // Evaluation: the counted gate, its tree, timeout, and declared
    // falsification.
    let gates = items(&explanation, "evaluation", "gate");
    assert_eq!(gates.len(), 1, "{explanation}");
    let gate = gates[0];
    assert_eq!(gate["standing"], "recorded");
    assert_eq!(gate["gate"], "fixable");
    assert_eq!(gate["result"], "pass");
    assert!(gate["tree"].as_str().is_some(), "{gate}");
    assert_eq!(gate["timeout_seconds"], 60);
    assert_eq!(gate["falsification"]["standing"], "declared");
    assert_eq!(gate["falsification"]["predicted_reason"], "marker absent");

    // Coverage at acceptance: the waiver the integration used, with the
    // debt's values as of integration, pointing at the later discharge.
    let coverage = &explanation["coverage_at_acceptance"];
    assert!(
        coverage["basis"]
            .as_str()
            .unwrap()
            .contains(integration.as_str()),
        "{coverage}"
    );
    let verdict = items(&explanation, "coverage_at_acceptance", "verdict");
    assert_eq!(verdict[0]["standing"], "recorded", "{coverage}");
    assert_eq!(verdict[0]["verdict"], "approved");
    assert_eq!(verdict[0]["reviewer"], "Solo");
    let waiver = items(&explanation, "coverage_at_acceptance", "debt");
    assert_eq!(waiver.len(), 1, "{coverage}");
    assert_eq!(waiver[0]["event_id"], debt.as_str());
    assert_eq!(waiver[0]["used"], "waiver-used");
    assert!(waiver[0]["missing"].as_str().is_some(), "{coverage}");
    assert!(waiver[0]["production"].is_object(), "{coverage}");
    assert!(
        waiver[0].get("discharged_by").is_none(),
        "the acceptance row keeps its as-of-integration values: {coverage}"
    );
    assert_eq!(waiver[0]["discharged_later_by"], audit.as_str());

    // Later knowledge: the audit, and the discharge it produced, which did
    // not approve.
    let later = rows(&explanation, "later_knowledge");
    let audit_row = later
        .iter()
        .find(|row| row["item"] == "audit-verdict")
        .unwrap();
    assert_eq!(audit_row["event_id"], audit.as_str());
    assert_eq!(audit_row["verdict"], "changes-requested");
    assert_eq!(audit_row["reviewer"], "Auditor");
    let discharge = later
        .iter()
        .find(|row| row["item"] == "debt-discharge")
        .unwrap();
    assert_eq!(discharge["outcome"], "fulfilled, not approved");
    assert_eq!(discharge["by_event_id"], audit.as_str());
    assert_eq!(discharge["debt_event_id"], debt.as_str());
    assert_eq!(
        discharge["standing"], "inferred",
        "no event records a discharge: {discharge}"
    );

    // The text view prints every slot, in order, and the same facts.
    let text = explain_text(repo, &[]);
    let mut from = 0;
    for (_, heading) in SLOTS {
        let at = text[from..]
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} missing or out of order:\n{text}"));
        from += at + heading.len();
    }
    assert!(text.contains("[absent: no tool record]"), "{text}");
    assert!(text.contains("waiver used"), "{text}");
    assert!(text.contains("see later knowledge"), "{text}");
    assert!(text.contains("fulfilled, not approved"), "{text}");
    assert!(
        text.contains(&format!("audit changes-requested `{audit}`")),
        "{text}"
    );
    assert!(
        text.contains("falsification declared: marker absent"),
        "{text}"
    );
    let coverage_text = &text
        [text.find("## Coverage at acceptance").unwrap()..text.find("## Later knowledge").unwrap()];
    assert!(!coverage_text.contains("audit changes-requested"), "{text}");

    // Reading wrote nothing, to the ledger or to the journal.
    let after = (
        event_count(repo, &change_id),
        journal_event_log(&journal_dir).len(),
    );
    assert_eq!(before, after);
}

#[test]
fn amended_reference_renders_both_digests() {
    let fixture = integrated_under_waiver();
    let edited = "# Decision\n\nrewritten after the snapshot\n";
    fs::write(&fixture.reference, edited).unwrap();

    let explanation = explain_json(&fixture.repo, &[]);
    let references = reference_rows(&explanation);
    assert!(!references.is_empty(), "{explanation}");
    for reference in references {
        assert_eq!(reference["standing"], "recorded", "{reference}");
        assert_eq!(reference["resolution"], "amended", "{reference}");
        assert_eq!(
            reference["recorded_digest"],
            fixture.reference_digest.as_str()
        );
        assert_eq!(
            reference["current_digest"],
            digest(edited.as_bytes()).as_str()
        );
    }

    let text = explain_text(&fixture.repo, &[]);
    assert!(
        text.contains(&format!(
            "amended, recorded {}, current {}",
            fixture.reference_digest,
            digest(edited.as_bytes())
        )),
        "{text}"
    );
}

#[test]
fn removed_reference_renders_missing() {
    let fixture = integrated_under_waiver();
    fs::remove_file(&fixture.reference).unwrap();

    let explanation = explain_json(&fixture.repo, &[]);
    let references = reference_rows(&explanation);
    assert!(!references.is_empty(), "{explanation}");
    for reference in references {
        assert_eq!(reference["resolution"], "missing", "{reference}");
        assert_eq!(
            reference["recorded_digest"],
            fixture.reference_digest.as_str()
        );
        assert!(reference.get("current_digest").is_none(), "{reference}");
    }

    let text = explain_text(&fixture.repo, &[]);
    assert!(
        text.contains(&format!(
            "journal ref {}: missing",
            file_name(&fixture.reference)
        )),
        "{text}"
    );
}

#[test]
fn at_bounds_later_knowledge() {
    let fixture = integrated_under_waiver();
    let repo = &fixture.repo;
    let first = audit_changes_requested(repo, "first audit");
    let second = audit_changes_requested(repo, "second audit");
    let integration = event_ids(repo, "change-integrated").pop().unwrap();
    let full = explain_json(repo, &[]);

    // At the integration event nothing later is in view, and the slot says
    // why; coverage at acceptance does not move.
    let at_integration = explain_json(repo, &["--at", &integration]);
    assert_eq!(at_integration["at"], integration.as_str());
    assert!(rows(&at_integration, "later_knowledge").is_empty());
    let absence = &at_integration["later_knowledge"]["absence"];
    assert_eq!(absence["standing"], "absent", "{at_integration}");
    assert!(
        absence["reason"]
            .as_str()
            .unwrap()
            .contains(integration.as_str()),
        "{absence}"
    );
    let text = explain_text(repo, &["--at", &integration]);
    let later_text = &text[text.find("## Later knowledge").unwrap()..];
    assert!(later_text.contains("[absent: "), "{text}");
    assert!(!later_text.contains("audit"), "{text}");
    let waiver = items(&at_integration, "coverage_at_acceptance", "debt");
    let full_waiver = items(&full, "coverage_at_acceptance", "debt");
    assert_eq!(waiver[0]["event_id"], full_waiver[0]["event_id"]);
    assert_eq!(waiver[0]["missing"], full_waiver[0]["missing"]);
    assert!(waiver[0].get("discharged_later_by").is_none(), "{waiver:?}");

    // At the first audit that audit is in view and the second is not.
    let at_first = explain_json(repo, &["--at", &first]);
    let audits: Vec<_> = items(&at_first, "later_knowledge", "audit-verdict")
        .iter()
        .map(|row| row["event_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(audits, vec![first.clone()], "{at_first}");
    let full_audits = items(&full, "later_knowledge", "audit-verdict");
    assert_eq!(full_audits.len(), 2);
    assert!(full_audits
        .iter()
        .any(|row| row["event_id"] == second.as_str()));

    // An event that is not on this change is refused, naming the change.
    let change_id = full["change_id"].as_str().unwrap();
    repo.arc(&repo.root)
        .args(["explain", WORK, "--at", "01ZZZZZZZZZZZZZZZZZZZZZZZZ"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(change_id));
}

#[test]
fn an_open_change_replays_as_of_an_event() {
    let repo = Repo::new();
    let change_id = begin_no_worktree(&repo, "draft", &[]);
    brief(&repo, &repo.root, "draft", "first contract", &[]);
    let first_brief = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "brief-recorded",
    ]));
    let first_brief: serde_json::Value =
        serde_json::from_str(first_brief.lines().next().unwrap()).unwrap();
    let first_brief = first_brief["event_id"].as_str().unwrap().to_string();
    brief(
        &repo,
        &repo.root,
        "draft",
        "second contract",
        &["--cause-note", "revised"],
    );

    let now = json_stdout(repo.arc(&repo.root).args(["explain", "draft", "--json"]));
    assert_eq!(now["outcome"], "open");
    assert_eq!(items(&now, "contract", "brief")[0]["version"], 2);
    assert_eq!(
        now["coverage_at_acceptance"]["absence"]["standing"],
        "absent"
    );
    assert_eq!(now["later_knowledge"]["absence"]["standing"], "absent");

    let then = json_stdout(repo.arc(&repo.root).args([
        "explain",
        "draft",
        "--json",
        "--at",
        &first_brief,
    ]));
    let brief = items(&then, "contract", "brief");
    assert_eq!(brief[0]["version"], 1, "{then}");
    assert_eq!(brief[0]["event_id"], first_brief.as_str());
}
