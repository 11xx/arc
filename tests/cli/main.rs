mod audit;
mod briefs;
mod bundle;
mod chain;
mod changelog;
mod claims;
mod common;
mod context;
mod cross_links;
mod diff;
mod docs;
mod doctor;
mod findings;
mod forge;
mod fork;
mod gate_environment;
mod hooks;
mod instructions;
mod integrate;
mod journal;
mod lifecycle;
mod messaging;
mod metadata;
mod observe;
mod orchestrate;
mod pass;
mod paths;
mod planners;
mod policy;
mod provenance;
mod queue;
mod rebase;
mod release;
mod replicas;
mod rescue;
mod review;
mod rewrite;
mod roles;
mod run;
mod sandbox;
mod skip_green;
mod stats;
mod take;
mod timeline;
mod tree_gates;
mod verify;
mod workspace;

use common::Repo;

#[test]
fn doctor_groups_advice_and_ignores_closed_claims() {
    doctor::doctor_groups_advice_and_ignores_closed_claims();
}

#[test]
fn doctor_reports_closed_registered_worktrees_without_removing_them() {
    doctor::doctor_reports_closed_registered_worktrees_without_removing_them();
}

#[test]
fn journal_dir_longest_prefix_and_git_identity_preserve_existing_slugs() {
    journal::journal_dir_longest_prefix_and_git_identity_preserve_existing_slugs();
}

#[test]
fn nested_leaf_at_top_level_suggests_its_command_path() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["note"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("journal note"));
}

#[test]
fn top_level_typo_retains_clap_suggestion() {
    let repo = Repo::new();
    repo.arc(&repo.root)
        .args(["journl"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "a similar subcommand exists: 'journal'",
        ));
}

#[test]
fn operator_policy_unions_with_project_policy_without_tracked_changes() {
    policy::operator_policy_unions_with_project_policy_without_tracked_changes();
}

#[test]
fn conflicting_gate_commands_are_reported_and_refused() {
    policy::conflicting_gate_commands_are_reported_and_refused();
}

#[test]
fn layered_gates_take_the_stricter_timeout_and_refuse_a_different_environment() {
    policy::layered_gates_take_the_stricter_timeout_and_refuse_a_different_environment();
}
