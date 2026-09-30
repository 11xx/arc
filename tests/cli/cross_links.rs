use super::common::*;

/// Write one journal artifact and return the filename it got.
fn journal_file(repo: &Repo, topic: &str, body: &str) -> String {
    let body_path = repo.home.join(format!("{topic}-body.md"));
    fs::write(&body_path, body).unwrap();
    let out = stdout(repo.arc(&repo.root).args([
        "journal",
        "todo",
        topic,
        "--body-file",
        body_path.to_str().unwrap(),
    ]));
    Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn begin(repo: &Repo, slug: &str) -> String {
    let out = stdout(repo.arc(&repo.root).args(["begin", slug, "--no-worktree"]));
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

#[test]
fn a_patchset_records_its_journal_links_and_thread() {
    let repo = Repo::new();
    let body = "# Framing\n\nwhat this work answers\n";
    let file = journal_file(&repo, "framing", body);
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())));
    let change_id = begin(&repo, "linked");
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args([
            "snapshot",
            "linked",
            "--journal-ref",
            &file,
            "--thread",
            "t3:thread-9",
        ])
        .assert()
        .success();

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id]));
    let patchset = &status["latest_patchset"];
    assert_eq!(patchset["journal_refs"][0]["file"], file, "{status}");
    assert_eq!(patchset["journal_refs"][0]["digest"], digest, "{status}");
    assert_eq!(patchset["thread"]["scheme"], "t3", "{status}");
    assert_eq!(patchset["thread"]["id"], "thread-9", "{status}");

    let show = stdout(repo.arc(&repo.root).args(["show", &change_id]));
    assert!(show.contains(&file), "{show}");
    assert!(show.contains(&digest), "{show}");
    assert!(show.contains("t3:thread-9"), "{show}");

    let log = stdout(repo.arc(&repo.root).args(["log", &change_id]));
    assert!(log.contains("1 journal link(s)"), "{log}");
    assert!(log.contains("thread t3:thread-9"), "{log}");

    // The artifact names the patchsets that cite it.
    let inventory =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "inventory", &file, "--json"]),
        );
    assert_eq!(inventory["schema"], "arc-journal-inventory/6");
    let citation = &inventory["patchset_citations"][file.as_str()][0];
    assert_eq!(citation["change_id"], change_id, "{inventory}");
    assert_eq!(citation["patchset_id"], "ps-01", "{inventory}");
    assert_eq!(citation["digest"], digest, "{inventory}");

    // Re-snapshotting the unchanged head with no links keeps them.
    repo.arc(&repo.root)
        .args(["snapshot", "linked"])
        .assert()
        .success()
        .stdout(predicates::str::contains("(unchanged)"));
    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id]));
    assert_eq!(status["latest_patchset"]["journal_refs"][0]["file"], file);
}

#[test]
fn an_unresolvable_link_refuses_the_snapshot() {
    let repo = Repo::new();
    let change_id = begin(&repo, "unlinked");
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args([
            "snapshot",
            "unlinked",
            "--journal-ref",
            "20260101T000000Z-missing-todo.md",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no such artifact"));
    repo.arc(&repo.root)
        .args(["snapshot", "unlinked", "--thread", "nocolon"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("SCHEME:ID"));

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "patchset-added",
    ]));
    assert!(events.trim().is_empty(), "{events}");
}

#[test]
fn done_records_the_links_on_the_patchset_it_snapshots() {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/gates.toml"),
        "[gates.build]\ncommand = \"true\"\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/gates.toml"]);
    git(&repo.root, &["commit", "-m", "test: declare a gate"]);
    let file = journal_file(&repo, "done-framing", "# Framing the done run\n");
    let change_id = begin(&repo, "done-links");
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args([
            "done",
            "done-links",
            "--journal-ref",
            &file,
            "--thread",
            "t3:done-1",
        ])
        .assert()
        .code(3);

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id]));
    assert_eq!(status["latest_patchset"]["journal_refs"][0]["file"], file);
    assert_eq!(status["latest_patchset"]["thread"]["id"], "done-1");
}

#[test]
fn exported_changes_carry_their_cross_links() {
    let repo = Repo::new();
    let file = journal_file(&repo, "exported-framing", "# Framing across machines\n");
    let change_id = begin(&repo, "exported-links");
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");
    repo.arc(&repo.root)
        .args([
            "snapshot",
            "exported-links",
            "--journal-ref",
            &file,
            "--thread",
            "t3:exported-1",
        ])
        .assert()
        .success();

    let bundle = repo.home.join("linked.json");
    repo.arc(&repo.root)
        .args(["export", &change_id, "--output", bundle.to_str().unwrap()])
        .assert()
        .success();
    let text = fs::read_to_string(&bundle).unwrap();
    assert!(text.contains(&file), "{text}");
    assert!(text.contains("exported-1"), "{text}");

    let other = Repo::new();
    other
        .arc(&other.root)
        .args(["import", bundle.to_str().unwrap()])
        .assert()
        .success();
    let events = stdout(other.arc(&other.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "patchset-added",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(event["journal_refs"][0]["file"], file, "{event}");
    assert_eq!(event["journal_refs"][0]["via"], "flag", "{event}");
    assert_eq!(event["thread"]["scheme"], "t3", "{event}");
    assert_eq!(event["thread"]["id"], "exported-1", "{event}");
}

#[test]
fn done_without_declared_gates_still_records_the_links() {
    let repo = Repo::new();
    let file = journal_file(&repo, "gateless-framing", "# Framing a gateless run\n");
    let change_id = begin(&repo, "gateless-links");
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    // With no declared gate there is nothing to run, and `done` says so. The
    // links are still part of the patchset it snapshots.
    repo.arc(&repo.root)
        .args([
            "done",
            "gateless-links",
            "--journal-ref",
            &file,
            "--thread",
            "t3:gateless-1",
        ])
        .assert()
        .code(3)
        .stdout(predicates::str::contains(
            "no gates declared for profile local; nothing was run",
        ));

    let status = json_stdout(repo.arc(&repo.root).args(["status", &change_id]));
    assert_eq!(status["latest_patchset"]["journal_refs"][0]["file"], file);
    assert_eq!(status["latest_patchset"]["thread"]["scheme"], "t3");
    assert_eq!(status["latest_patchset"]["thread"]["id"], "gateless-1");
}

fn digest_of(body: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(body.as_bytes())))
}

/// Write one plan artifact and return the filename it got.
fn plan_file(repo: &Repo, topic: &str, body: &str) -> String {
    let out = stdout(
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
            .write_stdin(body),
    );
    Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string()
}

fn begin_from(repo: &Repo, slug: &str, file: &str) -> String {
    let out =
        stdout(
            repo.arc(&repo.root)
                .args(["begin", slug, "--no-worktree", "--from-journal", file]),
        );
    out.lines()
        .find_map(|line| line.strip_prefix("change: "))
        .unwrap()
        .to_string()
}

/// Record a brief naming `plan`. A change opened from a non-plan artifact
/// already holds a brief seeded from it, so the next version names a cause.
/// The digest a link to `file` records while its body is what the hot journal
/// holds now.
fn hot_digest(repo: &Repo, file: &str) -> String {
    let dir = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    digest_of(&fs::read_to_string(Path::new(dir.trim()).join(file)).unwrap())
}

fn brief_with_plan(repo: &Repo, slug: &str, plan: &str, revision: bool) {
    let mut command = repo.arc(&repo.root);
    if revision {
        command.args(["brief", slug, "--cause-note", "fixture revision"]);
    } else {
        command.args(["brief", slug]);
    }
    command
        .args([
            "--body-file",
            "-",
            "--plan-ref",
            plan,
            "--plan-slice",
            "slice",
        ])
        .write_stdin("# Contract\n")
        .assert()
        .success();
}

fn latest_links(repo: &Repo, change_id: &str) -> Vec<serde_json::Value> {
    let status = json_stdout(repo.arc(&repo.root).args(["status", change_id]));
    status["latest_patchset"]["journal_refs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn begin_from_journal_records_the_opening_digest() {
    let repo = Repo::new();
    let body = "# Opening\n\nthe work this change answers\n";
    let file = journal_file(&repo, "opening", body);
    let change_id = begin_from(&repo, "opened", &file);

    let events = stdout(repo.arc(&repo.root).args([
        "events",
        "--change",
        &change_id,
        "--type",
        "change-opened",
    ]));
    let event: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(event["journal_ref"], file, "{event}");
    assert_eq!(event["journal_ref_digest"], digest_of(body), "{event}");

    let state = json_stdout(repo.arc(&repo.root).args(["show", &change_id, "--json"]));
    assert_eq!(state["schema"], "arc-state/3", "{state}");
    assert_eq!(state["journal_ref_digest"], digest_of(body), "{state}");
    let show = stdout(repo.arc(&repo.root).args(["show", &change_id]));
    assert!(
        show.contains(&format!("- Opened from: `{file}` ({})", digest_of(body))),
        "{show}"
    );
}

#[test]
fn snapshot_carries_opening_and_plan_references_by_default() {
    let repo = Repo::new();
    let opening_body = "# Opening\n\nwhy this change exists\n";
    let plan_body = "# Plan\n\nhow the work is sliced\n";
    let opening = journal_file(&repo, "opening", opening_body);
    let plan = plan_file(&repo, "slicing", plan_body);
    let change_id = begin_from(&repo, "defaulted", &opening);
    brief_with_plan(&repo, "defaulted", &plan, true);
    let plan_digest = hot_digest(&repo, &plan);
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args(["snapshot", "defaulted"])
        .assert()
        .success()
        .stderr(predicates::str::contains("warning").not());

    let links = latest_links(&repo, &change_id);
    assert_eq!(links.len(), 2, "{links:?}");
    assert_eq!(links[0]["file"], opening, "{links:?}");
    assert_eq!(links[0]["digest"], digest_of(opening_body), "{links:?}");
    assert_eq!(links[0]["via"], "begin", "{links:?}");
    assert_eq!(links[1]["file"], plan, "{links:?}");
    assert_eq!(links[1]["digest"], plan_digest, "{links:?}");
    assert_eq!(links[1]["via"], "brief", "{links:?}");

    let show = stdout(repo.arc(&repo.root).args(["show", &change_id]));
    assert!(
        show.contains(&format!(
            "`{opening}` ({}), via begin",
            digest_of(opening_body)
        )),
        "{show}"
    );
    assert!(
        show.contains(&format!("`{plan}` ({plan_digest}), via brief")),
        "{show}"
    );
    let log = stdout(repo.arc(&repo.root).args(["log", &change_id]));
    assert!(log.contains("2 journal link(s) via begin, brief"), "{log}");
    let inventory =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "inventory", &plan, "--json"]),
        );
    assert_eq!(
        inventory["patchset_citations"][plan.as_str()][0]["via"],
        "brief",
        "{inventory}"
    );

    // A bare rerun at the same head keeps the patchset it already recorded.
    repo.arc(&repo.root)
        .args(["snapshot", "defaulted"])
        .assert()
        .success()
        .stdout(predicates::str::contains("(unchanged)"));
}

#[test]
fn a_file_framing_both_opening_and_brief_is_linked_once_as_begin() {
    let repo = Repo::new();
    let body = "# Plan\n\npromoted and briefed from itself\n";
    let plan = plan_file(&repo, "self-briefed", body);
    let change_id = begin_from(&repo, "self-briefed", &plan);
    brief_with_plan(&repo, "self-briefed", &plan, false);
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args(["snapshot", "self-briefed"])
        .assert()
        .success();

    let links = latest_links(&repo, &change_id);
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["file"], plan, "{links:?}");
    assert_eq!(links[0]["via"], "begin", "{links:?}");
}

#[test]
fn archived_plan_reference_still_resolves() {
    let repo = Repo::new();
    let plan_body = "# Plan\n\narchived before the snapshot\n";
    let plan = plan_file(&repo, "archived", plan_body);
    let change_id = begin(&repo, "archived-plan");
    brief_with_plan(&repo, "archived-plan", &plan, false);
    repo.arc(&repo.root)
        .args(["journal", "consume", &plan])
        .assert()
        .success();
    let plan_digest = hot_digest(&repo, &plan);
    repo.arc(&repo.root)
        .args(["journal", "archive", &plan])
        .assert()
        .success();
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args(["snapshot", "archived-plan"])
        .assert()
        .success()
        .stderr(predicates::str::contains("warning").not());

    let links = latest_links(&repo, &change_id);
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["file"], plan, "{links:?}");
    assert_eq!(links[0]["digest"], plan_digest, "{links:?}");
    assert_eq!(links[0]["via"], "brief", "{links:?}");
}

#[test]
fn deleted_default_reference_is_skipped_with_a_warning() {
    let repo = Repo::new();
    let opening = journal_file(&repo, "vanished", "# Opening\n\ndeleted later\n");
    let plan_body = "# Plan\n\nstill here\n";
    let plan = plan_file(&repo, "surviving", plan_body);
    let change_id = begin_from(&repo, "vanished", &opening);
    brief_with_plan(&repo, "vanished", &plan, true);
    let plan_digest = hot_digest(&repo, &plan);
    let journal_dir = stdout(repo.arc(&repo.root).args(["journal", "dir"]));
    fs::remove_file(Path::new(journal_dir.trim()).join(&opening)).unwrap();
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    let output = repo
        .arc(&repo.root)
        .args(["snapshot", "vanished"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let warnings = stderr
        .lines()
        .filter(|line| line.contains(&opening))
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1, "{stderr}");
    assert!(warnings[0].contains("begin --from-journal"), "{stderr}");

    let links = latest_links(&repo, &change_id);
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["file"], plan, "{links:?}");
    assert_eq!(links[0]["digest"], plan_digest, "{links:?}");
    assert_eq!(links[0]["via"], "brief", "{links:?}");
}

#[test]
fn explicit_journal_ref_records_only_flagged_references() {
    let repo = Repo::new();
    let opening = journal_file(&repo, "opening", "# Opening\n");
    let plan = plan_file(&repo, "plan", "# Plan\n");
    let flagged_body = "# Flagged\n\nthe one the caller names\n";
    let flagged = journal_file(&repo, "flagged", flagged_body);
    let change_id = begin_from(&repo, "flagged", &opening);
    brief_with_plan(&repo, "flagged", &plan, true);
    repo.commit(&repo.root, "work.txt", "work\n", "test: work");

    repo.arc(&repo.root)
        .args(["snapshot", "flagged", "--journal-ref", &flagged])
        .assert()
        .success();

    let links = latest_links(&repo, &change_id);
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0]["file"], flagged, "{links:?}");
    assert_eq!(links[0]["digest"], digest_of(flagged_body), "{links:?}");
    assert_eq!(links[0]["via"], "flag", "{links:?}");

    // An explicit name that resolves to nothing stays a refusal.
    repo.arc(&repo.root)
        .args([
            "snapshot",
            "flagged",
            "--journal-ref",
            "20260101T000000Z-missing-todo.md",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no such artifact"));
}
