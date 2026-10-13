use super::common::*;

#[test]
fn guide_navigation_is_stateless_with_broken_configuration_and_every_role() {
    let repo = Repo::new();
    let config = repo.home.join(".local/ai/arc/config.toml");
    let broken = "[identity\ninvalid config\n";
    fs::write(&config, broken).unwrap();
    for role in ["implementer", "reviewer", "lead"] {
        for args in [
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
    assert_eq!(all, bare);
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
}
