use serde::Serialize;

pub const CHAIN_SCHEMA: &str = "arc-chain/1";

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
    pub review: Option<ChainReview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainReview {
    pub subject: Option<String>,
    pub verdicts: usize,
    pub identities: Vec<String>,
    pub non_self_verdict: bool,
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
