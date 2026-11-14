use super::common::*;

/// Claim: printed entry instructions fit their published size ceilings.
/// Rejects a capability map or root help exceeding the reader's bounded input.
#[test]
fn entry_instructions_fit_their_size_ceilings() {
    let repo = Repo::new();
    let map = stdout(&mut repo.arc(&repo.home));
    assert!(map.lines().count() <= 80);
    assert!(map.len() <= 6_144);
    for flag in ["-h", "--help"] {
        let help = stdout(repo.arc(&repo.home).arg(flag));
        assert!(help.len() <= 12_288);
    }
}

/// Claim: a lane starting a gate round reaches its topic from the guide index,
/// the topic names the command or declaration for each step arc performs, and
/// the order of work has one home that the other gate topics point at.
/// Rejects an unindexed topic, a step left as handwork where a command does it,
/// and the order of work restated in a second topic.
#[test]
fn gate_round_method_is_indexed_and_names_its_commands() {
    let repo = Repo::new();
    let index = stdout(repo.arc(&repo.home).arg("guide"));
    assert!(
        index
            .lines()
            .any(|line| line.trim_start().starts_with("arc guide gate-rounds ")),
        "{index}"
    );
    let topic = stdout(repo.arc(&repo.home).args(["guide", "gate-rounds"]));
    for named in [
        "`arc gates measure`",
        "`arc gates`",
        "`arc gates init`",
        "`arc gates measure --runs 6`",
        "`.arc/gates.toml`",
        "`budget`",
        "`paths`",
        "`exclusive`",
        "`[selection]`",
        "`arc journal conclusion <topic>`",
        "`arc workspace gates`",
    ] {
        assert!(topic.contains(named), "gate-rounds omits {named}");
    }
    for referring in ["gates", "gate-costs"] {
        let text = stdout(repo.arc(&repo.home).args(["guide", referring]));
        assert!(
            text.contains("`arc guide gate-rounds`"),
            "{referring} does not point at the method"
        );
    }
    let all = stdout(repo.arc(&repo.home).args(["guide", "--all"]));
    assert_eq!(
        all.matches("Get the number first").count(),
        1,
        "the order of work has more than one home"
    );
}
