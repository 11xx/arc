use super::common::*;

/// Gate declarations have one discoverable topic; rejects missing keys or declarations in files.
#[test]
fn gate_declarations_have_one_discoverable_guide_home() {
    let repo = Repo::new();
    let gates = stdout(repo.arc(&repo.home).args(["guide", "gates"]));
    let files = stdout(repo.arc(&repo.home).args(["guide", "files"]));
    assert!(
        files.contains("arc guide gates"),
        "missing files cross-reference"
    );
    for declaration in [
        "Gates are `[gates.<name>]` tables",
        "command = \"<shell command>\"",
        "profiles = [\"<profile>\"]",
        "events = [\"integration\"]",
        "timeout = \"10m\"",
        "environment = \"<probe command>\"",
        "bypass = \"<what qualifies>\"",
        "binds = \"commit\"",
        "A gate's `selector` reads `arc-gate-delta/1`",
    ] {
        assert!(
            gates.contains(declaration),
            "missing declaration: {declaration}"
        );
        assert!(
            !files.contains(declaration),
            "duplicate declaration: {declaration}"
        );
    }
    let index = stdout(repo.arc(&repo.home).arg("guide"));
    assert!(index.contains("arc guide gates"), "missing topic in index");
    let map = stdout(&mut repo.arc(&repo.home));
    let topics = map.split_once("Topics:").unwrap().1;
    assert!(
        topics.split_whitespace().any(|word| word == "gates"),
        "missing topic in map"
    );
    let search = stdout(
        repo.arc(&repo.home)
            .args(["guide", "--search", "arc-gate-delta/1"]),
    );
    assert!(
        search.contains("arc guide gates:"),
        "missing declaration search route"
    );
    assert!(
        !search.contains("arc guide files:"),
        "declaration search routes to files"
    );
}

#[test]
fn guide_navigation_is_stateless_with_broken_configuration_and_every_role() {
    let repo = Repo::new();
    let config = repo.home.join(".local/ai/arc/config.toml");
    let broken = "[identity\ninvalid config\n";
    fs::write(&config, broken).unwrap();
    for role in ["implementer", "reviewer", "lead"] {
        for args in [
            vec![],
            vec!["--help"],
            vec!["guide"],
            vec!["guide", "identity"],
            vec!["guide", "--all"],
            vec!["guide", "--search", "snapshot"],
        ] {
            repo.arc(&repo.home)
                .env("ARC_ROLE", role)
                .env("ARC_ACTOR", "unknown")
                .args(args)
                .assert()
                .success()
                .stderr("");
        }
    }
    assert_eq!(fs::read_to_string(config).unwrap(), broken);
    assert!(!repo.root.join(".git/arc").exists());
    assert!(!repo.home.join(".local/ai/journals").exists());
}

#[test]
fn guide_index_routes_are_bounded_and_full_depth_preserves_the_manual() {
    let repo = Repo::new();
    let index = stdout(repo.arc(&repo.home).arg("guide"));
    let mut seen = std::collections::BTreeSet::new();
    for line in index.lines() {
        let Some(route) = line.trim().strip_prefix("arc guide ") else {
            continue;
        };
        let name = route.split_whitespace().next().unwrap();
        if name.starts_with("--") {
            continue;
        }
        assert!(
            !name
                .rsplit('-')
                .next()
                .unwrap()
                .chars()
                .all(|ch| ch.is_ascii_digit()),
            "size-cut page {name}"
        );
        assert!(
            seen.insert(name.to_string()),
            "duplicate topic route: {name}"
        );
        let text = stdout(repo.arc(&repo.home).args(["guide", name]));
        assert!(text.len() <= 16_384, "{name}: {} bytes", text.len());
        assert!(!text.is_empty(), "{name}");
    }
    for direct in [
        "identity",
        "debt",
        "assurance",
        "probes",
        "cleanup",
        "candidates",
        "context",
        "cost",
    ] {
        assert!(seen.contains(direct), "missing {direct}");
    }
    for route in [
        "arc snapshot --help",
        "arc check --help",
        "arc journal show --help",
        "arc instructions git --help",
    ] {
        assert!(index.contains(route), "missing {route}");
    }
    let all = stdout(repo.arc(&repo.home).args(["guide", "--all"]));
    let bare = stdout(&mut repo.arc(&repo.home));
    assert!(all.len() > bare.len());
    assert!(all.contains("RUN A CHANGE"));
    assert!(all.contains("JOURNAL DIRECTORY RESOLUTION"));
    repo.arc(&repo.home)
        .args(["guide", "does-not-exist"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown guide topic"))
        .stderr(predicates::str::contains("use `arc guide`"));
}

#[test]
fn guide_search_is_literal_case_insensitive_and_keeps_private_env_values_out() {
    let repo = Repo::new();
    let search = |query: &str| {
        stdout(
            repo.arc(&repo.home)
                .env("ARC_SESSION_LINK", "https://private.invalid/sensitive-link")
                .env("ARC_SESSION", "private-session-marker")
                .args(["guide", "--search", query]),
        )
    };
    let lower = search("evaluation_reuse");
    assert_eq!(lower, search("EVALUATION_REUSE"));
    assert!(lower.contains("arc guide"), "{lower}");
    assert!(lower.contains("arc candidate select --help"), "{lower}");
    let routes: Vec<_> = lower
        .lines()
        .map(|line| line.split(": ").next().unwrap())
        .collect();
    let unique: std::collections::BTreeSet<_> = routes.iter().collect();
    assert_eq!(routes.len(), unique.len());
    let flags = search("--session-link");
    assert!(flags.contains("arc snapshot --help"), "{flags}");
    assert!(!flags.contains("sensitive-link"), "{flags}");
    assert!(!flags.contains("private-session-marker"), "{flags}");
    assert!(search("^no.*such$regex").contains("No instruction matches"));
    // Shared option rows repeat on every command and would bury real matches.
    assert!(search("see `arc guide identity-options`").contains("No instruction matches"));
}

/// The bootstrap must fit an ordinary tool response; a full manual here hides routes.
#[test]
fn bare_map_fits_both_budgets_and_names_bootstrap_routes() {
    let repo = Repo::new();
    let map = stdout(&mut repo.arc(&repo.home));
    assert!(map.lines().count() <= 80, "{} lines", map.lines().count());
    assert!(map.len() <= 6_144, "{} bytes", map.len());
    for route in [
        "eval \"$(arc env)\"",
        "arc catchup",
        "arc workspace",
        "arc journal show",
        "arc snapshot",
        "arc check",
        "arc claim",
        "arc debt",
        "arc audit",
        "arc findings",
        "arc guide --all",
        "arc guide --search",
        "arc guide identity",
    ] {
        assert!(map.contains(route), "missing bootstrap route {route}");
    }
}

/// Help budgets must be independent of private ambient provenance on root, group and leaf reads.
#[test]
fn help_is_bounded_and_env_values_never_change_instruction_bytes() {
    let repo = Repo::new();
    for path in [
        vec![],
        vec!["journal"],
        vec!["journal", "filed"],
        vec!["show"],
        vec!["query"],
    ] {
        for flag in ["-h", "--help"] {
            let render = |sentinel: &str| {
                stdout(
                    repo.arc(&repo.home)
                        .env("ARC_HARNESS", sentinel)
                        .env("ARC_SESSION", sentinel)
                        .env("ARC_SESSION_LINK", sentinel)
                        .env("ARC_ON_BEHALF_OF", sentinel)
                        .args(&path)
                        .arg(flag),
                )
            };
            let first = render("private-marker");
            assert_eq!(
                first,
                render("private-marker-with-a-much-longer-value"),
                "{path:?} {flag}"
            );
            assert!(!first.contains("private-marker"));
            for name in [
                "ARC_HARNESS",
                "ARC_SESSION",
                "ARC_SESSION_LINK",
                "ARC_ON_BEHALF_OF",
            ] {
                if !(path == ["query"] && name == "ARC_HARNESS")
                    && !(path == ["journal", "filed"] && name == "ARC_SESSION")
                {
                    assert!(first.contains(name), "missing {name}: {path:?}");
                }
            }
            if path.is_empty() {
                assert!(first.len() <= 12_288, "root {flag}: {} bytes", first.len());
            }
        }
    }
    let short = stdout(repo.arc(&repo.home).args(["show", "-h"]));
    let long = stdout(repo.arc(&repo.home).args(["show", "--help"]));
    assert!(
        short.len() < long.len(),
        "show -h must leave depth to --help"
    );
}

/// A journal family route must reach directory precedence, including non-Git anchors.
#[test]
fn journal_family_and_literal_search_reach_directory_resolution() {
    let repo = Repo::new();
    let family = stdout(repo.arc(&repo.home).args(["guide", "journal"]));
    assert!(family.contains("arc guide journal-resolution"));
    let resolution = stdout(repo.arc(&repo.home).args(["guide", "journal-resolution"]));
    let terms = [
        "ARC_JOURNAL_DIR",
        "[journals.dirs]",
        "<repo-path-slug>",
        "bindings.jsonl",
    ];
    let mut prior = 0;
    for term in terms {
        let at = resolution.find(term).unwrap();
        assert!(at >= prior, "resolution precedence: {term}");
        prior = at;
    }
    assert!(resolution.contains("outside a repository"));
    assert!(resolution.contains("arc journal rebind"));
    for term in ["anchor", "slug", "prefix", "directory", "ARC_JOURNAL_DIR"] {
        let matches = stdout(repo.arc(&repo.home).args(["guide", "--search", term]));
        assert!(
            matches.contains("arc guide journal-resolution:"),
            "{term}: {matches}"
        );
    }
    for args in [vec!["query", "--help"], vec!["journal", "filed", "--help"]] {
        let help = stdout(repo.arc(&repo.home).args(args));
        assert!(help.contains("only when explicitly typed"), "{help}");
    }
}

/// Refusal outcomes and executable remedies stay intact while facts gain bounded topic routes.
#[test]
fn check_and_catchup_point_to_topics_without_changing_refusal_outcomes() {
    let repo = Repo::new();
    begin_without_checkout(&repo, "topic-pointers", &[]);
    let before = git_out(&repo.root, &["show-ref"]);
    let check = repo
        .arc(&repo.root)
        .args(["check", "topic-pointers"])
        .output()
        .unwrap();
    assert_eq!(check.status.code(), Some(3));
    let check = String::from_utf8(check.stdout).unwrap();
    assert!(check.contains("Next step:"), "{check}");
    assert!(check.contains("Read: arc guide debt"), "{check}");
    let catchup = stdout(repo.arc(&repo.root).args(["catchup", "--sessions", "0"]));
    assert!(catchup.contains("arc guide debt"), "{catchup}");
    assert_eq!(catchup.matches("arc guide debt").count(), 1);
    assert_eq!(git_out(&repo.root, &["show-ref"]), before);
    repo.arc(&repo.root)
        .env("ARC_ROLE", "implementer")
        .args(["debt", "topic-pointers", "--reason", "fixture"])
        .assert()
        .code(9)
        .stderr(predicates::str::contains("role refusal:"))
        .stderr(predicates::str::contains("arc guide identity"));
}
