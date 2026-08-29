use serde::Serialize;

pub const CHAIN_SCHEMA: &str = "arc-chain/4";

#[derive(Debug, Clone, Serialize)]
pub struct ChainMember {
    pub change_id: String,
    pub slug: String,
    pub title: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_slice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<ChainReview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainReview {
    pub subject: Option<String>,
    /// Who wrote the brief the work was done from, when there is one. In an
    /// orchestrated chain this is the lead, and the verdict usually comes from
    /// the same identity — a fact worth reporting, and one arc holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_author: Option<String>,
    /// Whether every verdict in the lifetime window came from the brief's
    /// author. Emitted as null when there is no brief, or no verdict to
    /// attribute — unlike `brief_author`, which is simply absent, here the
    /// absence of an answer is the answer and a consumer should see it.
    ///
    /// This makes no inference about independence: arc cannot know that a
    /// reviewer directed the work, only that the same identity wrote the
    /// brief and the verdict.
    pub reviewed_only_by_brief_author: Option<bool>,
    pub at_final: ChainReviewWindow,
    pub lifetime: ChainReviewWindow,
    /// Per reviewer, the newest patchset it saw. This answers whether anybody
    /// looked at what shipped, which is the question that catches a panel
    /// running correctly right up to the corrections nobody re-reviewed.
    pub coverage: Vec<crate::status::ReviewerCoverage>,
    /// Reviewers whose last look predates the final patchset.
    pub stale_reviewers: usize,
    /// A review obligation recorded at integration and not yet discharged.
    pub debt_outstanding: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainReviewWindow {
    pub verdicts: usize,
    pub identities: Vec<String>,
    pub findings: usize,
    pub ad_hoc_verifications: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainPlan {
    pub plan_ref: String,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Chain {
    pub schema: &'static str,
    pub tag: String,
    pub members: Vec<ChainMember>,
    pub plans: Vec<ChainPlan>,
    pub next_ready: Option<String>,
}
