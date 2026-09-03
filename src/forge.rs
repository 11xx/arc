//! Recorded, validated forge (hosted-PR) projection facts.
//!
//! Arc never calls a forge API. Agents observe forge facts — the created
//! PR's repository tuple, its check rollup, its lifecycle — and RECORD them
//! through `arc forge declare/link/checks/pr-state`. This module owns the
//! value types those events carry, the fail-closed validation that a
//! recorded link matches what was declared, and the derived `forge` status
//! block. Everything here is a function of the append-only ledger; there is
//! no network, no autodetection, no `gh` invocation.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// The policy a declaration binds the observed link to. Validation of a
/// recorded link fails closed against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ForgePolicy {
    /// The PR must live entirely in one repository (base repo == head repo).
    SameRepositoryOnly,
    /// The PR's base repository must equal this exact `owner/name`.
    AllowedBaseRepo { repo: String },
}

impl ForgePolicy {
    /// Parse the `--policy` CLI value: `same-repository-only` (default) or
    /// `allowed-base-repo=<owner/name>`.
    pub fn parse(raw: &str) -> Result<Self> {
        if raw == "same-repository-only" {
            return Ok(ForgePolicy::SameRepositoryOnly);
        }
        if let Some(repo) = raw.strip_prefix("allowed-base-repo=") {
            if repo.trim().is_empty() {
                bail!("allowed-base-repo policy requires a non-empty <owner/name>");
            }
            return Ok(ForgePolicy::AllowedBaseRepo {
                repo: repo.to_string(),
            });
        }
        bail!(
            "invalid --policy {raw:?}; expected same-repository-only or \
             allowed-base-repo=<owner/name>"
        )
    }

    pub fn label(&self) -> String {
        match self {
            ForgePolicy::SameRepositoryOnly => "same-repository-only".to_string(),
            ForgePolicy::AllowedBaseRepo { repo } => format!("allowed-base-repo={repo}"),
        }
    }
}

/// Observed hosted-check rollup state. Zero hosted checks is never a green
/// result: `not-configured`/`not-triggered` are first-class, distinct from
/// `passed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeCheckState {
    NotConfigured,
    NotTriggered,
    Pending,
    Failed,
    Passed,
}

impl ForgeCheckState {
    pub fn as_str(self) -> &'static str {
        match self {
            ForgeCheckState::NotConfigured => "not-configured",
            ForgeCheckState::NotTriggered => "not-triggered",
            ForgeCheckState::Pending => "pending",
            ForgeCheckState::Failed => "failed",
            ForgeCheckState::Passed => "passed",
        }
    }
}

/// Observed PR lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ForgePrState {
    Open,
    Draft,
    Closed,
    Merged,
}

impl ForgePrState {
    pub fn as_str(self) -> &'static str {
        match self {
            ForgePrState::Open => "open",
            ForgePrState::Draft => "draft",
            ForgePrState::Closed => "closed",
            ForgePrState::Merged => "merged",
        }
    }
}

/// The explicit repository tuple carried by both a declaration and an
/// observed link. Validation compares two of these axis by axis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeTuple {
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
}

/// Latest-wins projection declaration, reduced from `forge-projection`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeProjectionRecord {
    pub host: String,
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
    pub policy: ForgePolicy,
}

impl ForgeProjectionRecord {
    pub fn tuple(&self) -> ForgeTuple {
        ForgeTuple {
            base_repo: self.base_repo.clone(),
            base_ref: self.base_ref.clone(),
            head_repo: self.head_repo.clone(),
            head_ref: self.head_ref.clone(),
        }
    }
}

/// Latest-wins observed link, reduced from `forge-link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeLinkRecord {
    /// The `forge-link` event that recorded this link. Lifecycle facts bind
    /// to it, so relinking cannot inherit the previous PR's state.
    pub event_id: String,
    pub pr_number: u64,
    pub url: String,
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
    pub head_sha: String,
}

impl ForgeLinkRecord {
    /// Whether two link observations name the same pull request at the same
    /// head. Re-recording an identical link is a second reading, not a move.
    pub fn same_pr_as(&self, other: &ForgeLinkRecord) -> bool {
        self.pr_number == other.pr_number
            && self.url == other.url
            && self.base_repo == other.base_repo
            && self.base_ref == other.base_ref
            && self.head_repo == other.head_repo
            && self.head_ref == other.head_ref
            && self.head_sha == other.head_sha
    }
}

/// Latest-wins observed check rollup, reduced from `forge-checks`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgeChecksRecord {
    pub pr_head: String,
    pub state: ForgeCheckState,
    pub detail: Option<String>,
}

/// One observed PR lifecycle fact, bound to the link and head it was read
/// at. Lifecycle observations accumulate rather than overwrite: an older
/// fact about a superseded PR is history, not the current state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForgePrStateRecord {
    pub state: ForgePrState,
    pub merge_sha: Option<String>,
    /// The link this was observed against. `None` predates binding.
    pub link_event_id: Option<String>,
    /// The PR head this was observed at. `None` predates binding.
    pub pr_head: Option<String>,
    /// The binding named a link this change had not recorded when the fact
    /// was written, or named only half of one. Neither describes any PR this
    /// ledger knows about, so the fact is never current — and it is not
    /// legacy either, because it did try to bind.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub unresolved_binding: bool,
}

/// Accumulated forge facts for one change: the latest of each observed
/// event kind. Absent everywhere until the first forge event.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ForgeState {
    pub projection: Option<ForgeProjectionRecord>,
    pub link: Option<ForgeLinkRecord>,
    pub checks: Option<ForgeChecksRecord>,
    /// Every observed lifecycle fact, oldest first. The current state is
    /// whichever one matches the current link and head, not the newest.
    pub pr_states: Vec<ForgePrStateRecord>,
    /// Every `forge-link` event, in order, paired with the link it recorded.
    /// Re-recording the same link is a second reading, not a relink, so the
    /// history is kept by content rather than counted by event.
    pub links: Vec<ForgeLinkRecord>,
}

impl ForgeState {
    /// Follow every revision these observations name forward through the
    /// recorded rewrites.
    ///
    /// A forge records revisions the same way the ledger does, and a rewritten
    /// branch strands them the same way. The observation itself is unchanged:
    /// what the host reported at a head is still what it reported, and this
    /// says which commit that head is here.
    pub fn follow_rewrites(&mut self, rewrites: &crate::rewrite::RewriteMap) {
        if let Some(link) = &mut self.link {
            rewrites.advance(&mut link.head_sha);
        }
        for link in &mut self.links {
            rewrites.advance(&mut link.head_sha);
        }
        if let Some(checks) = &mut self.checks {
            rewrites.advance(&mut checks.pr_head);
        }
        for state in &mut self.pr_states {
            rewrites.advance_opt(&mut state.merge_sha);
            rewrites.advance_opt(&mut state.pr_head);
        }
    }

    /// The lifecycle fact that describes the current link and head, if one
    /// was observed there. Newest first, because a PR can legitimately be
    /// observed more than once at the same head.
    ///
    /// A fact written before lifecycle binding names no link or head. It is
    /// read as current only while the change has recorded exactly one link:
    /// with nothing to have relinked from, there is no other PR it could
    /// have described.
    pub fn current_pr_state(&self) -> Option<&ForgePrStateRecord> {
        let link = self.link.as_ref()?;
        // Recording the same link twice is one PR observed twice, not a
        // relink, so a fact bound to any event that recorded this exact link
        // still describes it. Comparing event IDs alone would discard a good
        // observation because somebody re-read the same PR.
        let same_link: Vec<&str> = self
            .links
            .iter()
            .filter(|candidate| candidate.same_pr_as(link))
            .map(|candidate| candidate.event_id.as_str())
            .collect();
        self.pr_states.iter().rev().find(|record| {
            if record.unresolved_binding {
                return false;
            }
            match (&record.link_event_id, &record.pr_head) {
                (Some(event), Some(head)) => {
                    same_link.contains(&event.as_str()) && *head == link.head_sha
                }
                // Unbound predates binding entirely, and is read as current
                // only while this change has recorded one distinct link.
                (None, None) => !self.relinked(),
                // Half a binding describes nothing.
                _ => false,
            }
        })
    }

    /// Whether this change has recorded more than one distinct link.
    pub fn relinked(&self) -> bool {
        self.links
            .iter()
            .any(|candidate| !candidate.same_pr_as(&self.links[0]))
    }

    /// Whether this change has recorded any forge fact at all.
    pub fn is_empty(&self) -> bool {
        self.projection.is_none()
            && self.link.is_none()
            && self.checks.is_none()
            && self.pr_states.is_empty()
            && self.links.is_empty()
    }
}

/// A specific way an observed link fails its declared policy or tuple.
/// These are the only paths to exit 10; each names one refused axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeLinkRefusal {
    Undeclared,
    TupleMismatch {
        axis: &'static str,
        declared: String,
        observed: String,
    },
    SameRepositoryViolated {
        base_repo: String,
        head_repo: String,
    },
    AllowedBaseRepoViolated {
        required: String,
        observed: String,
    },
}

impl ForgeLinkRefusal {
    pub fn message(&self) -> String {
        match self {
            ForgeLinkRefusal::Undeclared => {
                "no forge projection declared; run `arc forge declare` before linking".to_string()
            }
            ForgeLinkRefusal::TupleMismatch {
                axis,
                declared,
                observed,
            } => {
                format!("observed {axis} {observed:?} does not match declared {axis} {declared:?}")
            }
            ForgeLinkRefusal::SameRepositoryViolated {
                base_repo,
                head_repo,
            } => format!(
                "policy same-repository-only requires base repo == head repo, \
                 but observed base {base_repo:?} != head {head_repo:?}"
            ),
            ForgeLinkRefusal::AllowedBaseRepoViolated { required, observed } => format!(
                "policy allowed-base-repo requires base repo {required:?}, \
                 but observed base {observed:?}"
            ),
        }
    }
}

/// Fail-closed validation of an observed link against the declared
/// projection. Returns the refusal on any violated axis; the caller must
/// append no event and exit 10 when this is `Err`.
pub fn validate_link(
    projection: Option<&ForgeProjectionRecord>,
    observed: &ForgeTuple,
) -> std::result::Result<(), ForgeLinkRefusal> {
    let Some(declared) = projection else {
        return Err(ForgeLinkRefusal::Undeclared);
    };
    let declared_tuple = declared.tuple();
    for (axis, declared_value, observed_value) in [
        ("base-repo", &declared_tuple.base_repo, &observed.base_repo),
        ("base-ref", &declared_tuple.base_ref, &observed.base_ref),
        ("head-repo", &declared_tuple.head_repo, &observed.head_repo),
        ("head-ref", &declared_tuple.head_ref, &observed.head_ref),
    ] {
        if declared_value != observed_value {
            return Err(ForgeLinkRefusal::TupleMismatch {
                axis,
                declared: declared_value.clone(),
                observed: observed_value.clone(),
            });
        }
    }
    match &declared.policy {
        ForgePolicy::SameRepositoryOnly => {
            if observed.base_repo != observed.head_repo {
                return Err(ForgeLinkRefusal::SameRepositoryViolated {
                    base_repo: observed.base_repo.clone(),
                    head_repo: observed.head_repo.clone(),
                });
            }
        }
        ForgePolicy::AllowedBaseRepo { repo } => {
            if &observed.base_repo != repo {
                return Err(ForgeLinkRefusal::AllowedBaseRepoViolated {
                    required: repo.clone(),
                    observed: observed.base_repo.clone(),
                });
            }
        }
    }
    Ok(())
}

// ---- Status rendering ------------------------------------------------------

/// Serialized view of the declared projection tuple and policy.
#[derive(Debug, Clone, Serialize)]
pub struct ForgeProjectionView {
    pub host: String,
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
    pub policy: String,
}

/// Serialized view of the observed link.
#[derive(Debug, Clone, Serialize)]
pub struct ForgeLinkView {
    pub pr_number: u64,
    pub url: String,
    pub base_repo: String,
    pub base_ref: String,
    pub head_repo: String,
    pub head_ref: String,
    pub head_sha: String,
}

/// Serialized view of the observed PR lifecycle.
#[derive(Debug, Clone, Serialize)]
pub struct ForgePrStateView {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_sha: Option<String>,
}

/// A held+linked change awaiting a user decision on its open PR.
#[derive(Debug, Clone, Serialize)]
pub struct AwaitingUser {
    pub pr_url: String,
    pub head_sha: String,
}

/// The `forge` block of `arc status`/`arc show`, derived from the latest
/// projection/link/checks/pr-state facts joined with the current head.
#[derive(Debug, Clone, Serialize)]
pub struct ForgeStatus {
    /// undeclared | declared | linked
    pub projection: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared: Option<ForgeProjectionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<ForgeLinkView>,
    /// Whether the linked head equals the current approved patchset head.
    pub head_match: bool,
    /// not-configured | not-triggered | pending | failed | passed | stale | unknown
    pub checks: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_state: Option<ForgePrStateView>,
    pub forge_ready: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awaiting_user: Option<AwaitingUser>,
}

/// Build the `forge` status block. Returns `None` when the change has no
/// forge facts and is not on the `forge` profile — the block is absent
/// rather than empty. `approved_head` is the current approved patchset
/// head (the exact-head rule compares the linked head against it); `held`
/// carries the active hold so a held+linked PR renders `awaiting-user`.
pub fn build_status(
    forge: &ForgeState,
    profile: &str,
    approved_head: Option<&str>,
    held: bool,
) -> Option<ForgeStatus> {
    if forge.is_empty() && profile != "forge" {
        return None;
    }

    let projection = match (&forge.projection, &forge.link) {
        (_, Some(_)) => "linked",
        (Some(_), None) => "declared",
        (None, None) => "undeclared",
    };

    let declared = forge.projection.as_ref().map(|p| ForgeProjectionView {
        host: p.host.clone(),
        base_repo: p.base_repo.clone(),
        base_ref: p.base_ref.clone(),
        head_repo: p.head_repo.clone(),
        head_ref: p.head_ref.clone(),
        policy: p.policy.label(),
    });

    let link = forge.link.as_ref().map(|l| ForgeLinkView {
        pr_number: l.pr_number,
        url: l.url.clone(),
        base_repo: l.base_repo.clone(),
        base_ref: l.base_ref.clone(),
        head_repo: l.head_repo.clone(),
        head_ref: l.head_ref.clone(),
        head_sha: l.head_sha.clone(),
    });

    let linked_head = forge.link.as_ref().map(|l| l.head_sha.as_str());
    let head_match = match (linked_head, approved_head) {
        (Some(linked), Some(approved)) => linked == approved,
        _ => false,
    };

    // Checks are read relative to the linked head. Without a link there is
    // no head to compare, so the rollup is `unknown`. A rollup recorded for
    // a different head than the linked one is `stale`, never trusted.
    let (checks, checks_detail) = match (&forge.checks, linked_head) {
        (Some(record), Some(head)) if record.pr_head == head => {
            (record.state.as_str(), record.detail.clone())
        }
        (Some(record), Some(_)) => ("stale", record.detail.clone()),
        (Some(_), None) | (None, _) => ("unknown", None),
    };

    // Lifecycle facts are read at a specific PR and head. Pairing the newest
    // one with the current link would let a fact about a superseded PR speak
    // for its replacement, which is exactly how `forge_ready` could go true
    // after a relink. An observation that cannot be shown to describe the
    // current link and head leaves the state unknown instead.
    let current_pr_state = forge.current_pr_state();
    let pr_state = current_pr_state.map(|p| ForgePrStateView {
        state: p.state.as_str().to_string(),
        merge_sha: p.merge_sha.clone(),
    });

    let mut caveats = Vec::new();
    if current_pr_state.is_none() && !forge.pr_states.is_empty() {
        caveats.push(
            match forge.link {
                None => "pr-state unknown: lifecycle facts are recorded but no link is",
                Some(_) => {
                    "pr-state unknown: no recorded lifecycle fact was observed at this link and \
                     head"
                }
            }
            .to_string(),
        );
    }
    if checks == "not-configured" {
        caveats
            .push("checks not-configured: zero hosted checks is not a passing result".to_string());
    }

    let pr_open = matches!(current_pr_state.map(|p| p.state), Some(ForgePrState::Open));
    let checks_ok = matches!(checks, "passed" | "not-configured");
    let forge_ready = forge.link.is_some() && head_match && checks_ok && pr_open;

    let awaiting_user = if held {
        forge.link.as_ref().map(|l| AwaitingUser {
            pr_url: l.url.clone(),
            head_sha: l.head_sha.clone(),
        })
    } else {
        None
    };

    Some(ForgeStatus {
        projection,
        declared,
        link,
        head_match,
        checks,
        checks_detail,
        pr_state,
        forge_ready,
        caveats,
        awaiting_user,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> ForgeProjectionRecord {
        ForgeProjectionRecord {
            host: "github.com".into(),
            base_repo: "11xx/streamrip".into(),
            base_ref: "dev".into(),
            head_repo: "11xx/streamrip".into(),
            head_ref: "arc/x".into(),
            policy: ForgePolicy::SameRepositoryOnly,
        }
    }

    fn observed(p: &ForgeProjectionRecord) -> ForgeTuple {
        p.tuple()
    }

    fn link(event_id: &str, pr: u64, head: &str) -> ForgeLinkRecord {
        ForgeLinkRecord {
            event_id: event_id.into(),
            pr_number: pr,
            url: format!("https://example.invalid/pull/{pr}"),
            base_repo: "o/r".into(),
            base_ref: "main".into(),
            head_repo: "o/r".into(),
            head_ref: "arc/x".into(),
            head_sha: head.into(),
        }
    }

    fn pr_state(link_event_id: Option<&str>, pr_head: Option<&str>) -> ForgePrStateRecord {
        ForgePrStateRecord {
            state: ForgePrState::Open,
            merge_sha: None,
            link_event_id: link_event_id.map(str::to_string),
            pr_head: pr_head.map(str::to_string),
            unresolved_binding: false,
        }
    }

    fn state_with(links: Vec<ForgeLinkRecord>, pr_states: Vec<ForgePrStateRecord>) -> ForgeState {
        ForgeState {
            projection: None,
            link: links.last().cloned(),
            checks: None,
            pr_states,
            links,
        }
    }

    /// A fact recorded before binding existed names no link. It describes the
    /// current PR only while there has been one, because with nothing to have
    /// relinked from there is no other PR it could have been about.
    #[test]
    fn an_unbound_fact_is_current_until_the_change_relinks() {
        let one = link("01A", 1, "aaa");
        let unbound = pr_state(None, None);
        let single = state_with(vec![one.clone()], vec![unbound.clone()]);
        assert_eq!(single.current_pr_state(), Some(&unbound));

        let relinked = state_with(vec![one.clone(), link("01B", 2, "bbb")], vec![unbound]);
        assert_eq!(relinked.current_pr_state(), None);

        // Reading the same PR twice is not a relink.
        let re_read = state_with(
            vec![one.clone(), link("01B", 1, "aaa")],
            vec![pr_state(None, None)],
        );
        assert!(re_read.current_pr_state().is_some());
    }

    /// A bound fact follows the PR it named, not the event that recorded the
    /// link, so re-reading the same PR does not discard it.
    #[test]
    fn a_bound_fact_survives_a_second_reading_of_the_same_link() {
        let bound = pr_state(Some("01A"), Some("aaa"));
        let state = state_with(
            vec![link("01A", 1, "aaa"), link("01B", 1, "aaa")],
            vec![bound.clone()],
        );
        assert_eq!(state.current_pr_state(), Some(&bound));

        // A different PR at the same head is a relink, and the old fact was
        // about the old PR.
        let moved = state_with(
            vec![link("01A", 1, "aaa"), link("01B", 2, "aaa")],
            vec![bound],
        );
        assert_eq!(moved.current_pr_state(), None);
    }

    /// Half a binding describes nothing, and neither does one naming a link
    /// this change never recorded. Reading either as legacy would let an
    /// imported or hand-edited fact speak for the current PR.
    #[test]
    fn a_partial_or_unresolved_binding_is_never_current() {
        let one = link("01A", 1, "aaa");
        for broken in [
            pr_state(Some("01A"), None),
            pr_state(None, Some("aaa")),
            ForgePrStateRecord {
                unresolved_binding: true,
                ..pr_state(Some("01A"), Some("aaa"))
            },
        ] {
            let state = state_with(vec![one.clone()], vec![broken.clone()]);
            assert_eq!(state.current_pr_state(), None, "{broken:?}");
        }
    }

    #[test]
    fn policy_parses_both_forms() {
        assert_eq!(
            ForgePolicy::parse("same-repository-only").unwrap(),
            ForgePolicy::SameRepositoryOnly
        );
        assert_eq!(
            ForgePolicy::parse("allowed-base-repo=a/b").unwrap(),
            ForgePolicy::AllowedBaseRepo { repo: "a/b".into() }
        );
        assert!(ForgePolicy::parse("nonsense").is_err());
        assert!(ForgePolicy::parse("allowed-base-repo=").is_err());
    }

    #[test]
    fn matching_link_validates() {
        let p = declared();
        assert!(validate_link(Some(&p), &observed(&p)).is_ok());
    }

    #[test]
    fn undeclared_refuses() {
        let p = declared();
        assert_eq!(
            validate_link(None, &observed(&p)),
            Err(ForgeLinkRefusal::Undeclared)
        );
    }

    #[test]
    fn each_tuple_axis_refuses() {
        let p = declared();
        for (axis, mut t) in [
            ("base-repo", observed(&p)),
            ("base-ref", observed(&p)),
            ("head-repo", observed(&p)),
            ("head-ref", observed(&p)),
        ] {
            match axis {
                "base-repo" => t.base_repo = "other/repo".into(),
                "base-ref" => t.base_ref = "main".into(),
                "head-repo" => t.head_repo = "other/repo".into(),
                "head-ref" => t.head_ref = "arc/y".into(),
                _ => unreachable!(),
            }
            assert!(matches!(
                validate_link(Some(&p), &t),
                Err(ForgeLinkRefusal::TupleMismatch { .. })
            ));
        }
    }

    #[test]
    fn same_repository_only_rejects_cross_repo() {
        // A cross-repo tuple that still matches the declaration is refused
        // because the declared policy forbids base != head.
        let mut p = declared();
        p.head_repo = "nathom/streamrip".into();
        let t = observed(&p);
        assert!(matches!(
            validate_link(Some(&p), &t),
            Err(ForgeLinkRefusal::SameRepositoryViolated { .. })
        ));
    }

    #[test]
    fn allowed_base_repo_accepts_target_refuses_others() {
        let mut p = declared();
        p.head_repo = "nathom/streamrip".into();
        p.policy = ForgePolicy::AllowedBaseRepo {
            repo: "11xx/streamrip".into(),
        };
        assert!(validate_link(Some(&p), &observed(&p)).is_ok());

        p.policy = ForgePolicy::AllowedBaseRepo {
            repo: "someone/else".into(),
        };
        assert!(matches!(
            validate_link(Some(&p), &observed(&p)),
            Err(ForgeLinkRefusal::AllowedBaseRepoViolated { .. })
        ));
    }
}
