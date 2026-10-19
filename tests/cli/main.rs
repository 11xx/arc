mod absent_actor;
mod absent_identity;
mod accept;
mod assurance;
mod audit;
mod briefs;
mod bundle;
mod candidate;
mod catchup_open_change_reads;
mod catchup_reads;
mod chain;
mod changelog;
mod checkout_defaults;
mod claims;
mod codex_model_checkpoint;
mod commit_lookup_gaps;
mod common;
mod context;
mod contribution;
mod cost;
mod cost_completeness;
mod cost_lineage_whole;
mod cost_participant_share;
mod cost_replay_once;
mod cost_report_ranges;
mod cost_span_split;
mod cross_links;
mod debt_plan;
mod debt_standing;
mod diff;
mod doctor;
mod explain;
mod external;
mod falsification_transport;
mod final_wave_cost;
mod findings;
mod forge;
mod forge_custody;
mod forge_observation;
mod forge_round_trip;
mod forge_view;
mod fork;
mod gate_declarations;
mod gate_environment;
mod guide_topics;
mod hooks;
mod implementer_trailers;
mod import_authorization;
mod ineligible_review;
mod instructions;
mod integrate;
mod journal;
mod journal_exchange;
mod lifecycle;
mod lookup_reasons;
mod messaging;
mod metadata;
mod model_resolution;
mod native_child_binding;
mod observe;
mod orchestrate;
mod pass;
mod paths;
mod placeholders;
mod planners;
mod policy;
mod provenance;
mod provisional;
mod queue;
mod rebase;
mod received_evidence;
mod registration_retirement;
mod relations;
mod release;
mod relocate_redact;
mod replicas;
mod rescue;
mod retention;
mod review;
mod review3_fixes_cost;
mod review_fixes_cost;
mod review_fixes_native_child;
mod review_fixes_span_split;
mod rewrite;
mod roles;
mod run;
mod sandbox;
mod schemas;
mod selection;
mod skip_green;
mod stats;
mod take;
mod timeline;
mod trailer_edits;
mod trailer_projection;
mod tree_gates;
mod verify;
mod workspace;

use common::{PredicateBooleanExt, Repo};

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

/// A value that starts with `-` reads as the next option. The refusal names
/// it and the attached form that passes it, not a similarly spelled option or
/// a trailing positional.
#[test]
fn a_hyphenated_option_value_names_the_attached_form() {
    let repo = Repo::new();
    for (args, typed) in [
        (
            ["keep", "x", "--kind", "verified", "--evidence", "--at 2026"],
            "--at 2026",
        ),
        (
            ["keep", "x", "--kind", "verified", "--evidence", "--at"],
            "--at",
        ),
    ] {
        repo.arc(&repo.root)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicates::str::contains(format!(
                "unexpected argument '{typed}' found"
            )))
            .stderr(predicates::str::contains(format!(
                "attach it: '--evidence={typed}'"
            )))
            .stderr(predicates::str::contains("--actor").not())
            .stderr(predicates::str::contains(format!("'-- {typed}'")).not());
    }
    // An option that takes no value leaves clap's own reading of the token.
    repo.arc(&repo.root)
        .args(["integrate", "x", "--dry-run", "--nope"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "unexpected argument '--nope' found",
        ))
        .stderr(predicates::str::contains("attach it").not());
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
