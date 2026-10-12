use super::common::*;

const SONNET_AUTHOR: &str = "claude:claude-sonnet-5-5#high";
const SOL_AUTHOR: &str = "codex:gpt-6-sol#medium";
const OPUS_READ: &str = "claude-opus-5-5#xhigh";
const ASTRA_READ: &str = "gpt-6-astra#xhigh";
const FRONTIER_TIER: i64 = 3;

/// A stand-in `agent-routing` that answers fixed `agent-routing-frontier/1`
/// documents: one file per author and one per read, both under one directory.
/// It records every call it receives.
struct FakeRouting {
    dir: PathBuf,
}

fn file_key(coordinate: &str) -> String {
    coordinate.replace('#', "_")
}

impl FakeRouting {
    fn new(repo: &Repo) -> FakeRouting {
        let dir = repo.home.join("fake-routing");
        fs::create_dir_all(dir.join("bin")).unwrap();
        let script = dir.join("bin/agent-routing");
        fs::write(
            &script,
            r#"#!/bin/sh
d="$FAKE_ROUTING_DIR"
[ "$1" = frontier ] || exit 2
shift
author=
reads=
while [ $# -gt 0 ]; do
  case "$1" in
    --author) author=$2; shift 2 ;;
    --reads)
      shift
      while [ $# -gt 0 ] && [ "${1#--}" = "$1" ]; do reads="$reads $1"; shift; done ;;
    *) shift ;;
  esac
done
echo "$author$reads" >> "$d/calls"
[ -f "$d/fail" ] && exit 3
a=$(printf %s "$author" | tr '#' _)
if [ ! -f "$d/author-$a.json" ]; then
  echo '{"coordinate":"x","kind":"unknown-coordinate","message":"m","role":"author","schema":"agent-routing-refusal/1"}'
  exit 1
fi
for r in $reads; do
  k=$(printf %s "$r" | tr '#' _)
  if [ ! -f "$d/read-$a-$k.json" ] && [ ! -f "$d/read-$k.json" ]; then
    echo '{"coordinate":"x","kind":"unknown-coordinate","message":"m","role":"reviewer","schema":"agent-routing-refusal/1"}'
    exit 1
  fi
done
cat "$d/author-$a.json"
sep=
for r in $reads; do
  k=$(printf %s "$r" | tr '#' _)
  p="$d/read-$a-$k.json"
  [ -f "$p" ] || p="$d/read-$k.json"
  printf %s "$sep"; cat "$p"; sep=,
done
printf ']}'
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&script, permissions).unwrap();
        FakeRouting { dir }
    }

    /// An author answered with a frontier at `tier`, status `frontier`.
    fn author(&self, coordinate: &str, tier: i64) -> &Self {
        self.author_with_status(coordinate, "frontier", Some(tier))
    }

    fn author_with_status(&self, coordinate: &str, status: &str, tier: Option<i64>) -> &Self {
        let frontier = tier.map_or("null".to_string(), |tier| {
            format!("{{\"review_tier\":{tier},\"seats\":[]}}")
        });
        fs::write(
            self.dir
                .join(format!("author-{}.json", file_key(model_of(coordinate)))),
            format!(
                "{{\"schema\":\"agent-routing-frontier/1\",\"status\":\"{status}\",\
                 \"route_version\":\"r1\",\"frontier\":{frontier},\"reads\":["
            ),
        )
        .unwrap();
        self
    }

    /// A read as routing assesses it: independence, tier and standing.
    fn read(&self, coordinate: &str, eligible: bool, tier: i64, standing: &str) -> &Self {
        self.read_file(
            &format!("read-{}.json", file_key(coordinate)),
            eligible,
            tier,
            standing,
        )
    }

    /// The same, for one author's answer only.
    fn read_for(
        &self,
        author: &str,
        coordinate: &str,
        eligible: bool,
        tier: i64,
        standing: &str,
    ) -> &Self {
        self.read_file(
            &format!(
                "read-{}-{}.json",
                file_key(model_of(author)),
                file_key(coordinate)
            ),
            eligible,
            tier,
            standing,
        )
    }

    fn read_file(&self, name: &str, eligible: bool, tier: i64, standing: &str) -> &Self {
        let (independence, qualifiers) = if eligible {
            ("eligible", "[]")
        } else {
            ("ineligible", "[\"same-family\"]")
        };
        fs::write(
            self.dir.join(name),
            format!(
                "{{\"standing\":\"{standing}\",\"assessment\":{{\"independence\":\"{independence}\",\
                 \"qualifiers\":{qualifiers},\"reviewer\":{{\"model\":\"m\",\"review_tier\":{tier}}},\
                 \"route_version\":\"r1\",\"schema\":\"agent-routing-assessment/1\"}}}}"
            ),
        )
        .unwrap();
        self
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.dir.join("bin").display())
    }

    fn arc(&self, repo: &Repo, cwd: &Path) -> AssertCommand {
        let mut cmd = repo.arc(cwd);
        cmd.env("PATH", self.path())
            .env("FAKE_ROUTING_DIR", &self.dir);
        cmd
    }
}

/// The model a role value names, with its effort and without its harness.
fn model_of(coordinate: &str) -> &str {
    coordinate
        .split_once(':')
        .map_or(coordinate, |(_, rest)| rest)
}

/// A repository whose policy declares one dangerous path.
fn repo_with_danger() -> Repo {
    let repo = Repo::new();
    fs::create_dir_all(repo.root.join(".arc")).unwrap();
    fs::write(
        repo.root.join(".arc/policy.toml"),
        "[danger]\npaths = [\"danger/**\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".arc/policy.toml"]);
    git(&repo.root, &["commit", "-m", "policy"]);
    repo
}

/// What one integrated change is made of.
struct Spec<'a> {
    slug: &'a str,
    /// Path and content of the one file the change adds.
    file: (&'a str, &'a str),
    /// `Implemented-by` values on the change's commit.
    authors: &'a [&'a str],
    /// Models of the approving reads, one reviewer each.
    reads: &'a [&'a str],
}

fn integrate(repo: &Repo, spec: &Spec) -> String {
    let change = opened_change_id(&stdout(repo.arc(&repo.root).args(["begin", spec.slug])));
    let worktree = repo
        .home
        .join(".worktrees")
        .join(format!("repo-{}", spec.slug));
    let mut message = format!("feat: {}\n", spec.slug);
    for author in spec.authors {
        message.push_str(&format!("\nImplemented-by: {author}"));
    }
    let path = worktree.join(spec.file.0);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, spec.file.1).unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", &message]);
    stdout(repo.arc(&worktree).args(["snapshot", spec.slug]));
    for (index, model) in spec.reads.iter().enumerate() {
        stdout(
            repo.arc(&repo.root)
                .env("ARC_ACTOR", format!("reviewer-{index}"))
                .args([
                    "--model",
                    model,
                    "review",
                    spec.slug,
                    "--verdict",
                    "approved",
                ]),
        );
    }
    // Nobody read a change that holds no approval, so it integrates owing one.
    if spec.reads.is_empty() {
        stdout(
            repo.arc(&repo.root)
                .args(["integrate", spec.slug, "--debt", "nobody read it"]),
        );
    } else {
        stdout(repo.arc(&repo.root).args(["integrate", spec.slug]));
    }
    change
}

fn change(slug: &str, reads: &'static [&'static str]) -> Spec<'static> {
    Spec {
        slug: Box::leak(slug.to_string().into_boxed_str()),
        file: (
            Box::leak(format!("{slug}.txt").into_boxed_str()),
            "one\ntwo\nthree\n",
        ),
        authors: &[SONNET_AUTHOR],
        reads,
    }
}

fn assurance(fake: &FakeRouting, repo: &Repo, extra: &[&str]) -> serde_json::Value {
    json_stdout(
        fake.arc(repo, &repo.root)
            .args(["query", "--assurance", "--json"])
            .args(extra),
    )
}

fn row<'a>(report: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["change_id"] == id)
        .unwrap_or_else(|| panic!("no row for {id} in {report}"))
}

fn standing(report: &serde_json::Value, id: &str) -> String {
    row(report, id)["assurance"]["standing"]
        .as_str()
        .unwrap_or_else(|| panic!("no standing for {id} in {report}"))
        .to_string()
}

fn ids(report: &serde_json::Value) -> Vec<String> {
    report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["change_id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_integrated_change_lists_the_reads_it_received() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    let read = integrate(&repo, &change("read", &[OPUS_READ]));
    let bare = integrate(&repo, &change("bare", &[]));
    stdout(repo.arc(&repo.root).args(["begin", "open-change"]));

    let report = assurance(&fake, &repo, &[]);
    assert_eq!(report["schema"], "arc-assurance/1", "{report}");
    assert_eq!(report["stale"], false, "{report}");
    assert_eq!(ids(&report).len(), 2, "{report}");
    let reads = row(&report, &read)["reads"].as_array().unwrap();
    assert_eq!(reads.len(), 1, "{report}");
    assert_eq!(reads[0]["kind"], "verdict", "{report}");
    assert_eq!(reads[0]["verdict"], "approved", "{report}");
    assert_eq!(reads[0]["model"], OPUS_READ, "{report}");
    assert_eq!(reads[0]["effort"], "xhigh", "{report}");
    assert!(reads[0]["at"].is_string(), "{report}");
    assert_eq!(
        row(&report, &bare)["reads"],
        serde_json::json!([]),
        "{report}"
    );
    assert_eq!(row(&report, &bare)["debt"], "owed", "{report}");
    // The view asks routing for nothing unless the standing is requested.
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert!(row(&report, &read).get("assurance").is_none(), "{report}");
}

#[test]
fn an_audit_is_a_read_beside_the_verdicts() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    let id = integrate(&repo, &change("audited", &[]));
    stdout(repo.arc(&repo.root).env("ARC_ACTOR", "auditor").args([
        "--model",
        ASTRA_READ,
        "audit",
        "audited",
        "--verdict",
        "approved",
    ]));
    let report = assurance(&fake, &repo, &[]);
    let reads = row(&report, &id)["reads"].as_array().unwrap();
    assert_eq!(reads.len(), 1, "{report}");
    assert_eq!(reads[0]["kind"], "audit", "{report}");
    assert_eq!(reads[0]["model"], ASTRA_READ, "{report}");
}

#[test]
fn a_change_approved_by_an_eligible_read_at_the_frontier_is_not_stale() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read(ASTRA_READ, true, 3, "at");
    let id = integrate(&repo, &change("fresh", &[ASTRA_READ]));

    let report = assurance(&fake, &repo, &["--stale"]);
    assert!(
        ids(&report).is_empty(),
        "a fresh change is not listed: {report}"
    );
    assert_eq!(report["stale"], true, "{report}");
    let _ = id;
}

#[test]
fn a_same_family_read_at_the_frontier_tier_leaves_the_change_owed() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read(OPUS_READ, false, 3, "at");
    let id = integrate(&repo, &change("family", &[OPUS_READ]));

    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &id), "owed", "{report}");
}

#[test]
fn a_change_whose_only_read_is_below_the_frontier_is_stale_with_its_gap() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    let id = integrate(&repo, &change("below", &["gpt-5-5#high"]));

    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &id), "stale", "{report}");
    let judged = &row(&report, &id)["assurance"];
    assert_eq!(judged["components"]["frontier_gap"], 2, "{report}");
    assert_eq!(judged["route_version"], "r1", "{report}");
    assert_eq!(
        judged["authors"],
        serde_json::json!(["claude-sonnet-5-5#high"]),
        "{report}"
    );
    assert_eq!(
        judged["components"]["live_fraction"]["fraction"], 1.0,
        "{report}"
    );
    assert_eq!(
        judged["components"]["live_fraction"]["added"], 3,
        "{report}"
    );
    assert!(judged["components"]["age_seconds"].is_number(), "{report}");

    let text = stdout(
        fake.arc(&repo, &repo.root)
            .args(["query", "--assurance", "--stale"]),
    );
    assert!(text.contains("standing: stale"), "{text}");
    assert!(text.contains("frontier gap: 2"), "{text}");
}

#[test]
fn a_change_whose_lines_were_all_rewritten_has_nothing_to_refresh() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    let id = integrate(&repo, &change("rewritten", &["gpt-5-5#high"]));
    repo.commit(
        &repo.root,
        "rewritten.txt",
        "wholly\nnew\ncontent\n",
        "rewrite it",
    );

    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &id), "no-live-lines", "{report}");
    let live = &row(&report, &id)["assurance"]["components"]["live_fraction"];
    assert_eq!(live["fraction"], 0.0, "{report}");
    assert_eq!(live["live"], 0, "{report}");
}

#[test]
fn a_file_moved_later_keeps_its_lines_live() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    let id = integrate(&repo, &change("moved", &["gpt-5-5#high"]));
    git(&repo.root, &["mv", "moved.txt", "relocated.txt"]);
    git(&repo.root, &["commit", "-m", "move it"]);

    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &id), "stale", "{report}");
    let live = &row(&report, &id)["assurance"]["components"]["live_fraction"];
    assert_eq!(live["live"], 3, "{report}");
}

#[test]
fn a_danger_path_change_sorts_before_a_plain_change_with_the_same_gap() {
    let repo = repo_with_danger();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    // The plain change is older and has the lower change id, so only the
    // exposure component can put the dangerous one first.
    let plain = integrate(&repo, &change("a-plain", &["gpt-5-5#high"]));
    let mut dangerous = change("z-danger", &["gpt-5-5#high"]);
    dangerous.file = ("danger/engine.txt", "one\ntwo\nthree\n");
    let danger = integrate(&repo, &dangerous);

    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(
        ids(&report),
        vec![danger.clone(), plain.clone()],
        "{report}"
    );
    let exposure = &row(&report, &danger)["assurance"]["components"]["exposure"];
    assert_eq!(exposure["state"], "touched", "{report}");
    assert_eq!(exposure["paths"], 1, "{report}");
    let exposure = &row(&report, &plain)["assurance"]["components"]["exposure"];
    assert_eq!(exposure["state"], "untouched", "{report}");
}

#[test]
fn routing_unavailable_leaves_every_row_unknown_and_listed() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    let one = integrate(&repo, &change("one", &[OPUS_READ]));
    let two = integrate(&repo, &change("two", &[ASTRA_READ]));
    // No `agent-routing` is on this PATH at all.
    let empty = repo.home.join("empty-bin");
    fs::create_dir_all(&empty).unwrap();
    let report = json_stdout(
        repo.arc(&repo.root)
            .env("PATH", format!("{}:/usr/bin:/bin", empty.display()))
            .args(["query", "--assurance", "--stale", "--json"]),
    );
    for id in [&one, &two] {
        assert_eq!(standing(&report, id), "unknown", "{report}");
        assert_eq!(
            row(&report, id)["assurance"]["reason"],
            "routing-unavailable",
            "{report}"
        );
        // The other components do not depend on routing.
        assert_eq!(
            row(&report, id)["assurance"]["components"]["live_fraction"]["fraction"],
            1.0,
            "{report}"
        );
    }

    // A routing that runs and fails is the same unavailability, and says
    // nothing of what it printed.
    fs::write(fake.dir.join("fail"), "").unwrap();
    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &one), "unknown", "{report}");
    assert!(!report.to_string().contains("fake-routing"), "{report}");
}

#[test]
fn a_change_with_two_implementing_models_is_stale_when_either_frontier_is_above() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    // The read meets the first author's frontier and falls short of the
    // second's.
    fake.author("claude-sonnet-5-5#high", 1)
        .author("gpt-6-sol#medium", FRONTIER_TIER)
        .read_for("claude-sonnet-5-5#high", "gpt-5-5#high", true, 1, "at")
        .read_for("gpt-6-sol#medium", "gpt-5-5#high", true, 1, "below");
    let mut both = change("both", &["gpt-5-5#high"]);
    both.authors = &[SONNET_AUTHOR, SOL_AUTHOR];
    let id = integrate(&repo, &both);

    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &id), "stale", "{report}");
    assert_eq!(
        row(&report, &id)["assurance"]["authors"],
        serde_json::json!(["claude-sonnet-5-5#high", "gpt-6-sol#medium"]),
        "{report}"
    );
    assert_eq!(fake.calls().len(), 2, "{:?}", fake.calls());
}

#[test]
fn routing_is_asked_once_per_distinct_author_and_reads() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    for slug in ["one", "two", "three"] {
        integrate(&repo, &change(slug, &["gpt-5-5#high"]));
    }
    assurance(&fake, &repo, &["--stale"]);
    assert_eq!(
        fake.calls(),
        vec!["claude-sonnet-5-5#high gpt-5-5#high".to_string()]
    );
}

#[test]
fn a_read_without_a_recorded_model_is_unknown_never_fresh() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER);
    let id = integrate(&repo, &change("unrecorded", &[""]));
    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &id), "unknown", "{report}");
    assert_eq!(
        row(&report, &id)["assurance"]["reason"],
        "read-model-unrecorded",
        "{report}"
    );
}

#[test]
fn a_read_routing_refuses_does_not_hide_the_others() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    let known = integrate(&repo, &change("known", &["gpt-5-5#high"]));
    // A change with a stale read and one routing has never heard of cannot be
    // called stale: the unheard-of read may stand at the frontier.
    let mixed = integrate(&repo, &change("mixed", &["gpt-5-5#high", "mystery#high"]));
    let report = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(standing(&report, &known), "stale", "{report}");
    assert_eq!(standing(&report, &mixed), "unknown", "{report}");
    assert_eq!(
        row(&report, &mixed)["assurance"]["reason"],
        "read-unassessed",
        "{report}"
    );
}

#[test]
fn the_blame_is_cached_per_head_and_change() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    let id = integrate(&repo, &change("cached", &["gpt-5-5#high"]));
    let head = repo.head(&repo.root);

    let first = assurance(&fake, &repo, &["--stale"]);
    assert_eq!(
        row(&first, &id)["assurance"]["components"]["live_fraction"]["live"],
        3
    );
    let file = repo
        .root
        .join(".git/arc/cache/assurance")
        .join(format!("{head}-{id}.json"));
    assert!(file.exists(), "no cache file at {}", file.display());

    // A second read at the same revision answers from the cache: an entry
    // that says something else is what it reports.
    let mut entry: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    entry["live"] = serde_json::json!(1);
    fs::write(&file, entry.to_string()).unwrap();
    let second = assurance(&fake, &repo, &["--stale"]);
    let live = &row(&second, &id)["assurance"]["components"]["live_fraction"];
    assert_eq!(live["live"], 1, "{second}");

    // A new revision is a new key, measured again.
    repo.commit(&repo.root, "elsewhere.txt", "x\n", "unrelated");
    let third = assurance(&fake, &repo, &["--stale"]);
    let live = &row(&third, &id)["assurance"]["components"]["live_fraction"];
    assert_eq!(live["live"], 3, "{third}");
}

#[test]
fn a_change_whose_blame_fails_keeps_its_other_components() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    let id = integrate(&repo, &change("blamed", &["gpt-5-5#high"]));
    // The reviewed range is gone from this checkout's view of history.
    let onto = repo.home.join("elsewhere");
    git(
        &repo.root,
        &[
            "worktree",
            "add",
            "--detach",
            onto.to_str().unwrap(),
            "HEAD~1",
        ],
    );

    let report =
        json_stdout(
            fake.arc(&repo, &onto)
                .args(["query", "--assurance", "--stale", "--json"]),
        );
    assert_eq!(standing(&report, &id), "stale", "{report}");
    let components = &row(&report, &id)["assurance"]["components"];
    assert_eq!(
        components["live_fraction"]["reason"], "not-reachable-from-head",
        "{report}"
    );
    assert!(
        components["live_fraction"].get("fraction").is_none(),
        "{report}"
    );
    assert_eq!(components["frontier_gap"], 2, "{report}");
}

#[test]
fn no_path_or_raw_text_travels_in_the_view() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    integrate(&repo, &change("private", &[OPUS_READ]));
    let report = assurance(&fake, &repo, &["--stale"]).to_string();
    let home = repo.home.to_string_lossy().to_string();
    assert!(!report.contains(&home), "{report}");
}

#[test]
fn assurance_never_gates_an_integration() {
    let repo = Repo::new();
    let fake = FakeRouting::new(&repo);
    fake.author("claude-sonnet-5-5#high", FRONTIER_TIER)
        .read("gpt-5-5#high", true, 1, "below");
    integrate(&repo, &change("stale-one", &["gpt-5-5#high"]));
    assurance(&fake, &repo, &["--stale"]);
    // A stale change in the ledger does not stand in the way of the next.
    integrate(&repo, &change("next", &["gpt-5-5#high"]));
}
