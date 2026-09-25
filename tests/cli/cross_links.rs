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
    assert_eq!(inventory["schema"], "arc-journal-inventory/5");
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
    assert_eq!(event["thread"]["scheme"], "t3", "{event}");
    assert_eq!(event["thread"]["id"], "exported-1", "{event}");
}
