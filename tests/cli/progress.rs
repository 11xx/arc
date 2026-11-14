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

/// Claim: a linked item's stage is derived from its change at every read:
/// it follows the change through commits, a snapshot, green gates, a review
/// that asked for changes (counted as a round), a fresh snapshot, approval
/// and integration, with its landing time and revision, while no progress
/// write happens after the link. Rejects a stage that is written rather than
/// derived, or that stops following the change at any step.
#[test]
fn a_linked_items_stage_follows_its_change_without_a_progress_write() {
    let repo = Repo::new();
    repo.redeclare_gates("[gates.unit]\ncommand = \"true\"\n");
    let file = start(&repo, &repo.root, "ship", &["Wire the reader"]);
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
    assert_eq!(stage(&repo, &file, "i1")["stage"], "committed");

    stdout(repo.arc(&worktree).args(["snapshot", "reader"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "snapshotted");

    stdout(repo.arc(&worktree).args(["verify", "reader", "--all"]));
    assert_eq!(stage(&repo, &file, "i1")["stage"], "gates-green");

    stdout(repo.arc(&worktree).args([
        "review",
        "reader",
        "--verdict",
        "changes-requested",
        "--cause",
        "executor",
    ]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "changes-requested");
    assert_eq!(row["changes_requested_rounds"], 1);

    repo.commit(&worktree, "reader.txt", "two\n", "fix: reader");
    stdout(repo.arc(&worktree).args(["snapshot", "reader"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "snapshotted");
    assert_eq!(row["changes_requested_rounds"], 1);

    stdout(repo.arc(&worktree).args(["verify", "reader", "--all"]));
    stdout(
        repo.arc(&worktree)
            .args(["review", "reader", "--verdict", "approved"]),
    );
    assert_eq!(stage(&repo, &file, "i1")["stage"], "approved");

    stdout(repo.arc(&repo.root).args(["integrate", "reader"]));
    let row = stage(&repo, &file, "i1");
    assert_eq!(row["stage"], "landed");
    assert!(row["landed_at"].is_string(), "{row}");
    assert_eq!(row["revision"], repo.head(&repo.root).as_str());
    let report = report(&repo, &repo.root, &[]);
    let counts = &series(&report, &file)["counts"];
    assert_eq!(counts["complete"], 1);
    assert_eq!(counts["percent"], 100);
    assert_eq!(series(&report, &file)["last_landing"]["item"], "i1");

    assert_eq!(
        item_events(&dir),
        writes,
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

    let text = progress(&repo, &repo.root, &[]);
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
    let text = progress(&repo, &repo.root, &[]);
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

    let text = progress(&repo, &repo.root, &[]);
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
    assert!(progress(&repo, &repo.root, &[]).contains("(one-sided)"));
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
    let text = progress(&repo, &repo.root, &[]);
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

    let text = progress(&repo, &repo.root, &[]);
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
    let output = repo.arc(&repo.root).arg("progress").output().unwrap();
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
/// superseded with the conclusion named as its successor, removes it from
/// the default listing, keeps it readable by name, and refuses further
/// edits. Rejects a retired series still listed, one unreadable by name, and
/// a retirement that does not name the conclusion.
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
                ("landed", "number"),
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
