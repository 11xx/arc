use serde::Serialize;

pub const CHAIN_SCHEMA: &str = "arc-chain/2";

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
    pub non_self_verdict: bool,
    pub at_final: ChainReviewWindow,
    pub lifetime: ChainReviewWindow,
    /// Per reviewer, the newest patchset it saw. `non_self_verdict` answers
    /// whether somebody independent ever looked; this answers whether anybody
    /// looked at what shipped, which is the question that catches a panel
    /// running correctly right up to the corrections nobody re-reviewed.
    pub coverage: Vec<crate::status::ReviewerCoverage>,
    /// Reviewers whose last look predates the final patchset.
    pub stale_reviewers: usize,
    /// A review obligation recorded at integration and not yet discharged.
    pub audit_debt_outstanding: bool,
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
