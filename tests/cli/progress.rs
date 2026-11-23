use super::common::*;

/// Start a series in `cwd` with `items`, returning its filename.
fn start(repo: &Repo, cwd: &Path, topic: &str, items: &[&str]) -> String {
    let mut command = repo.arc(cwd);
    command.args(["progress", "start", topic, "--title", topic]);
    for item in items {
        command.args(["--item", item]);
    }
    stdout(&mut command).lines().next().unwrap().to_string()
}

fn progress(repo: &Repo, cwd: &Path, args: &[&str]) -> String {
    stdout(repo.arc(cwd).arg("progress").args(args))
}

fn report(repo: &Repo, cwd: &Path, args: &[&str]) -> serde_json::Value {
    let value = json_stdout(repo.arc(cwd).arg("progress").args(args).arg("--json"));
    assert_eq!(value["schema"], "arc-progress/1");
    value
}

/// The one series a report holds under `file`.
fn series<'a>(report: &'a serde_json::Value, file: &str) -> &'a serde_json::Value {
    report["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|series| series["file"] == file)
        .unwrap_or_else(|| panic!("no series {file} in {report}"))
}

fn stage(repo: &Repo, file: &str, item: &str) -> serde_json::Value {
    let report = report(repo, &repo.root, &[file]);
    series(&report, file)["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == item)
        .unwrap()
        .clone()
}

fn journal_dir(repo: &Repo, cwd: &Path) -> PathBuf {
    PathBuf::from(stdout(repo.arc(cwd).args(["journal", "dir"])).trim())
}

fn item_events(dir: &Path) -> usize {
    journal_event_log(dir)
        .iter()
        .filter(|event| {
            event["event"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("item-") || kind == "series-related")
        })
        .count()
}

fn sibling_project(repo: &Repo, name: &str) -> PathBuf {
    let root = repo.root.parent().unwrap().join(name);
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-b", "master"]);
    git(&root, &["config", "user.name", "Tester"]);
    git(&root, &["config", "user.email", "tester@example.invalid"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("README.md"), "other\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "init"]);
    fs::canonicalize(root).unwrap()
}

/// Claim: a linked item's stage is what its change's events record, read
/// afresh at every report with no progress write after the link: `opened`,
/// `snapshotted` once a patchset is recorded, `verified` once a pass is
/// recorded at it, `reviewed` with the verdict and the rounds that asked for
/// changes, back to `snapshotted` on a new patchset, and `integrated` with
/// its time and revision. A failure recorded after a pass at the same
/// patchset takes the stage back to `snapshotted`. Rejects a stage written
/// rather than recorded, one that stops following the change, and one that
/// keeps an earlier pass after a later recorded failure.
#[test]
fn a_linked_items_stage_follows_its_change_without_a_progress_write() {
    let repo = Repo::new();
    let flag = repo.home.join("gate-fails");
    repo.redeclare_gates(&format!(
        "[gates.unit]\ncommand = \"test ! -e {}\"\n",
        flag.display()
    ));
    let file = start(
        &repo,
        &repo.root,
        "ship",
        &["Wire the reader", "Fails after passing"],
    );
    let begun = stdout(
        repo.arc(&repo.root)
            .args(["begin", "reader", "--item", "ship:i1"]),
    );
    let change = opened_change_id(&begun);
    let dir = journal_dir(&repo, &repo.root);
    let writes = item_events(&dir);
    assert_eq!(stage(&repo, &file, "i1")["change"], change.as_str());
    assert_eq!(stage(&repo, &file, "i1")["stage"], "opened");

    let worktree = repo.home.join(".worktrees").join("repo-reader");
    repo.commit(&worktree, "reader.txt", "one\n", "feat: reader");
    assert_eq!(
        stage(&repo, &file, "i1")["stage"],
        "opened",
        "a commit is not a ledger record"
    );

    stdout(repo.arc(&worktree).args(["snapshot", "reader"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "snapshotted");

    stdout(repo.arc(&worktree).args(["verify", "reader", "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "verified");

    stdout(repo.arc(&worktree).args([
        "review",
        "reader",
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
    ]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "reviewed");
    assert_eq!(row["verdict"], "changes-requested");
    assert_eq!(row["changes_requested_rounds"], 1);

    repo.commit(&worktree, "reader.txt", "two\n", "fix: reader");
    stdout(repo.arc(&worktree).args(["snapshot", "reader"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "snapshotted");
    assert_eq!(row["changes_requested_rounds"], 1);

    stdout(repo.arc(&worktree).args(["verify", "reader", "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "verified");
    stdout(
        repo.arc(&worktree)
            .args(["review", "reader", "--verdict", "approved"]),
    );
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "reviewed");
    assert_eq!(row["verdict"], "approved");

    stdout(repo.arc(&repo.root).args(["integrate", "reader"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "integrated");
    assert!(row["integrated_at"].is_string(), "{row}");
    assert_eq!(row["revision"], repo.head(&repo.root).as_str());
    let report = report(&repo, &repo.root, &[]);
    let counts = &series(&report, &file)["counts"];
    assert_eq!(counts["integrated"], 1);
    assert_eq!(counts["complete"], 1);
    assert_eq!(counts["percent"], 50);
    assert_eq!(series(&report, &file)["last_integration"]["item"], "i1");

    stdout(
        repo.arc(&repo.root)
            .args(["begin", "flaky", "--item", "ship:i2"]),
    );
    let worktree = repo.home.join(".worktrees").join("repo-flaky");
    repo.commit(&worktree, "flaky.txt", "one\n", "feat: flaky");
    stdout(repo.arc(&worktree).args(["snapshot", "flaky"]));
    stdout(repo.arc(&worktree).args(["verify", "flaky", "--all"]));
    assert_eq!(stage(&repo, &file, "i2")["stage"], "verified");
    fs::write(&flag, "").unwrap();
    stdout_any_status(
        repo.arc(&worktree)
            .args(["verify", "flaky", "--all", "--rerun"]),
    );
    assert_eq!(
        stage(&repo, &file, "i2")["stage"],
        "snapshotted",
        "a pass outlived the failure recorded after it"
    );

    assert_eq!(
        item_events(&dir),
        writes + 1,
        "the stage moved through a progress write rather than the ledger"
    );
}

/// Claim: add, rename, drop, split and move each append one event and leave
/// every earlier event intact, and the report shows the list as it stands,
/// in its order, counted from the items that stand and from what changed
/// since the start. Rejects an edit that rewrites earlier events or a count
/// taken from the list as first declared.
#[test]
fn list_edits_keep_earlier_events_and_the_report_counts_the_list_as_it_stands() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "edits", &["One", "Two", "Three"]);
    let dir = journal_dir(&repo, &repo.root);
    let edits: [&[&str]; 6] = [
        &["add", "edits", "Zero", "--before", "i1"],
        &["rename", "edits", "i2", "Two, renamed"],
        &["drop", "edits", "i3", "--reason", "folded into i2"],
        &[
            "split",
            "edits",
            "i1",
            "--into",
            "One a",
            "--into",
            "One b",
            "--reason",
            "two parts",
        ],
        &["move", "edits", "i4", "--after", "i2"],
        &["done", "edits", "i5", "--note", "measured"],
    ];
    let mut log = journal_event_log(&dir);
    for edit in edits {
        progress(&repo, &repo.root, edit);
        let next = journal_event_log(&dir);
        assert_eq!(
            next.len(),
            log.len() + 1,
            "{edit:?} did not append one event"
        );
        assert_eq!(
            next[..log.len()],
            log[..],
            "{edit:?} rewrote an earlier event"
        );
        log = next;
    }

    let report = report(&repo, &repo.root, &[]);
    let series = series(&report, &file);
    let listed: Vec<(String, String, String)> = series["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap().to_string(),
                row["title"].as_str().unwrap().to_string(),
                row["stage"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected = [
        ("i1", "One", "split"),
        ("i5", "One a", "done"),
        ("i6", "One b", "open"),
        ("i2", "Two, renamed", "open"),
        ("i4", "Zero", "open"),
        ("i3", "Three", "dropped"),
    ];
    assert_eq!(
        listed,
        expected
            .iter()
            .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
            .collect::<Vec<_>>()
    );
    let counts = &series["counts"];
    assert_eq!(counts["items"], 4, "{counts}");
    assert_eq!(counts["complete"], 1);
    assert_eq!(counts["open"], 3);
    assert_eq!(counts["percent"], 25);
    assert_eq!(counts["since_start"]["declared"], 3);
    assert_eq!(counts["since_start"]["added"], 1);
    assert_eq!(counts["since_start"]["dropped"], 1);
    assert_eq!(counts["since_start"]["split"], 1);

    let text = progress(&repo, &repo.root, &["--plain"]);
    assert!(
        text.contains("4 items; 3 declared at the start; since then 1 added, 1 dropped, 1 split"),
        "{text}"
    );
}

/// Claim: a dropped item leaves the share complete and stays visible with
/// its reason. Rejects a dropped item counted against the share, and one
/// that disappears from the list.
#[test]
fn a_dropped_item_leaves_the_share_and_stays_visible() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "share", &["Keep", "Lose"]);
    progress(&repo, &repo.root, &["done", "share", "i1"]);
    progress(
        &repo,
        &repo.root,
        &["drop", "share", "i2", "--reason", "out of scope"],
    );
    let report = report(&repo, &repo.root, &[]);
    let series = series(&report, &file);
    assert_eq!(series["counts"]["items"], 1);
    assert_eq!(series["counts"]["complete"], 1);
    assert_eq!(series["counts"]["percent"], 100);
    assert_eq!(series["items"][1]["stage"], "dropped");
    assert_eq!(series["items"][1]["note"], "out of scope");
    let text = progress(&repo, &repo.root, &["--plain"]);
    assert!(text.contains("1 of 1 complete"), "{text}");
    assert!(text.contains("dropped  Lose  [out of scope]"), "{text}");
}

/// Every RFC 3339 timestamp in `text` replaced by `<ts>`: when the session
/// last wrote is a clock fact every journal write moves, not a line of
/// progress.
fn without_timestamps(text: &str) -> String {
    let bytes = text.as_bytes();
    let stamp = |at: usize| {
        bytes.len() >= at + 19
            && bytes[at..at + 19]
                .iter()
                .enumerate()
                .all(|(index, byte)| match index {
                    4 | 7 => *byte == b'-',
                    10 => *byte == b'T',
                    13 | 16 => *byte == b':',
                    _ => byte.is_ascii_digit(),
                })
    };
    let mut out = String::new();
    let mut at = 0;
    while at < bytes.len() {
        if stamp(at) {
            out.push_str("<ts>");
            at += 19;
            while at < bytes.len() && !matches!(bytes[at], b' ' | b'"' | b',' | b'\n') {
                at += 1;
            }
        } else {
            let next = text[at..].chars().next().unwrap();
            out.push(next);
            at += next.len_utf8();
        }
    }
    out
}

/// Claim: progress is pulled, never pushed: `arc status`, `arc check` and
/// `arc catchup` print byte for byte the same, timestamps aside, before and
/// after a series' items are added, linked to the change those commands
/// read, marked done and dropped. The series is started before the first
/// read, since filing any artifact is session activity catchup reports for
/// every kind. Rejects any of them carrying a line of progress.
#[test]
fn progress_prints_nothing_into_status_check_or_catchup() {
    let repo = Repo::new();
    let change = begin_change(&repo, "quiet", None);
    start(&repo, &repo.root, "quiet-series", &[]);
    let reads = |repo: &Repo| -> Vec<String> {
        [
            vec!["status", change.as_str()],
            vec!["status", change.as_str(), "--json"],
            vec!["check", change.as_str()],
            vec!["catchup"],
            vec!["catchup", "--json"],
        ]
        .iter()
        .map(|args| {
            without_timestamps(&without_clock(&stdout_any_status(
                repo.arc(&repo.root).args(args),
            )))
        })
        .collect()
    };
    let before = reads(&repo);

    for title in ["Linked", "Free", "Gone"] {
        progress(&repo, &repo.root, &["add", "quiet-series", title]);
    }
    progress(&repo, &repo.root, &["link", "quiet-series", "i1", &change]);
    progress(&repo, &repo.root, &["done", "quiet-series", "i2"]);
    progress(
        &repo,
        &repo.root,
        &["drop", "quiet-series", "i3", "--reason", "not needed"],
    );
    let after = reads(&repo);
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(
            before, after,
            "a read carried progress it was not asked for"
        );
    }
}

/// Claim: with several active series in one project, bare `arc progress`
/// prints every one in full under its own header line, and each series'
/// counts are its own. Rejects a summary line in place of a series, a
/// series left out, and counts that mix series.
#[test]
fn two_active_series_print_in_full_with_independent_counts() {
    let repo = Repo::new();
    let first = start(&repo, &repo.root, "first", &["A", "B"]);
    let second = start(&repo, &repo.root, "second", &["C", "D", "E"]);
    progress(&repo, &repo.root, &["done", "first", "i1"]);
    progress(&repo, &repo.root, &["done", "second", "i3"]);
    progress(&repo, &repo.root, &["done", "second", "i2"]);

    let report = report(&repo, &repo.root, &[]);
    assert_eq!(report["series"].as_array().unwrap().len(), 2);
    let one = series(&report, &first);
    let two = series(&report, &second);
    assert_eq!(
        (
            one["counts"]["complete"].clone(),
            one["counts"]["items"].clone()
        ),
        (1.into(), 2.into())
    );
    assert_eq!(
        (
            two["counts"]["complete"].clone(),
            two["counts"]["items"].clone()
        ),
        (2.into(), 3.into())
    );

    let text = progress(&repo, &repo.root, &["--plain"]);
    for header in [
        format!("== first ({first})"),
        format!("== second ({second})"),
    ] {
        assert!(text.contains(&header), "{text}");
    }
    for title in ["A", "B", "C", "D", "E"] {
        assert!(
            text.lines()
                .any(|line| line.trim_end().ends_with(&format!(" {title}"))),
            "item {title} missing:\n{text}"
        );
    }
    assert!(text.contains("1 of 2 complete"), "{text}");
    assert!(text.contains("2 of 3 complete"), "{text}");
}

/// Claim: a relation made in one project is recorded only there and shows
/// as one-sided, and becomes mutual once the same call is made in the other
/// project. Rejects a relation reported mutual on one call, or one-sided
/// after both.
#[test]
fn a_relation_is_one_sided_until_made_in_the_other_project() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    let here = start(&repo, &repo.root, "here", &["Local"]);
    let there = start(&repo, &other, "there", &["Remote"]);
    let label = repo.root.file_name().unwrap().to_string_lossy().to_string();

    let related = progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    assert!(related.contains("one-sided"), "{related}");
    let report_here = report(&repo, &repo.root, &[]);
    let relation = &series(&report_here, &here)["related"][0];
    assert_eq!(relation["file"], there.as_str());
    assert_eq!(relation["project"], "other");
    assert_eq!(relation["mutual"], false);
    assert!(progress(&repo, &repo.root, &["--plain"]).contains("(one-sided)"));
    assert_eq!(
        series(&report(&repo, &other, &[]), &there)["related"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "the relation was written into the other project's journal"
    );

    let back = progress(
        &repo,
        &other,
        &["relate", "there", &format!("{label}::here")],
    );
    assert!(back.contains("(mutual)"), "{back}");
    let mutual = report(&repo, &repo.root, &[]);
    assert_eq!(series(&mutual, &here)["related"][0]["mutual"], true);
    let text = progress(&repo, &repo.root, &["--plain"]);
    let header = text
        .lines()
        .position(|line| line.starts_with("== here"))
        .unwrap();
    assert_eq!(
        text.lines().nth(header + 1).unwrap(),
        format!("related: other::{there} (mutual)")
    );
}

/// Claim: bare `arc progress` prints each related series of another project
/// after the local ones as a compact block marked with its project, read
/// from that project's journal and ledger; when that journal cannot be read
/// it prints one line and exits 0. Rejects a related series left out, one
/// printed without its project, and a failure that fails the report.
#[test]
fn a_related_series_prints_compact_and_an_unreadable_one_prints_one_line() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &repo.root, "here", &["Local"]);
    let there = start(
        &repo,
        &other,
        "there",
        &["Remote one", "Remote two", "Remote three"],
    );
    progress(&repo, &other, &["done", "there", "i1"]);
    let change = begin_change_in(&repo, &other, "remote-work");
    progress(&repo, &other, &["link", "there", "i2", &change]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);

    let text = progress(&repo, &repo.root, &["--plain"]);
    let block = text
        .find(&format!("-- other: there ({there})"))
        .unwrap_or_else(|| panic!("{text}"));
    let local = text.find("== here").unwrap();
    assert!(local < block, "{text}");
    let compact = &text[block..];
    assert!(compact.contains("1 of 3 complete"), "{compact}");
    assert!(compact.contains("1 in flight"), "{compact}");
    assert!(compact.contains("i2   opened  Remote two"), "{compact}");
    assert!(!compact.contains("Remote three"), "{compact}");
    let json = report(&repo, &repo.root, &[]);
    assert_eq!(json["related"][0]["project"], "other");
    assert_eq!(json["related"][0]["in_flight"][0]["id"], "i2");

    let other_journal = journal_dir(&repo, &other);
    fs::remove_dir_all(&other_journal).unwrap();
    let output = repo
        .arc(&repo.root)
        .args(["progress", "--plain"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    let unavailable: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("unavailable"))
        .collect();
    assert_eq!(unavailable.len(), 1, "{text}");
    assert!(
        unavailable[0].starts_with("related ") && unavailable[0].contains(&format!("::{there}")),
        "{text}"
    );
}

fn begin_change_in(repo: &Repo, cwd: &Path, slug: &str) -> String {
    let branch = format!("arc/{slug}");
    git(cwd, &["branch", &branch, "HEAD"]);
    opened_change_id(&stdout(
        repo.arc(cwd).args(["begin", slug, "--adopt", &branch]),
    ))
}

/// Claim: retiring a series by its conclusion records it consumed as
/// superseded with the conclusion named as what resolved it, which the
/// journal's library shows as its successor; removes it from the default
/// listing, keeps it readable by name, and refuses further edits. Rejects a
/// retired series still listed, one unreadable by name, and a retirement
/// whose successor the journal's own readers do not show.
#[test]
fn retiring_a_series_by_its_conclusion_leaves_the_listing_and_records_the_successor() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "finish", &["Only"]);
    let kept = start(&repo, &repo.root, "ongoing", &["Still"]);
    let (_, conclusion) = journal_artifact(&repo, "finish", "conclusion", "# Finished\n");
    let retired = progress(
        &repo,
        &repo.root,
        &["retire", "finish", "--by", &conclusion],
    );
    assert!(retired.contains(&conclusion), "{retired}");

    let dir = journal_dir(&repo, &repo.root);
    let consumed = journal_event_log(&dir)
        .into_iter()
        .rev()
        .find(|event| event["event"] == "consumed")
        .unwrap();
    assert_eq!(consumed["file"], file.as_str());
    assert_eq!(consumed["outcome"], "superseded");
    assert_eq!(consumed["decision"], conclusion.as_str());
    assert_eq!(consumed["decision_kind"], "conclusion");

    let listing = report(&repo, &repo.root, &[]);
    let files: Vec<&str> = listing["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|series| series["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, vec![kept.as_str()]);
    let named = report(&repo, &repo.root, &[&file]);
    assert_eq!(series(&named, &file)["retired"]["by"], conclusion.as_str());
    repo.arc(&repo.root)
        .args(["progress", "add", &file, "Late"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("retired"));

    let library =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--library", "--json"]),
        );
    let row = library["projects"][0]["library"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["file"] == file)
        .unwrap()
        .clone();
    assert_eq!(row["status"], "superseded", "{row}");
    assert_eq!(row["superseded_by"], conclusion.as_str(), "{row}");
}

/// Claim: `arc progress --json` carries the `arc-progress/1` shape the guide
/// documents, for several series with relations and every item kind: each
/// field present with its type. Rejects a report missing a documented field
/// or carrying one of the wrong type.
#[test]
fn progress_json_has_the_arc_progress_1_shape() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &other, "there", &["Remote"]);
    start(
        &repo,
        &repo.root,
        "one",
        &["Open", "Done", "Dropped", "Split"],
    );
    start(&repo, &repo.root, "two", &[]);
    let change = begin_change(&repo, "shape", None);
    progress(&repo, &repo.root, &["link", "one", "i1", &change]);
    progress(&repo, &repo.root, &["done", "one", "i2"]);
    progress(&repo, &repo.root, &["drop", "one", "i3", "--reason", "r"]);
    progress(
        &repo,
        &repo.root,
        &[
            "split", "one", "i4", "--into", "a", "--into", "b", "--reason", "r",
        ],
    );
    progress(&repo, &repo.root, &["relate", "one", "other::there"]);

    let report = report(&repo, &repo.root, &[]);
    let is = |value: &serde_json::Value, what: &str| match what {
        "string" => value.is_string(),
        "number" => value.is_u64(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "bool" => value.is_boolean(),
        _ => unreachable!(),
    };
    let fields = |value: &serde_json::Value, expected: &[(&str, &str)]| {
        for (name, kind) in expected {
            assert!(is(&value[name], kind), "{name} is not a {kind}: {value}");
        }
    };
    fields(
        &report,
        &[
            ("schema", "string"),
            ("journal_dir", "string"),
            ("series", "array"),
            ("related", "array"),
            ("waiting_on_person", "array"),
        ],
    );
    assert_eq!(report["series"].as_array().unwrap().len(), 2);
    for series in report["series"].as_array().unwrap() {
        fields(
            series,
            &[
                ("file", "string"),
                ("title", "string"),
                ("started_at", "string"),
                ("related", "array"),
                ("items", "array"),
                ("counts", "object"),
            ],
        );
        fields(
            &series["counts"],
            &[
                ("items", "number"),
                ("complete", "number"),
                ("integrated", "number"),
                ("done", "number"),
                ("in_flight", "number"),
                ("open", "number"),
                ("closed", "number"),
                ("since_start", "object"),
            ],
        );
        for row in series["items"].as_array().unwrap() {
            fields(
                row,
                &[("id", "string"), ("title", "string"), ("stage", "string")],
            );
        }
    }
    let one = &report["series"][0];
    fields(
        &one["related"][0],
        &[
            ("project", "string"),
            ("slug", "string"),
            ("file", "string"),
            ("mutual", "bool"),
        ],
    );
    fields(&one["items"][0], &[("change", "string")]);
    fields(&one["items"][2], &[("note", "string")]);
    fields(&one["items"][3], &[("split_into", "array")]);
    fields(&one["counts"], &[("percent", "number")]);
    fields(
        &report["related"][0],
        &[
            ("project", "string"),
            ("slug", "string"),
            ("file", "string"),
            ("related_from", "array"),
            ("title", "string"),
            ("counts", "object"),
            ("in_flight", "array"),
        ],
    );
}

/// The `arc-perf/1` record `ARC_TRACE` writes for one `arc progress`.
fn traced_progress(repo: &Repo) -> serde_json::Value {
    let output = repo
        .arc(&repo.root)
        .env("ARC_TRACE", "1")
        .arg("progress")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr
        .lines()
        .find(|line| line.contains("\"arc-perf/1\""))
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap_or_else(|| panic!("no trace record: {stderr}"))
}

/// What one report read: change ledgers opened and decoded, changes
/// reduced, and Git processes started.
fn reads(record: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "change_files_opened": record["ledger"]["change"]["opened"],
        "change_events_decoded": record["ledger"]["change"]["decoded"],
        "changes_reduced": record["counts"]["changes_reduced"],
        "git": record["children"]["runs"],
    })
}

/// Claim: a report reads the ledger only for its linked items: a second
/// change that no item links, however many events it holds, changes neither
/// the ledger files the report opens and decodes, the changes it reduces,
/// nor the Git processes it starts. An item linked into another project
/// adds that change's reduction and at most the two Git processes that open
/// one more ledger; a second item linked into the same project adds its
/// change's reduction and no Git process, and that project's unlinked
/// changes are not read either. Rejects a report whose cost follows the size
/// of any ledger rather than its own items, and one whose Git processes
/// grow with the items linked into one project.
#[test]
fn a_report_reads_only_the_changes_its_items_link() {
    let repo = Repo::new();
    start(
        &repo,
        &repo.root,
        "bounded",
        &["Linked", "Free", "Remote", "Remote too"],
    );
    let linked = begin_change(&repo, "linked", None);
    progress(&repo, &repo.root, &["link", "bounded", "i1", &linked]);
    let before = reads(&traced_progress(&repo));

    let unrelated = begin_change(&repo, "unrelated", None);
    for index in 0..12 {
        stdout(repo.arc(&repo.root).args([
            "keep",
            &unrelated,
            "--kind",
            "constraint",
            "--body",
            &format!("fact {index}"),
        ]));
    }
    let after = reads(&traced_progress(&repo));
    assert_eq!(
        before, after,
        "an unlinked change changed what the report read"
    );

    let other = sibling_project(&repo, "other");
    let remote = begin_change_in(&repo, &other, "remote");
    progress(
        &repo,
        &repo.root,
        &["link", "bounded", "i3", &format!("other::{remote}")],
    );
    let foreign = reads(&traced_progress(&repo));
    let count = |reads: &serde_json::Value, key: &str| reads[key].as_u64().unwrap();
    assert_eq!(
        count(&foreign, "changes_reduced"),
        count(&after, "changes_reduced") + 1
    );
    assert!(
        count(&foreign, "git") <= count(&after, "git") + 2,
        "{after} then {foreign}"
    );
    let second = begin_change_in(&repo, &other, "remote-too");
    progress(
        &repo,
        &repo.root,
        &["link", "bounded", "i4", &format!("other::{second}")],
    );
    let both = reads(&traced_progress(&repo));
    assert_eq!(count(&both, "git"), count(&foreign, "git"), "{both}");
    assert_eq!(
        count(&both, "changes_reduced"),
        count(&foreign, "changes_reduced") + 1
    );
    let idle = begin_change_in(&repo, &other, "remote-idle");
    for index in 0..12 {
        stdout(repo.arc(&other).args([
            "keep",
            &idle,
            "--kind",
            "constraint",
            "--body",
            &format!("fact {index}"),
        ]));
    }
    assert_eq!(
        reads(&traced_progress(&repo)),
        both,
        "an unlinked change of the other project changed what the report read"
    );
}

/// Claim: `retire --by` and `start --follows` take only a bare artifact
/// filename of this journal that is a regular file: a relative path climbing
/// out, an absolute path, a filename-shaped symbolic link to a file outside
/// the journal, and a qualified `<directory>::<file>` reference through a
/// symbolically linked directory are each refused and record nothing; the
/// qualified one is refused by its spelling, before anything under the named
/// directory is read. Rejects a name that leads the journal to read a file
/// outside its directories, a decoy binding among them.
#[test]
fn artifact_names_on_the_command_line_never_leave_the_journal() {
    let repo = Repo::new();
    start(&repo, &repo.root, "contained", &["Only"]);
    let dir = journal_dir(&repo, &repo.root);
    let outside = repo.home.join("20261114T000000Z-outside-conclusion.md");
    fs::write(&outside, "# Outside\n").unwrap();
    let plan = repo.home.join("20261114T000000Z-outside-plan.md");
    fs::write(&plan, "# Outside plan\n").unwrap();
    let link = "20261114T000001Z-linked-conclusion.md";
    std::os::unix::fs::symlink(&outside, dir.join(link)).unwrap();
    let relative = pathdiff(&dir, &outside);
    let events = journal_event_log(&dir).len();
    for name in [relative.as_str(), outside.to_str().unwrap(), link] {
        repo.arc(&repo.root)
            .args(["progress", "retire", "contained", "--by", name])
            .assert()
            .failure();
    }
    let decoy = repo.home.join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    fs::write(
        decoy.join("bindings.jsonl"),
        "{\"schema\":\"journal-binding/1\",\"ts\":\"2026-11-14T00:00:00Z\",\"event\":\"bound\",\"anchor\":\"DECOY-MARKER\"}\n",
    )
    .unwrap();
    fs::write(decoy.join("20261114T000000Z-x-conclusion.md"), "# X\n").unwrap();
    fs::write(decoy.join("20261114T000000Z-x-plan.md"), "# X\n").unwrap();
    let through = repo.home.join("through");
    std::os::unix::fs::symlink(&decoy, &through).unwrap();
    let qualified = |file: &str| format!("{}::{file}", through.display());
    for args in [
        vec![
            "retire".to_string(),
            "contained".to_string(),
            "--by".to_string(),
            qualified("20261114T000000Z-x-conclusion.md"),
        ],
        vec![
            "start".to_string(),
            "decoyed".to_string(),
            "--title".to_string(),
            "Decoyed".to_string(),
            "--follows".to_string(),
            qualified("20261114T000000Z-x-plan.md"),
        ],
    ] {
        let output = repo
            .arc(&repo.root)
            .arg("progress")
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("DECOY-MARKER"),
            "a qualified name was resolved outside the journal: {stderr}"
        );
        assert!(stderr.contains("bare"), "{stderr}");
    }
    let relative_plan = pathdiff(&dir, &plan);
    for name in [relative_plan.as_str(), plan.to_str().unwrap()] {
        repo.arc(&repo.root)
            .args([
                "progress",
                "start",
                "other",
                "--title",
                "Other",
                "--follows",
                name,
            ])
            .assert()
            .failure();
    }
    assert_eq!(
        journal_event_log(&dir).len(),
        events,
        "a refused name recorded something"
    );
    assert_eq!(
        report(&repo, &repo.root, &[])["series"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// `to` spelled relative to `from`, climbing with `..` to their common root.
fn pathdiff(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|part| part.as_os_str().to_string_lossy().to_string()),
    );
    parts.join("/")
}

/// Claim: `begin --item` checks the item before anything is written: a
/// series or item that does not exist is refused in a repository with no
/// ledger, and leaves no ledger, branch or change behind. Rejects a preflight
/// that creates the ledger before refusing.
#[test]
fn begin_with_an_unknown_item_leaves_nothing_behind() {
    let repo = Repo::new();
    start(&repo, &repo.root, "fresh", &["Only"]);
    let ledger = repo.root.join(".git/arc");
    assert!(!ledger.exists(), "the fixture starts without a ledger");
    for spec in ["nosuch:i1", "fresh:i9"] {
        repo.arc(&repo.root)
            .args(["begin", "never", "--item", spec])
            .assert()
            .failure();
        assert!(!ledger.exists(), "{spec} left a ledger behind");
    }
    assert!(git_out(&repo.root, &["branch", "--list", "arc/never"])
        .trim()
        .is_empty());
}

/// Claim: `reopen` undoes `drop` for an item that keeps its link, and the
/// reopened item reads its stage from the change again and counts in flight.
/// Rejects a reopen limited to unlinked items, or one that leaves a linked
/// item reading as open.
#[test]
fn reopening_a_dropped_linked_item_returns_it_to_its_derived_stage() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "back", &["Linked"]);
    let change = begin_change(&repo, "back-work", None);
    progress(&repo, &repo.root, &["link", "back", "i1", &change]);
    progress(
        &repo,
        &repo.root,
        &["drop", "back", "i1", "--reason", "parked"],
    );
    assert_eq!(stage(&repo, &file, "i1")["stage"], "dropped");
    progress(
        &repo,
        &repo.root,
        &["reopen", "back", "i1", "--note", "resumed"],
    );
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "opened");
    assert_eq!(row["change"], change.as_str());
    let report = report(&repo, &repo.root, &[]);
    assert_eq!(series(&report, &file)["counts"]["in_flight"], 1);
}

/// Claim: a link is written only while the item's link is still the one the
/// command read, so a link another writer made between that read and the
/// write is refused and kept, never silently replaced. Rejects a link decided
/// on a stale reading.
#[cfg(debug_assertions)]
#[test]
fn a_link_made_meanwhile_is_not_replaced() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "race", &["Contested"]);
    let first = begin_change(&repo, "first", None);
    let second = begin_change(&repo, "second", None);
    let dir = journal_dir(&repo, &repo.root);
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".locks/transition.lock"))
        .unwrap();
    held.lock().unwrap();
    let mut child = spawn_arc_paused_on_lock(
        &repo,
        &repo.root,
        &["progress", "link", "race", "i1", &second],
        "transition",
        &[],
    );
    let competing = serde_json::json!({
        "schema": "journal-events/2",
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "topic": "race",
        "event": "item-linked",
        "file": file,
        "item": "i1",
        "change": first,
    });
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    use std::io::Write;
    writeln!(log, "{competing}").unwrap();
    drop(held);
    release_lock_wait(&mut child);
    let status = wait_for_exit(&mut child);
    let stderr = child_stderr(&mut child);
    assert!(
        !status.success(),
        "the stale link replaced the newer one: {stderr}"
    );
    assert!(stderr.contains("since this command read it"), "{stderr}");
    assert_eq!(stage(&repo, &file, "i1")["change"], first.as_str());
}

/// Claim: a series' title as corrected through the journal is the title the
/// report shows, retired or not, as the journal's other views show it.
/// Rejects a report that keeps the title as first written.
#[test]
fn a_corrected_series_title_is_the_one_reported() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "titled", &["Only"]);
    let (_, conclusion) = journal_artifact(&repo, "titled", "conclusion", "# Done\n");
    progress(
        &repo,
        &repo.root,
        &["retire", "titled", "--by", &conclusion],
    );
    stdout(repo.arc(&repo.root).args([
        "journal",
        "correct",
        &file,
        "--target",
        "artifact",
        "--field",
        "title",
        "--value",
        "Titled, corrected",
    ]));
    let report = report(&repo, &repo.root, &[&file]);
    assert_eq!(series(&report, &file)["title"], "Titled, corrected");
    assert!(progress(&repo, &repo.root, &[&file, "--plain"])
        .starts_with(&format!("== Titled, corrected ({file})")));
}

/// Claim: `unrelate` removes a relation this journal records, appending one
/// event, so the series no longer names or prints the related series.
/// Rejects a relation that stays after it was removed.
#[test]
fn unrelate_removes_the_relation_here() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    let here = start(&repo, &repo.root, "here", &["Local"]);
    let there = start(&repo, &other, "there", &["Remote"]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    let dir = journal_dir(&repo, &repo.root);
    let events = journal_event_log(&dir).len();
    let removed = progress(&repo, &repo.root, &["unrelate", "here", "other::there"]);
    assert!(removed.contains(&there), "{removed}");
    assert_eq!(journal_event_log(&dir).len(), events + 1);
    let report = report(&repo, &repo.root, &[]);
    assert!(series(&report, &here)["related"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(report["related"].as_array().unwrap().is_empty());
    repo.arc(&repo.root)
        .args(["progress", "unrelate", "here", "other::there"])
        .assert()
        .failure();
}

/// Claim: `arc workspace progress` reports a project whose ledger cannot be
/// opened as a failure and exits 16, naming the project. Rejects a ledger
/// error read as an `unknown` stage, which would report a broken ledger as
/// a series in good order.
#[test]
fn workspace_progress_fails_on_a_ledger_it_cannot_open() {
    let repo = Repo::new();
    start(&repo, &repo.root, "broken", &["Linked"]);
    let change = begin_change(&repo, "broken-work", None);
    progress(&repo, &repo.root, &["link", "broken", "i1", &change]);
    let config = repo.root.join(".git/arc/config.json");
    assert!(config.is_file(), "{}", config.display());
    fs::write(&config, "{ not json").unwrap();

    let output = repo
        .arc(&repo.root)
        .args(["workspace", "progress", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(16), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let failures = value["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 1, "{value}");
    assert!(
        failures[0]["reason"]
            .as_str()
            .unwrap()
            .contains("cannot read the ledger"),
        "{value}"
    );
    assert!(value["projects"].as_array().unwrap().is_empty(), "{value}");
}

/// Claim: a related series whose project's ledger cannot be opened prints
/// one `unavailable` line naming the ledger, and the report still exits 0
/// with the local series in full. Rejects a ledger error that fails the
/// local report, and one shown as a related block of `unknown` stages.
#[test]
fn a_related_series_with_an_unreadable_ledger_is_unavailable() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &repo.root, "here", &["Local"]);
    let there = start(&repo, &other, "there", &["Remote"]);
    let change = begin_change_in(&repo, &other, "remote-work");
    progress(&repo, &other, &["link", "there", "i1", &change]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    let config = other.join(".git/arc/config.json");
    assert!(config.is_file(), "{}", config.display());
    fs::write(&config, "{ not json").unwrap();

    let output = repo
        .arc(&repo.root)
        .args(["progress", "--plain"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("== here"), "{text}");
    let unavailable: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("unavailable"))
        .collect();
    assert_eq!(unavailable.len(), 1, "{text}");
    assert!(
        unavailable[0].contains(&format!("::{there}")) && unavailable[0].contains("ledger"),
        "{text}"
    );
    assert!(!text.contains("unknown"), "{text}");
}

/// Claim: a superseded consumption records a successor only when its
/// decision names an artifact this journal holds by a bare name: one naming
/// a missing file, or spelled as another directory's reference, records
/// none. Rejects a successor taken from whatever the event names.
#[test]
fn a_successor_is_only_an_artifact_the_journal_holds() {
    let repo = Repo::new();
    let held = start(&repo, &repo.root, "held", &["Only"]);
    let missing = start(&repo, &repo.root, "missing", &["Only"]);
    let qualified = start(&repo, &repo.root, "qualified", &["Only"]);
    let (_, conclusion) = journal_artifact(&repo, "held", "conclusion", "# Held\n");
    progress(&repo, &repo.root, &["retire", "held", "--by", &conclusion]);

    let dir = journal_dir(&repo, &repo.root);
    let retired = journal_event_log(&dir)
        .into_iter()
        .rev()
        .find(|event| event["event"] == "consumed")
        .unwrap();
    let mut lines = String::new();
    for (file, decision) in [
        (
            &missing,
            "20261114T000000Z-absent-conclusion.md".to_string(),
        ),
        (&qualified, format!("/x::{conclusion}")),
    ] {
        let mut event = retired.clone();
        event["file"] = serde_json::json!(file);
        event["decision"] = serde_json::json!(decision);
        lines.push_str(&format!("{event}\n"));
    }
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut log, lines.as_bytes()).unwrap();

    let library =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--library", "--json"]),
        );
    let row = |file: &str| {
        library["projects"][0]["library"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"] == file)
            .unwrap()
            .clone()
    };
    assert_eq!(row(&held)["superseded_by"], conclusion.as_str());
    for file in [&missing, &qualified] {
        let row = row(file);
        assert!(
            row["superseded_by"].is_null(),
            "a successor was taken from a name the journal does not hold: {row}"
        );
    }
}

/// Claim: `verified` needs a pass recorded at the newest patchset's own head.
/// Rejects a pass recorded before a rewrite reading as a run at the rewritten
/// head, which would show a patchset as verified that nothing was run at.
#[test]
fn a_pass_recorded_before_a_rewrite_does_not_verify_the_rewritten_head() {
    let repo = Repo::new();
    repo.declare_gates_locally("[gates.unit]\ncommand='true'\n");
    let file = start(&repo, &repo.root, "rewritten", &["one"]);
    let begun = stdout(
        repo.arc(&repo.root)
            .args(["begin", "carried", "--item", "rewritten:i1"]),
    );
    let change = opened_change_id(&begun);
    let worktree = repo.home.join(".worktrees").join("repo-carried");
    repo.commit(&worktree, "carried.txt", "one\n", "feat: carried");
    stdout(repo.arc(&worktree).args(["snapshot", &change]));
    stdout(repo.arc(&worktree).args(["verify", &change, "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "verified");

    stdout(repo.arc(&worktree).args([
        "rewrite",
        "trailers",
        "--from",
        "HEAD",
        "--append",
        "Assisted-by: fixture:model",
        "--no-sign",
    ]));
    stdout(repo.arc(&worktree).args(["snapshot", &change]));
    assert_eq!(
        stage(&repo, &file, "i1")["stage"],
        "snapshotted",
        "no run is recorded at the rewritten head"
    );
}

/// Claim: an item linked to a change this ledger does not hold reads
/// `unknown` and the report still prints. Rejects failing the whole report
/// for one absent change, and reading a link in the change's place as either
/// an absent change or the history it points at.
#[test]
fn an_item_whose_change_the_ledger_does_not_hold_reads_unknown() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "absent", &["one", "two"]);
    let begun = stdout(
        repo.arc(&repo.root)
            .args(["begin", "gone", "--item", "absent:i1"]),
    );
    let change = opened_change_id(&begun);
    assert_eq!(stage(&repo, &file, "i1")["stage"], "opened");
    let held = repo.root.join(".git/arc/changes").join(&change);
    let aside = repo.home.join("change-aside");
    fs::rename(&held, &aside).unwrap();
    assert_eq!(stage(&repo, &file, "i1")["stage"], "unknown");
    assert_eq!(stage(&repo, &file, "i2")["stage"], "open");

    // A link standing where the change's directory should be is a ledger
    // that cannot be read, not an absent change and not a way to another's
    // history.
    std::os::unix::fs::symlink(&aside, &held).unwrap();
    repo.arc(&repo.root)
        .args(["progress", &file, "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not a directory of this ledger"));
}

/// Claim: without a form flag `arc progress` prints the view for a person,
/// unpainted when standard output is not a terminal, and `--plain` prints
/// the complete text with the series' filename; `arc workspace progress`
/// takes the same forms. Rejects escape bytes reaching a pipe, a default
/// that is still the complete text, and a `--plain` that lost a filename
/// or a note.
#[test]
fn the_default_is_the_unpainted_view_and_plain_keeps_every_fact() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "forms", &["Keep", "Wait"]);
    progress(
        &repo,
        &repo.root,
        &["done", "forms", "i1", "--note", "by hand"],
    );

    let view = progress(&repo, &repo.root, &[]);
    assert!(!view.contains('\u{1b}'), "{view:?}");
    let lines: Vec<&str> = view.lines().collect();
    assert_eq!(
        &lines[..7],
        [
            "forms",
            "██████████░░░░░░░░░░  1 of 2 · 50%   1 open",
            "forms",
            "",
            "  ✓ i1  Keep  done",
            "        by hand",
            "  ○ i2  Wait",
        ],
        "{view}"
    );
    assert!(!view.contains(&file), "{view}");

    let plain = progress(&repo, &repo.root, &["--plain"]);
    assert!(plain.contains(&format!("== forms ({file})")), "{plain}");
    assert!(plain.contains("done  Keep  [by hand]"), "{plain}");
    assert!(plain.is_ascii(), "{plain}");

    repo.arc(&repo.root)
        .args(["progress", "--plain", "--json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));

    let workspace = |form: &[&str]| {
        stdout(
            repo.arc(&repo.root)
                .args(["workspace", "progress", "--here"])
                .args(form),
        )
    };
    let view = workspace(&[]);
    assert!(view.contains("  ✓ i1  Keep  done"), "{view}");
    assert!(view.contains("(1 series)"), "{view}");
    assert!(!view.contains('\u{1b}'), "{view:?}");
    assert!(
        workspace(&["--plain"]).contains(&format!("== forms ({file})")),
        "{view}"
    );
}

/// Claim: `arc workspace report --json` carries a project's active series
/// only when given `--progress`, and then exactly the series
/// `arc workspace progress --json` reports for that project. Rejects
/// progress printed into a report that did not ask for it, a second reading
/// of the series that differs from the first, and a key on a project that
/// has no series.
#[test]
fn the_workspace_report_carries_series_only_when_asked() {
    let repo = Repo::new();
    let report = |flags: &[&str]| {
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--here", "--json"])
                .args(flags),
        )
    };
    assert!(report(&["--progress"])["projects"][0]
        .get("progress")
        .is_none());

    start(&repo, &repo.root, "carried", &["Linked", "By hand"]);
    let change = begin_change(&repo, "carried-work", None);
    progress(&repo, &repo.root, &["link", "carried", "i1", &change]);
    progress(&repo, &repo.root, &["done", "carried", "i2"]);

    let plain = report(&[]);
    assert_eq!(plain["schema"], "arc-workspace-report/3");
    assert!(plain["projects"][0].get("progress").is_none(), "{plain}");

    let asked = report(&["--progress"]);
    let own = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "progress", "--here", "--json"]),
    );
    assert_eq!(
        asked["projects"][0]["progress"],
        own["projects"][0]["series"]
    );
    assert_eq!(
        asked["projects"][0]["progress"][0]["items"][0]["stage"],
        "opened"
    );
    let (mut without, mut plain) = (asked.clone(), plain);
    without["projects"][0]
        .as_object_mut()
        .unwrap()
        .remove("progress");
    for report in [&mut without, &mut plain] {
        report["observation"] = serde_json::Value::Null;
    }
    assert_eq!(without, plain, "--progress changed the rest of the report");
}

/// Claim: with `--progress`, each project of the report carries its own
/// series even when two registered projects share a label. Rejects series
/// attached by label, which shows one project another's stages and leaves
/// the other without any.
#[test]
fn projects_sharing_a_label_each_carry_their_own_series() {
    let repo = Repo::new();
    let label = repo.root.file_name().unwrap().to_str().unwrap();
    let twin = sibling_project(&repo, &format!("nested/{label}"));
    start(&repo, &repo.root, "first-here", &["Here"]);
    start(&repo, &twin, "then-there", &["There"]);

    let report =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "report", "--json", "--progress"]),
        );
    let carried: std::collections::BTreeMap<String, String> = report["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|project| project["project"] == label)
        .map(|project| {
            let series = project["progress"].as_array().unwrap();
            assert_eq!(series.len(), 1, "{project}");
            (
                project["anchor"].as_str().unwrap().to_string(),
                series[0]["title"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        carried,
        std::collections::BTreeMap::from([
            (repo.root.display().to_string(), "first-here".to_string()),
            (twin.display().to_string(), "then-there".to_string()),
        ])
    );
}

/// Claim: `arc progress start` ends with one line naming how a change joins
/// the series as it opens, in the series' own topic, after the filename and
/// one line per item. Rejects a start that leaves the `begin --item` habit
/// unsaid, and one that names a series other than the one it started.
#[test]
fn start_ends_naming_how_a_change_joins_the_series() {
    let repo = Repo::new();
    let out = progress(
        &repo,
        &repo.root,
        &["start", "ship", "--title", "Ship", "--item", "One"],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(
        lines[2],
        "a change joins as it opens: arc begin <slug> --item ship:<item>"
    );
}

/// The last `item-linked` event the journal at `dir` holds.
fn last_link(dir: &Path) -> serde_json::Value {
    journal_event_log(dir)
        .into_iter()
        .rev()
        .find(|event| event["event"] == "item-linked")
        .unwrap()
}

/// Claim: `link <series> <item> <project>::<change>` records the other
/// project's registry slug on the link, and the item's stage then follows
/// that project's ledger exactly as a local link follows this one's,
/// through `opened`, `snapshotted` and `integrated` with that ledger's time
/// and revision, with no progress write after the link; the row names the
/// project beside the change in every form. A link is replaced only as the
/// other ledger allows. Once the project is no longer registered the item
/// reads `unknown` and the report still succeeds. Rejects a foreign link
/// read from this project's ledger (`unknown` from the start), a stage that
/// stops following the other ledger, a row that drops the project, and an
/// unregistered project failing the report.
#[test]
fn a_link_into_another_project_follows_that_ledger() {
    let repo = Repo::new();
    let other = sibling_project(&repo, "other");
    start(&repo, &other, "there", &["Remote"]);
    let file = start(&repo, &repo.root, "cross", &["Remote slice", "Local"]);
    let change = begin_change_in(&repo, &other, "remote-slice");
    let linked = progress(
        &repo,
        &repo.root,
        &["link", "cross", "i1", &format!("other::{change}")],
    );
    assert_eq!(linked.trim(), format!("linked i1 to other::{change}"));
    let dir = journal_dir(&repo, &repo.root);
    let writes = item_events(&dir);
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "opened", "{row}");
    assert_eq!(row["change"], change.as_str());
    assert_eq!(row["project"], "other");
    let slug = row["project_slug"].as_str().unwrap().to_string();
    assert_eq!(last_link(&dir)["change_project"], slug.as_str());
    assert_eq!(
        series(&report(&repo, &repo.root, &[]), &file)["counts"]["in_flight"],
        1
    );
    let view = progress(&repo, &repo.root, &[]);
    assert!(view.contains(&format!("opened  other::{change}")), "{view}");
    let plain = progress(&repo, &repo.root, &["--plain"]);
    assert!(
        plain.contains(&format!("change other::{change} (project {slug})")),
        "{plain}"
    );

    let local = begin_change(&repo, "local-work", None);
    repo.arc(&repo.root)
        .args(["progress", "link", "cross", "i1", &local])
        .assert()
        .failure()
        .stderr(predicates::str::contains("still open or integrated"));

    git(&other, &["checkout", "arc/remote-slice"]);
    repo.commit(&other, "slice.txt", "one\n", "feat: slice");
    stdout(repo.arc(&other).args(["snapshot", "remote-slice"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "snapshotted");

    git(&other, &["checkout", "master"]);
    stdout(
        repo.arc(&other)
            .args(["review", "remote-slice", "--verdict", "approved"]),
    );
    stdout(repo.arc(&other).args(["integrate", "remote-slice"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "integrated", "{row}");
    assert_eq!(row["revision"], repo.head(&other).as_str());
    assert!(row["integrated_at"].is_string(), "{row}");
    assert_eq!(
        item_events(&dir),
        writes,
        "a progress write moved the stage"
    );

    fs::remove_dir_all(journal_dir(&repo, &other)).unwrap();
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "unknown", "{row}");
    assert_eq!(row["project_slug"], slug.as_str());
    assert_eq!(stage(&repo, &file, "i2")["stage"], "open");
}

/// A discussion in `repo`'s journal holding one open question that waits on
/// a person: its filename and the question's id.
fn open_question(repo: &Repo, topic: &str) -> (String, String) {
    let (_, file) = journal_artifact(repo, topic, "discussion", "# Pick a store\n");
    repo.arc(&repo.root)
        .args([
            "journal",
            "question",
            &file,
            "--placement",
            "opening",
            "--option",
            "files",
            "--option",
            "sqlite",
            "--body-file",
            "-",
        ])
        .write_stdin("Which store holds the cache?\n")
        .assert()
        .success();
    let summary =
        json_stdout(
            repo.arc(&repo.root)
                .args(["journal", "discussion", &file, "--json"]),
        );
    let id = summary["questions"][0]["id"].as_str().unwrap().to_string();
    (file, id)
}

/// Claim: an item said to wait on a journal question carries the wait (file,
/// question id, heading) while the question is open and waits on a person,
/// whatever its stage, and the series counts it as waiting; an open item
/// reads `waiting` with the `?` mark and the question under its title, a
/// linked one keeps its stage and gains `waiting` beside it. Once the
/// question is answered the rows show nothing of it, with no progress event
/// written, and `unwait` removes a wait. Rejects a wait that changes a
/// stage, one that outlives its answer, one that takes a progress write to
/// clear, and a view that hides the wait.
#[test]
fn a_wait_shows_while_its_question_is_open() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "held", &["Blocked", "Linked", "Free"]);
    let (discussion, question) = open_question(&repo, "pick-a-store");
    let waits = progress(
        &repo,
        &repo.root,
        &["wait", "held", "i1", &discussion, "--question", &question],
    );
    assert_eq!(
        waits.trim(),
        format!("i1 waits on {question} of {discussion}")
    );
    let change = begin_change(&repo, "linked-work", None);
    progress(&repo, &repo.root, &["link", "held", "i2", &change]);
    progress(
        &repo,
        &repo.root,
        &["wait", "held", "i2", &discussion, "--question", &question],
    );
    let dir = journal_dir(&repo, &repo.root);
    let writes = item_events(&dir);

    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "open");
    assert_eq!(row["waiting"]["file"], discussion.as_str());
    assert_eq!(row["waiting"]["question"], question.as_str());
    let heading = row["waiting"]["heading"].as_str().unwrap().to_string();
    let linked = stage(&repo, &file, "i2");
    assert_eq!(linked["stage"], "opened");
    assert_eq!(linked["waiting"]["question"], question.as_str());
    assert!(stage(&repo, &file, "i3").get("waiting").is_none());
    let json = report(&repo, &repo.root, &[]);
    let counts = &series(&json, &file)["counts"];
    assert_eq!(counts["waiting"], 2);
    assert_eq!(counts["open"], 2);
    assert_eq!(counts["in_flight"], 1);

    let view = progress(&repo, &repo.root, &[]);
    let lines: Vec<&str> = view.lines().collect();
    let blocked = lines
        .iter()
        .position(|line| line.starts_with("  ? i1  Blocked"))
        .unwrap_or_else(|| panic!("{view}"));
    assert!(lines[blocked].contains("waiting"), "{view}");
    assert_eq!(lines[blocked + 1].trim(), heading, "{view}");
    assert!(
        lines.iter().any(|line| line.contains("i2  Linked")
            && line.contains("opened")
            && line.contains("waiting")),
        "{view}"
    );
    assert!(
        lines[1].contains("1 in flight · 2 waiting · 2 open"),
        "{view}"
    );
    let plain = progress(&repo, &repo.root, &["--plain"]);
    assert!(
        plain.contains(&format!("waiting on {discussion} {question}: {heading}")),
        "{plain}"
    );
    assert!(plain.contains("; 2 waiting; 2 open"), "{plain}");

    repo.arc(&repo.root)
        .args([
            "journal",
            "answer",
            &discussion,
            "--question",
            &question,
            "--option",
            "files",
            "--body-file",
            "-",
        ])
        .write_stdin("Files.\n")
        .assert()
        .success();
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "open");
    assert!(row.get("waiting").is_none(), "{row}");
    assert!(stage(&repo, &file, "i2").get("waiting").is_none());
    let json = report(&repo, &repo.root, &[]);
    assert!(series(&json, &file)["counts"].get("waiting").is_none());
    let view = progress(&repo, &repo.root, &[]);
    assert!(!view.contains("waiting"), "{view}");
    assert!(view.contains("  ○ i1  Blocked"), "{view}");
    assert_eq!(
        item_events(&dir),
        writes,
        "a progress write cleared the wait"
    );

    let unwaited = progress(&repo, &repo.root, &["unwait", "held", "i1"]);
    assert!(unwaited.contains("i1 waits no longer"), "{unwaited}");
    repo.arc(&repo.root)
        .args(["progress", "unwait", "held", "i1"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("waits on no question"));
}

/// Claim: `wait` takes only a discussion of this journal named by its bare
/// filename, and only a question that discussion holds and that is still
/// open for a person; a question id the file does not hold, a path out of
/// the journal, an absolute path, another kind of artifact and an answered
/// question are each refused and record nothing. Rejects a wait on a
/// question nobody asked, and a name that reaches outside the journal.
#[test]
fn a_wait_names_only_an_open_question_of_this_journal() {
    let repo = Repo::new();
    start(&repo, &repo.root, "held", &["Blocked"]);
    let (discussion, question) = open_question(&repo, "pick-a-store");
    let (_, note) = journal_artifact(&repo, "plain-note", "note", "# A note\n");
    let dir = journal_dir(&repo, &repo.root);
    let outside = repo.home.join("20261114T000000Z-outside-discussion.md");
    fs::write(&outside, "# Outside\n").unwrap();
    let climbing = format!("../{}", outside.file_name().unwrap().to_string_lossy());
    let before = item_events(&dir);
    let refused = |file: &str, id: &str, why: &str| {
        repo.arc(&repo.root)
            .args(["progress", "wait", "held", "i1", file, "--question", id])
            .assert()
            .failure()
            .stderr(predicates::str::contains(why.to_string()));
    };
    refused(&discussion, "q-none", "holds no question q-none");
    refused(&climbing, &question, "bare discussion artifact filename");
    refused(
        outside.to_str().unwrap(),
        &question,
        "bare discussion artifact filename",
    );
    refused(&note, &question, "is a note");
    assert_eq!(item_events(&dir), before);

    repo.arc(&repo.root)
        .args([
            "journal",
            "answer",
            &discussion,
            "--question",
            &question,
            "--option",
            "files",
            "--body-file",
            "-",
        ])
        .write_stdin("Files.\n")
        .assert()
        .success();
    refused(&discussion, &question, "is answered");
    assert_eq!(item_events(&dir), before);
}

/// The relation `series` in `project` of a workspace progress report
/// records to `file`.
fn workspace_relation<'a>(
    report: &'a serde_json::Value,
    project: &str,
    series: &str,
    file: &str,
) -> &'a serde_json::Value {
    report["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["project"] == project)
        .and_then(|row| {
            row["series"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["file"] == series)
        })
        .and_then(|row| {
            row["related"]
                .as_array()
                .unwrap()
                .iter()
                .find(|relation| relation["file"] == file)
        })
        .unwrap_or_else(|| panic!("no relation {project}::{series} -> {file} in {report}"))
}

/// Claim: `arc workspace progress` fills a relation's `mutual` from the
/// series it read itself: `true` when the related series relates back,
/// `false` when that series was read and does not, and absent when this
/// command did not read it (out of its scope); the plain form says the
/// same. Rejects every relation reported unread in a workspace view that
/// read both ends, and a guess about a series it did not read.
#[test]
fn workspace_progress_knows_whether_a_relation_is_mutual() {
    let repo = Repo::new();
    let label = repo.root.file_name().unwrap().to_string_lossy().to_string();
    let here = start(&repo, &repo.root, "here", &["Local"]);
    let other = sibling_project(&repo, "other");
    let there = start(&repo, &other, "there", &["Remote"]);
    let third = sibling_project(&repo, "third");
    let far = start(&repo, &third, "far", &["Far"]);
    progress(&repo, &repo.root, &["relate", "here", "other::there"]);
    progress(&repo, &repo.root, &["relate", "here", "third::far"]);
    progress(
        &repo,
        &other,
        &["relate", "there", &format!("{label}::here")],
    );

    let all = json_stdout(
        repo.arc(&repo.root)
            .args(["workspace", "progress", "--json"]),
    );
    assert_eq!(
        workspace_relation(&all, &label, &here, &there)["mutual"],
        true
    );
    assert_eq!(
        workspace_relation(&all, &label, &here, &far)["mutual"],
        false
    );
    assert_eq!(
        workspace_relation(&all, "other", &there, &here)["mutual"],
        true
    );
    let plain = stdout(
        repo.arc(&repo.root)
            .args(["workspace", "progress", "--plain"]),
    );
    assert!(
        plain.contains(&format!("other::{there} (mutual)")),
        "{plain}"
    );
    assert!(
        plain.contains(&format!("third::{far} (one-sided)")),
        "{plain}"
    );

    let scoped =
        json_stdout(
            repo.arc(&repo.root)
                .args(["workspace", "progress", "--here", "--json"]),
        );
    for file in [&there, &far] {
        let relation = workspace_relation(&scoped, &label, &here, file);
        assert!(relation.get("mutual").is_none(), "{relation}");
    }
}

/// Claim: a linked item in flight carries the newest run the ledger records
/// dispatched against its change until that run's end is recorded: whom it
/// was dispatched to (the represented subject, else the route), when, and
/// whether a backend session is bound, in every form. Rejects a dispatch
/// shown after its recorded end, a bound run shown unbound, and a dispatch
/// against another change shown on this item.
#[test]
fn a_dispatch_shows_on_its_item_until_its_end_is_recorded() {
    let repo = Repo::new();
    let file = start(&repo, &repo.root, "runs", &["Delegated", "Other"]);
    let change = begin_change(&repo, "delegated", None);
    let elsewhere = begin_change(&repo, "elsewhere", None);
    progress(&repo, &repo.root, &["link", "runs", "i1", &change]);
    progress(&repo, &repo.root, &["link", "runs", "i2", &elsewhere]);
    assert!(stage(&repo, &file, "i1").get("dispatch").is_none());
    let dispatch = |route: &str| {
        let printed = stdout(repo.arc(&repo.root).args([
            "run",
            "dispatch",
            "--route",
            route,
            "--worktree",
            "/work/run",
            "--change",
            &change,
        ]));
        printed
            .lines()
            .find_map(|line| line.strip_prefix("event: "))
            .unwrap()
            .to_string()
    };
    let first = dispatch("first-route");
    let run = dispatch("second-route");
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["dispatch"]["to"], "second-route", "{row}");
    assert_eq!(row["dispatch"]["bound"], false);
    assert!(row["dispatch"]["at"].is_string(), "{row}");
    assert!(stage(&repo, &file, "i2").get("dispatch").is_none());
    let view = progress(&repo, &repo.root, &[]);
    assert!(
        view.contains("dispatched to second-route just now, unbound"),
        "{view}"
    );
    assert!(
        progress(&repo, &repo.root, &["--plain"]).contains("dispatched to second-route at "),
        "plain lost the dispatch"
    );

    stdout(repo.arc(&repo.root).args([
        "run",
        "bind",
        &run,
        "--executor-harness",
        "pi",
        "--executor-session",
        "session-one",
    ]));
    assert_eq!(stage(&repo, &file, "i1")["dispatch"]["bound"], true);
    let view = progress(&repo, &repo.root, &[]);
    assert!(
        view.contains("dispatched to second-route just now"),
        "{view}"
    );
    assert!(!view.contains("unbound"), "{view}");

    stdout(
        repo.arc(&repo.root)
            .args(["run", "end", &run, "--outcome", "completed"]),
    );
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["dispatch"]["to"], "first-route", "{row}");
    stdout(
        repo.arc(&repo.root)
            .args(["run", "end", &first, "--outcome", "completed"]),
    );
    assert!(stage(&repo, &file, "i1").get("dispatch").is_none());
}

/// What a question body read outside a journal would print.
const OUTSIDE_MARKER: &str = "OUTSIDE-THE-JOURNAL-MARKER";

/// Append to the journal at `dir` one copy of its first `question` event per
/// name in `files`, each naming that file and a question id of its own, and
/// return the ids in order.
fn forge_questions(dir: &Path, files: &[String]) -> Vec<String> {
    let template = journal_event_log(dir)
        .into_iter()
        .find(|event| event["event"] == "question")
        .unwrap();
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let id = format!("q-forged{index}");
            let mut event = template.clone();
            event["file"] = file.as_str().into();
            event["question_id"] = id.as_str().into();
            use std::io::Write;
            writeln!(log, "{event}").unwrap();
            id
        })
        .collect()
}

/// A discussion-shaped file outside every journal whose questions all read
/// as the marker, and the names a recorded question can reach it by from
/// the journal at `dir`: its absolute path, a path climbing out of the
/// journal, and a discussion-named symbolic link inside the journal.
fn outside_discussions(repo: &Repo, dir: &Path, ids: &[&str]) -> Vec<String> {
    let name = "20261114T000000Z-outside-discussion.md";
    let mut body = String::from("# Outside\n");
    for id in ids {
        body.push_str(&format!("\n### Question {id}\n\n{OUTSIDE_MARKER}\n"));
    }
    let outside = repo.home.join(name);
    fs::write(&outside, &body).unwrap();
    fs::write(dir.parent().unwrap().join(name), &body).unwrap();
    let linked = "20261114T000001Z-linked-discussion.md";
    std::os::unix::fs::symlink(&outside, dir.join(linked)).unwrap();
    vec![
        outside.display().to_string(),
        format!("../{name}"),
        linked.to_string(),
    ]
}

/// Claim: a question's body is read only from a bare artifact filename that
/// is a regular file in its journal or that journal's cold archive. A
/// recorded question naming an absolute path, a path climbing out of the
/// journal or a symbolic link inside it is shown without a heading and
/// fails nothing: `arc progress` in every form, `arc progress wait`, and the
/// related-series projection of another project's waiting item print none
/// of what such a file holds. Rejects a question body read through a name
/// that leads out of the journal's directories.
#[test]
fn a_question_body_is_never_read_from_outside_its_journal() {
    let repo = Repo::new();
    start(&repo, &repo.root, "held", &["Blocked"]);
    let (discussion, question) = open_question(&repo, "pick-a-store");
    let dir = journal_dir(&repo, &repo.root);
    let ids = ["q-forged0", "q-forged1", "q-forged2"];
    let names = outside_discussions(&repo, &dir, &ids);
    assert_eq!(forge_questions(&dir, &names), ids);

    for form in [&[][..], &["--plain"], &["--json"]] {
        let printed = progress(&repo, &repo.root, form);
        assert!(!printed.contains(OUTSIDE_MARKER), "{printed}");
        assert!(printed.contains("q-forged2"), "{printed}");
    }
    let waited = progress(
        &repo,
        &repo.root,
        &["wait", "held", "i1", &discussion, "--question", &question],
    );
    assert!(!waited.contains(OUTSIDE_MARKER), "{waited}");

    // Another project's item in flight that a forged event says waits on
    // the question behind its symbolic link.
    let other = sibling_project(&repo, "other");
    let there = start(&repo, &other, "there", &["Remote"]);
    let change = begin_change_in(&repo, &other, "remote-work");
    progress(&repo, &other, &["link", "there", "i1", &change]);
    let other_dir = journal_dir(&repo, &other);
    fs::write(
        other_dir.join("20261114T000002Z-real-discussion.md"),
        "# Real\n",
    )
    .unwrap();
    let mut seed = journal_event_log(&dir)
        .into_iter()
        .find(|event| event["event"] == "question")
        .unwrap();
    seed["file"] = "20261114T000002Z-real-discussion.md".into();
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(other_dir.join("events.jsonl"))
        .unwrap();
    use std::io::Write;
    writeln!(log, "{seed}").unwrap();
    drop(log);
    let names = outside_discussions(&repo, &other_dir, &ids);
    forge_questions(&other_dir, &names);
    let mut wait = journal_event_log(&other_dir)
        .into_iter()
        .find(|event| event["event"] == "item-linked")
        .unwrap();
    wait["event"] = "item-waiting".into();
    wait.as_object_mut().unwrap().remove("change");
    wait["waits_on"] = serde_json::json!({ "file": names[2], "question": "q-forged2" });
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(other_dir.join("events.jsonl"))
        .unwrap();
    writeln!(log, "{wait}").unwrap();
    drop(log);
    progress(&repo, &repo.root, &["relate", "held", "other::there"]);
    for form in [&[][..], &["--plain"], &["--json"]] {
        let printed = progress(&repo, &repo.root, form);
        assert!(!printed.contains(OUTSIDE_MARKER), "{printed}");
        assert!(printed.contains("Remote"), "{printed}");
    }
    let json = report(&repo, &repo.root, &[]);
    let related = json["related"]
        .as_array()
        .unwrap()
        .iter()
        .find(|related| related["file"] == there.as_str())
        .unwrap();
    assert_eq!(related["in_flight"][0]["waiting"]["question"], "q-forged2");
    assert!(related["in_flight"][0]["waiting"]["heading"].is_null());
}

fn write_journal_artifact(repo: &Repo, cwd: &Path, kind: &str, topic: &str, body: &str) -> String {
    let body_path = repo
        .root
        .parent()
        .unwrap()
        .join(format!("body-{kind}-{topic}.md"));
    fs::write(&body_path, body).unwrap();
    let out = stdout(
        repo.arc(cwd)
            .args(["journal", kind, topic, "--body-file"])
            .arg(&body_path),
    );
    Path::new(out.trim())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// An item's `--from` / `source` link validates allowed artifact kinds (local
/// or cross-project), derives open/consumed status, flags drift when a done or
/// integrated item's non-plan source remains open, and clears drift once the
/// source is consumed. Rejects unvalidated kinds/paths, missing drift signals,
/// or false drift on plan sources.
#[test]
fn source_link_tracks_open_consumed_and_drift() {
    let repo = Repo::new();
    let todo = write_journal_artifact(&repo, &repo.root, "todo", "fix-cache", "# Fix cache\n");
    let plan = write_journal_artifact(&repo, &repo.root, "plan", "cache-plan", "# Cache plan\n");
    let note = write_journal_artifact(&repo, &repo.root, "note", "scratch", "# Scratch\n");

    let file = stdout(repo.arc(&repo.root).args([
        "progress",
        "start",
        "cache-work",
        "--title",
        "Cache work",
        "--item",
        "Fix cache",
        "--from",
        &todo,
    ]))
    .lines()
    .next()
    .unwrap()
    .to_string();

    let added = progress(
        &repo,
        &repo.root,
        &["add", &file, "Plan slice", "--from", &plan],
    );
    assert!(added.contains("i2"), "{added}");

    // Disallowed kind and path traversal are refused before writing.
    let bad_kind = repo
        .arc(&repo.root)
        .args(["progress", "source", &file, "i1", &note])
        .output()
        .unwrap();
    assert!(!bad_kind.status.success());
    let bad_path = repo
        .arc(&repo.root)
        .args(["progress", "source", &file, "i1", "../outside-todo.md"])
        .output()
        .unwrap();
    assert!(!bad_path.status.success());

    // Open item shows source open, no drift.
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["source"]["file"], todo);
    assert_eq!(i1["source"]["status"], "open");
    assert!(i1["source"]["drift"].is_null());

    // Marking i1 done while todo is open flags drift; marking i2 (plan) done does not.
    progress(&repo, &repo.root, &["done", &file, "i1"]);
    progress(&repo, &repo.root, &["done", &file, "i2"]);
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["source"]["status"], "open");
    assert_eq!(i1["source"]["drift"], true);
    let plain = progress(&repo, &repo.root, &[&file, "--plain"]);
    assert!(plain.contains("drift: source still open"), "{plain}");
    let view = progress(&repo, &repo.root, &[&file]);
    assert!(
        view.contains(&format!("source still open: {todo}")),
        "{view}"
    );
    let i2 = stage(&repo, &file, "i2");
    assert_eq!(i2["source"]["status"], "open");
    assert!(i2["source"]["drift"].is_null());

    // Consuming the todo clears drift.
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "consume", &todo, "--outcome", "done"]),
    );
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["source"]["status"], "consumed");
    assert!(i1["source"]["drift"].is_null());

    // Cross-project source artifact resolves through the registry, and unsource clears it.
    let other = sibling_project(&repo, "other");
    let remote_todo = write_journal_artifact(&repo, &other, "todo", "remote-fix", "# Remote\n");
    progress(&repo, &repo.root, &["add", &file, "Remote slice"]);
    progress(
        &repo,
        &repo.root,
        &["source", &file, "i3", &format!("other::{remote_todo}")],
    );
    let i3 = stage(&repo, &file, "i3");
    assert_eq!(i3["source"]["project"], "other");
    assert_eq!(i3["source"]["file"], remote_todo);
    assert_eq!(i3["source"]["status"], "open");

    progress(&repo, &repo.root, &["unsource", &file, "i3"]);
    let i3 = stage(&repo, &file, "i3");
    assert!(i3["source"].is_null());

    // Journal doctor accepts all new events cleanly.
    let doctor = repo
        .arc(&repo.root)
        .args(["journal", "doctor"])
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
}

/// `arc progress done --by` validates bare conclusion/decision filenames and
/// recorded checkpoint names, surfaces `resolved_by` in all views, refuses
/// invalid targets, and clears `resolved_by` when reopened.
#[test]
fn done_by_validates_conclusion_decision_and_checkpoint() {
    let repo = Repo::new();
    let conclusion = write_journal_artifact(
        &repo,
        &repo.root,
        "conclusion",
        "bench-done",
        "# Benchmark conclusion\n",
    );
    let decision = write_journal_artifact(
        &repo,
        &repo.root,
        "decision",
        "use-cache",
        "# Cache decision\n",
    );
    let todo = write_journal_artifact(&repo, &repo.root, "todo", "not-a-resolution", "# Todo\n");
    stdout(
        repo.arc(&repo.root)
            .args(["checkpoint", "record", "rc1-test"]),
    );

    let file = start(
        &repo,
        &repo.root,
        "non-change",
        &["Bench", "Decide", "Ship"],
    );

    // Rejects wrong journal kind, missing checkpoint, and path traversal.
    for bad in [
        todo.as_str(),
        "no-such-checkpoint",
        "../escape-conclusion.md",
    ] {
        let out = repo
            .arc(&repo.root)
            .args(["progress", "done", &file, "i1", "--by", bad])
            .output()
            .unwrap();
        assert!(!out.status.success(), "expected refusal for {bad}");
    }

    progress(
        &repo,
        &repo.root,
        &["done", &file, "i1", "--by", &conclusion],
    );
    progress(&repo, &repo.root, &["done", &file, "i2", "--by", &decision]);
    progress(
        &repo,
        &repo.root,
        &["done", &file, "i3", "--by", "rc1-test"],
    );

    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["resolved_by"]["kind"], "conclusion");
    assert_eq!(i1["resolved_by"]["target"], conclusion);
    let i2 = stage(&repo, &file, "i2");
    assert_eq!(i2["resolved_by"]["kind"], "decision");
    assert_eq!(i2["resolved_by"]["target"], decision);
    let i3 = stage(&repo, &file, "i3");
    assert_eq!(i3["resolved_by"]["kind"], "checkpoint");
    assert_eq!(i3["resolved_by"]["target"], "rc1-test");

    let plain = progress(&repo, &repo.root, &[&file, "--plain"]);
    assert!(
        plain.contains(&format!("by conclusion {conclusion}")),
        "{plain}"
    );
    assert!(plain.contains("by checkpoint rc1-test"), "{plain}");
    let view = progress(&repo, &repo.root, &[&file]);
    assert!(view.contains(&format!("by {conclusion}")), "{view}");
    assert!(view.contains("by rc1-test"), "{view}");

    // Reopening clears resolved_by.
    progress(&repo, &repo.root, &["reopen", &file, "i1"]);
    let reopened = stage(&repo, &file, "i1");
    assert_eq!(reopened["stage"], "open");
    assert!(reopened["resolved_by"].is_null());
}

/// `arc begin --item` propagates an item's open `--from` source into
/// `--from-journal` (consuming it and seeding the brief), refuses an explicit
/// `--from-journal` naming a different artifact, and succeeds without error
/// when the item's source was already consumed.
#[test]
fn begin_item_propagates_and_crosschecks_item_source() {
    let repo = Repo::new();
    let todo_a = write_journal_artifact(&repo, &repo.root, "todo", "slice-a", "# Slice A\n");
    let todo_b = write_journal_artifact(&repo, &repo.root, "todo", "slice-b", "# Slice B\n");
    let file = start(&repo, &repo.root, "propagate", &["A", "B"]);
    progress(&repo, &repo.root, &["source", &file, "i1", &todo_a]);
    progress(&repo, &repo.root, &["source", &file, "i2", &todo_b]);

    // Conflicting explicit --from-journal refuses before opening anything.
    let conflict = repo
        .arc(&repo.root)
        .args([
            "begin",
            "conflict-slice",
            "--no-worktree",
            "--item",
            "propagate:i1",
            "--from-journal",
            &todo_b,
        ])
        .output()
        .unwrap();
    assert!(!conflict.status.success());
    let stderr = String::from_utf8_lossy(&conflict.stderr);
    assert!(
        stderr.contains("differs from progress item source"),
        "{stderr}"
    );

    // Omitting --from-journal propagates todo_a automatically and consumes it.
    let out = stdout(
        repo.arc(&repo.root)
            .args(["begin", "slice-a", "--item", "propagate:i1"]),
    );
    assert!(out.contains(&todo_a), "{out}");
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["stage"], "opened");
    assert_eq!(i1["source"]["status"], "consumed");

    // Pre-consuming todo_b does not block begin --item; it simply skips auto-promotion.
    stdout(
        repo.arc(&repo.root)
            .args(["journal", "consume", &todo_b, "--outcome", "done"]),
    );
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "slice-b", "--item", "propagate:i2"]),
    );
    let i2 = stage(&repo, &file, "i2");
    assert_eq!(i2["stage"], "opened");
    assert_eq!(i2["source"]["status"], "consumed");
}

/// Pre-change advisory ownership surfaces on unlinked open items from either
/// an active `arc claim` on the item's `--from` source or a live `arc journal
/// lane open --item <series>:<item>` (with live lane taking precedence), counts
/// in `claimed`, and clears once released/closed or once `arc begin` links a
/// change.
#[test]
fn pre_change_claims_surface_from_source_claim_and_live_lane() {
    let repo = Repo::new();
    let todo = write_journal_artifact(&repo, &repo.root, "todo", "probe-work", "# Probe work\n");
    let file = start(&repo, &repo.root, "prechange", &["Probe work", "Other"]);
    progress(&repo, &repo.root, &["source", &file, "i1", &todo]);

    // 1. Active `arc claim` on the source todo surfaces as artifact-claim.
    stdout(repo.arc(&repo.root).args([
        "claim",
        &todo,
        "--session",
        "sess-claim",
        "--harness",
        "codex",
    ]));
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["stage"], "open");
    assert_eq!(i1["claimed"]["via"], "artifact-claim");
    let full = report(&repo, &repo.root, &[&file]);
    assert_eq!(series(&full, &file)["counts"]["claimed"], 1);
    let view = progress(&repo, &repo.root, &[&file]);
    assert!(view.contains("◈"), "{view}");
    assert!(view.contains("claimed by"), "{view}");

    // 2. Live lane targeting `prechange:i1` takes precedence over source claim.
    stdout(repo.arc(&repo.root).args([
        "journal",
        "lane",
        "open",
        "probe-lane",
        "--item",
        "prechange:i1",
        "--session",
        "sess-lane",
        "--harness",
        "claude",
        "--status",
        "profiling",
    ]));
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["claimed"]["via"], "lane");
    assert_eq!(i1["claimed"]["lane"], "probe-lane");
    assert_eq!(i1["claimed"]["owner"], "claude sess-lane");

    // `journal lane list --json` carries the canonical item target.
    let lanes = json_stdout(
        repo.arc(&repo.root)
            .args(["journal", "lane", "list", "--json"]),
    );
    assert_eq!(lanes["lanes"][0]["item"]["file"], file);
    assert_eq!(lanes["lanes"][0]["item"]["item"], "i1");

    // 3. Closing the lane falls back to the active source claim.
    stdout(repo.arc(&repo.root).args([
        "journal",
        "lane",
        "close",
        "probe-lane",
        "--session",
        "sess-lane",
        "--harness",
        "claude",
    ]));
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["claimed"]["via"], "artifact-claim");

    // 4. Releasing the claim clears pre-change ownership.
    stdout(repo.arc(&repo.root).args([
        "release-claim",
        &todo,
        "--session",
        "sess-claim",
        "--harness",
        "codex",
    ]));
    let i1 = stage(&repo, &file, "i1");
    assert!(i1["claimed"].is_null());

    // 5. Once linked to a change, a live lane targeting the item no longer
    // renders a pre-change claim on top of the ledger-backed stage.
    stdout(repo.arc(&repo.root).args([
        "journal",
        "lane",
        "open",
        "probe-lane-2",
        "--item",
        "prechange:i1",
        "--session",
        "sess-lane-2",
        "--harness",
        "claude",
    ]));
    stdout(
        repo.arc(&repo.root)
            .args(["begin", "probe-change", "--item", "prechange:i1"]),
    );
    let i1 = stage(&repo, &file, "i1");
    assert_eq!(i1["stage"], "opened");
    assert!(i1["claimed"].is_null());
}

#[test]
fn archived_source_has_no_drift_and_unregistered_foreign_source_refuses_explicit_from_journal() {
    let repo = Repo::new();

    // 1. A shelved (cold-archived, unconsumed) todo reports status=archived and drift=false when done.
    let (_, todo) = journal_artifact(&repo, "shelved-work", "todo", "# Shelved work\n");
    let file = start(&repo, &repo.root, "archive-drift", &["Shelved item"]);
    progress(&repo, &repo.root, &["source", &file, "i1", &todo]);
    let dir = journal_dir(&repo, &repo.root);
    let mut cold = dir.as_os_str().to_os_string();
    cold.push("-archive");
    let archive_dir = std::path::PathBuf::from(cold);
    fs::create_dir_all(&archive_dir).unwrap();
    fs::rename(dir.join(&todo), archive_dir.join(&todo)).unwrap();
    progress(&repo, &repo.root, &["done", &file, "i1"]);
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["source"]["status"], "archived");
    assert_ne!(row["source"]["drift"], true);

    // 2. When an item records a foreign source whose project is no longer
    // registered, `begin --item ... --from-journal <unrelated>` refuses rather
    // than consuming the unrelated artifact.
    let (_, local_todo) = journal_artifact(&repo, "unrelated", "todo", "# Unrelated\n");
    let series2 = start(&repo, &repo.root, "foreign-src", &["Foreign item"]);
    let fake_sourced = serde_json::json!({
        "schema": "journal-events/2",
        "ts": "2026-01-02T00:00:00Z",
        "topic": "foreign-src",
        "event": "item-sourced",
        "file": series2,
        "item": "i1",
        "item_source": {
            "project": "vanished-project",
            "file": "20260101T000000Z-ghost-todo.md"
        }
    });
    let mut events_text = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    events_text.push_str(&format!("{fake_sourced}\n"));
    fs::write(dir.join("events.jsonl"), events_text).unwrap();

    repo.arc(&repo.root)
        .args([
            "begin",
            "mismatch",
            "--item",
            &format!("{series2}:i1"),
            "--from-journal",
            &local_todo,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not registered"));
    // Unrelated local todo remains live and unconsumed.
    assert!(dir.join(&local_todo).is_file());
}
