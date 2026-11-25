//! Offline conversion of pre-fetched forge provider JSON into
//! `arc-forge-observation/1` documents and verified custody byte sets.
//!
//! Arc never contacts a provider. Callers supply pre-fetched JSON (such as
//! `gh pr view --json`, GitHub REST API responses, or MCP `pull_request_read`
//! output), and this module constructs a validated observation that reuses
//! unchanged generations and chains `previous_observation` from the change's
//! latest usable admitted read.

use crate::forge::{ForgeCheckState, ForgePrState};
use crate::forge_observation::{
    Coverage, CoverageState, ForgeObservation, Generation, Item, ItemFacts, ObservationRecord,
    ProviderIdentity, ReadCoverage, Response, RetainedContentRef,
};
use crate::model::CaptureState;
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum InputFormat {
    Auto,
    Gh,
    GithubRest,
    Mcp,
}

impl InputFormat {
    fn adapter_tag(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gh => "gh",
            Self::GithubRest => "github-rest",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct IngestOverrides {
    pub provider: Option<String>,
    pub host: Option<String>,
    pub repository: Option<String>,
    pub pr_number: Option<u64>,
    pub url: Option<String>,
    pub base_repo: Option<String>,
    pub base_ref: Option<String>,
    pub head_repo: Option<String>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub read_id: Option<String>,
    pub read_at: Option<DateTime<Utc>>,
    pub adapter: Option<String>,
    pub truncated: Vec<String>,
}

pub struct SupplementalInput<'a> {
    pub response_id: &'static str,
    pub handle: &'a str,
    pub bytes: &'a [u8],
}

pub struct BuiltObservation {
    pub document: ForgeObservation,
    /// Verified bytes keyed by lowercase SHA-256 digest for every response and
    /// item reference carried by `document`.
    pub blobs: BTreeMap<String, Vec<u8>>,
}

/// Write `DIR/sha256/<digest>` files matching `arc forge observe --custody-dir`.
pub fn write_custody_dir(dir: &Path, blobs: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let sha_dir = dir.join("sha256");
    std::fs::create_dir_all(&sha_dir)
        .with_context(|| format!("cannot create {}", sha_dir.display()))?;
    for (digest, bytes) in blobs {
        let path = sha_dir.join(digest);
        std::fs::write(&path, bytes).with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}

/// Minimal valid `arc-forge-observation/1` document with self-consistent
/// SHA-256 digests and lengths, demonstrating pull-request, checks, and
/// comment items.
pub fn example_observation() -> ForgeObservation {
    let response_body = br#"{"number":42,"state":"OPEN"}"#;
    let pr_body = br#"{"base_ref":"main","head_ref":"feature","pr_number":42,"state":"open"}"#;
    let checks_body = br#"{"count":1,"state":"passed"}"#;
    let comment_body = b"Looks good after addressing the edge case.";

    let make_ref = |handle: &str, bytes: &[u8], media_type: &str, capture: CaptureState| {
        let sha256 = sha256_hex(bytes);
        let byte_length = bytes.len() as u64;
        (
            sha256.clone(),
            byte_length,
            RetainedContentRef {
                provider: "github".into(),
                handle: handle.into(),
                sha256,
                byte_length,
                media_type: media_type.into(),
                capture,
            },
        )
    };

    let (resp_sha, resp_len, resp_ref) = make_ref(
        "response:pr-42",
        response_body,
        "application/json",
        CaptureState::Unpinned,
    );
    let (pr_sha, pr_len, pr_ref) = make_ref(
        "pull-request:pr-42",
        pr_body,
        "application/json",
        CaptureState::Unpinned,
    );
    let (checks_sha, checks_len, checks_ref) = make_ref(
        "checks:checks-rollup",
        checks_body,
        "application/json",
        CaptureState::Unpinned,
    );
    let (comment_sha, comment_len, comment_ref) = make_ref(
        "comment:IC_example_1",
        comment_body,
        "text/plain",
        CaptureState::Pinned,
    );

    let mut collections = BTreeMap::new();
    for (name, scope) in [
        ("pull-request", "pull request metadata"),
        ("checks", "status check rollup"),
        ("comments", "issue and review comments"),
    ] {
        collections.insert(
            name.into(),
            Coverage {
                state: CoverageState::Complete,
                scope: scope.into(),
                frontier: None,
                failure_reason: None,
            },
        );
    }

    let read_at = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("static timestamp")
        .with_timezone(&Utc);

    ForgeObservation {
        schema: "arc-forge-observation/1".into(),
        read_id: "read-example-1".into(),
        adapter: "example/1".into(),
        provider_identity: ProviderIdentity {
            provider: "github".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            repository_id: Some("R_example".into()),
            pr_number: 42,
            native_id: Some("PR_example".into()),
            url: "https://github.com/owner/repo/pull/42".into(),
            base_repo: "owner/repo".into(),
            base_ref: "main".into(),
            head_repo: "owner/repo".into(),
            head_ref: "feature".into(),
        },
        observed_head_sha: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
        observed_base_sha: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
        read_at,
        provider_updated_at: Some(read_at),
        provider_event_id: None,
        coverage: ReadCoverage {
            overall: Coverage {
                state: CoverageState::Complete,
                scope: "pull-request, checks, comments".into(),
                frontier: None,
                failure_reason: None,
            },
            collections,
        },
        responses: vec![Response {
            response_id: "payload".into(),
            sha256: resp_sha,
            byte_length: resp_len,
            content: resp_ref,
        }],
        items: vec![
            Item {
                kind: "pull-request".into(),
                node_id: "PR_example".into(),
                generation_id: "gen-pr-1".into(),
                sha256: pr_sha.clone(),
                byte_length: pr_len,
                content: pr_ref,
                facts: ItemFacts::PullRequest {
                    state: ForgePrState::Open,
                    merge_sha: None,
                },
            },
            Item {
                kind: "checks".into(),
                node_id: "checks-rollup".into(),
                generation_id: "gen-checks-1".into(),
                sha256: checks_sha.clone(),
                byte_length: checks_len,
                content: checks_ref,
                facts: ItemFacts::Checks {
                    state: ForgeCheckState::Passed,
                    count: 1,
                },
            },
            Item {
                kind: "comment".into(),
                node_id: "IC_example_1".into(),
                generation_id: "gen-comment-1".into(),
                sha256: comment_sha.clone(),
                byte_length: comment_len,
                content: comment_ref,
                facts: ItemFacts::Comment,
            },
        ],
        generations: vec![
            Generation {
                generation_id: "gen-pr-1".into(),
                predecessor: None,
                kind: "pull-request".into(),
                node_id: "PR_example".into(),
                sha256: pr_sha,
            },
            Generation {
                generation_id: "gen-checks-1".into(),
                predecessor: None,
                kind: "checks".into(),
                node_id: "checks-rollup".into(),
                sha256: checks_sha,
            },
            Generation {
                generation_id: "gen-comment-1".into(),
                predecessor: None,
                kind: "comment".into(),
                node_id: "IC_example_1".into(),
                sha256: comment_sha,
            },
        ],
        previous_observation: None,
    }
}

/// Build and validate an `arc-forge-observation/1` document and its custody
/// byte map from pre-fetched provider JSON and the change's prior observations.
#[allow(clippy::too_many_arguments)]
pub fn build_observation(
    primary_bytes: &[u8],
    primary_handle: &str,
    comments_input: Option<SupplementalInput<'_>>,
    reviews_input: Option<SupplementalInput<'_>>,
    checks_input: Option<SupplementalInput<'_>>,
    format: InputFormat,
    overrides: &IngestOverrides,
    history: &[ObservationRecord],
) -> Result<BuiltObservation> {
    let root: Value =
        serde_json::from_slice(primary_bytes).context("primary payload is not valid JSON")?;
    ensure!(
        root.is_object(),
        "primary provider payload must be a JSON object"
    );

    let pr_obj = root
        .get("pull_request")
        .or_else(|| root.get("pullRequest"))
        .or_else(|| root.get("pr"))
        .filter(|v| v.is_object())
        .unwrap_or(&root);

    let detected = match format {
        InputFormat::Auto => detect_format(pr_obj, &root),
        explicit => explicit,
    };

    let provider = overrides
        .provider
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "github".into());

    let raw_url = overrides
        .url
        .clone()
        .or_else(|| str_field(pr_obj, &["url", "html_url", "htmlUrl"]));
    let parsed_url = raw_url.as_deref().and_then(parse_pr_url);

    let host = overrides
        .host
        .clone()
        .or_else(|| str_field(pr_obj, &["host"]))
        .or_else(|| parsed_url.as_ref().map(|u| u.host.clone()))
        .unwrap_or_else(|| "github.com".into());

    let pr_number = overrides
        .pr_number
        .or_else(|| u64_field(pr_obj, &["number", "pr_number", "prNumber"]))
        .or_else(|| parsed_url.as_ref().map(|u| u.pr_number))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider payload has no PR number; include `number`/`url` or pass --pr <N>"
            )
        })?;
    ensure!(pr_number > 0, "PR number must be positive");

    let base_repo_from_payload = pr_obj
        .get("base")
        .and_then(|b| b.get("repo"))
        .and_then(|r| str_field(r, &["full_name", "nameWithOwner"]))
        .or_else(|| {
            pr_obj
                .get("baseRepository")
                .and_then(|r| str_field(r, &["nameWithOwner", "full_name"]))
        })
        .or_else(|| str_field(pr_obj, &["base_repo", "baseRepo", "repository"]));

    let repository = overrides
        .repository
        .clone()
        .or_else(|| parsed_url.as_ref().map(|u| u.repository.clone()))
        .or_else(|| base_repo_from_payload.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider payload has no repository (OWNER/REPO); include `url`/`repository` or pass --repo OWNER/REPO"
            )
        })?;

    let base_repo = overrides
        .base_repo
        .clone()
        .or(base_repo_from_payload)
        .unwrap_or_else(|| repository.clone());

    let head_repo = overrides
        .head_repo
        .clone()
        .or_else(|| {
            pr_obj
                .get("head")
                .and_then(|h| h.get("repo"))
                .and_then(|r| str_field(r, &["full_name", "nameWithOwner"]))
        })
        .or_else(|| {
            pr_obj.get("headRepository").and_then(|r| {
                str_field(r, &["nameWithOwner", "full_name"]).or_else(|| {
                    let name = str_field(r, &["name"])?;
                    let owner = pr_obj
                        .get("headRepositoryOwner")
                        .and_then(|o| str_field(o, &["login"]))
                        .or_else(|| r.get("owner").and_then(|o| str_field(o, &["login"])))
                        .or_else(|| repository.split_once('/').map(|(o, _)| o.to_string()))?;
                    Some(format!("{owner}/{name}"))
                })
            })
        })
        .or_else(|| str_field(pr_obj, &["head_repo", "headRepo"]))
        .unwrap_or_else(|| repository.clone());

    let base_ref = overrides
        .base_ref
        .clone()
        .or_else(|| pr_obj.get("base").and_then(|b| str_field(b, &["ref"])))
        .or_else(|| str_field(pr_obj, &["baseRefName", "baseBranch", "baseRef", "base_ref"]))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider payload has no base ref; include `baseRefName`/`base.ref`/`baseBranch` or pass --base-ref <REF>"
            )
        })?;

    let head_ref = overrides
        .head_ref
        .clone()
        .or_else(|| pr_obj.get("head").and_then(|h| str_field(h, &["ref"])))
        .or_else(|| str_field(pr_obj, &["headRefName", "headBranch", "headRef", "head_ref"]))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider payload has no head ref; include `headRefName`/`head.ref`/`headBranch` or pass --head-ref <REF>"
            )
        })?;

    let url = raw_url.unwrap_or_else(|| format!("https://{host}/{repository}/pull/{pr_number}"));

    let repository_id = pr_obj
        .get("base")
        .and_then(|b| b.get("repo"))
        .and_then(|r| id_field(r, &["node_id", "id"]))
        .or_else(|| {
            pr_obj
                .get("baseRepository")
                .and_then(|r| id_field(r, &["id", "node_id"]))
        })
        .or_else(|| str_field(pr_obj, &["repository_id", "repositoryId"]));

    let native_id = id_field(pr_obj, &["node_id", "id", "native_id", "nativeId"]);

    let observed_head_sha = overrides
        .head_sha
        .clone()
        .or_else(|| pr_obj.get("head").and_then(|h| str_field(h, &["sha"])))
        .or_else(|| {
            str_field(
                pr_obj,
                &["headRefOid", "headSha", "headOid", "head_sha", "observed_head_sha"],
            )
        })
        .map(|s| normalize_sha("observed head SHA", &s))
        .transpose()?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider payload has no observed head SHA; include `headRefOid`/`head.sha`/`headSha` or pass --head-sha <SHA>"
            )
        })?;

    let observed_base_sha = overrides
        .base_sha
        .clone()
        .or_else(|| pr_obj.get("base").and_then(|b| str_field(b, &["sha"])))
        .or_else(|| {
            str_field(
                pr_obj,
                &["baseRefOid", "baseSha", "baseOid", "base_sha", "observed_base_sha"],
            )
        })
        .map(|s| normalize_sha("observed base SHA", &s))
        .transpose()?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider payload has no observed base SHA; include `baseRefOid`/`base.sha`/`baseSha` or pass --base-sha <SHA>"
            )
        })?;

    let (pr_state, merge_sha) = extract_pr_state(pr_obj)?;

    let provider_updated_at = str_field(pr_obj, &["updatedAt", "updated_at"])
        .map(|s| {
            DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&Utc))
                .with_context(|| format!("invalid provider updated timestamp {s:?}"))
        })
        .transpose()?;

    let provider_identity = ProviderIdentity {
        provider: provider.clone(),
        host,
        repository,
        repository_id,
        pr_number,
        native_id: native_id.clone(),
        url,
        base_repo,
        base_ref,
        head_repo,
        head_ref,
    };

    let truncated_flags: BTreeSet<String> = overrides
        .truncated
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    let mut blobs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut responses: Vec<Response> = Vec::new();
    let mut record_response = |response_id: &str, handle: &str, bytes: &[u8]| {
        let sha256 = sha256_hex(bytes);
        let byte_length = bytes.len() as u64;
        blobs
            .entry(sha256.clone())
            .or_insert_with(|| bytes.to_vec());
        responses.push(Response {
            response_id: response_id.into(),
            sha256: sha256.clone(),
            byte_length,
            content: RetainedContentRef {
                provider: provider.clone(),
                handle: handle.into(),
                sha256,
                byte_length,
                media_type: "application/json".into(),
                capture: CaptureState::Unpinned,
            },
        });
    };

    record_response("payload", primary_handle, primary_bytes);

    let comments_extra = comments_input
        .map(|input| -> Result<Value> {
            record_response(input.response_id, input.handle, input.bytes);
            serde_json::from_slice(input.bytes).context("comments file is not valid JSON")
        })
        .transpose()?;
    let reviews_extra = reviews_input
        .map(|input| -> Result<Value> {
            record_response(input.response_id, input.handle, input.bytes);
            serde_json::from_slice(input.bytes).context("reviews file is not valid JSON")
        })
        .transpose()?;
    let checks_extra = checks_input
        .map(|input| -> Result<Value> {
            record_response(input.response_id, input.handle, input.bytes);
            serde_json::from_slice(input.bytes).context("checks file is not valid JSON")
        })
        .transpose()?;

    let mut collections: BTreeMap<String, Coverage> = BTreeMap::new();
    let mut raw_items: Vec<RawItem> = Vec::new();

    // 1. Pull request item
    let pr_node_id = native_id
        .clone()
        .unwrap_or_else(|| format!("pr-{pr_number}"));
    let pr_title = str_field(pr_obj, &["title"]);
    let pr_body = str_field(pr_obj, &["body"]);
    let pr_draft = pr_obj
        .get("isDraft")
        .or_else(|| pr_obj.get("draft"))
        .and_then(Value::as_bool);
    let pr_canonical = serde_json::to_vec(&json!({
        "base_ref": provider_identity.base_ref,
        "base_repo": provider_identity.base_repo,
        "body": pr_body,
        "draft": pr_draft,
        "head_ref": provider_identity.head_ref,
        "head_repo": provider_identity.head_repo,
        "merge_sha": merge_sha,
        "observed_base_sha": observed_base_sha,
        "observed_head_sha": observed_head_sha,
        "pr_number": pr_number,
        "state": pr_state.as_str(),
        "title": pr_title,
    }))?;
    raw_items.push(RawItem {
        kind: "pull-request".into(),
        node_id: pr_node_id.clone(),
        handle: format!("pull-request:{pr_node_id}"),
        media_type: "application/json".into(),
        capture: CaptureState::Unpinned,
        bytes: pr_canonical,
        facts: ItemFacts::PullRequest {
            state: pr_state,
            merge_sha: merge_sha.clone(),
        },
    });
    let pr_truncated = truncated_flags.contains("pull-request");
    collections.insert(
        "pull-request".into(),
        Coverage {
            state: if pr_truncated {
                CoverageState::Truncated
            } else {
                CoverageState::Complete
            },
            scope: "pull request metadata".into(),
            frontier: None,
            failure_reason: None,
        },
    );

    // 2. Checks rollup (only when present in primary payload or --checks-file)
    let checks_value = checks_extra
        .as_ref()
        .or_else(|| root.get("statusCheckRollup"))
        .or_else(|| pr_obj.get("statusCheckRollup"))
        .or_else(|| root.get("checks"))
        .or_else(|| pr_obj.get("checks"))
        .or_else(|| root.get("check_runs"))
        .or_else(|| pr_obj.get("check_runs"));
    if let Some(checks_val) = checks_value.filter(|v| !v.is_null()) {
        let parsed = parse_checks(checks_val, truncated_flags.contains("checks"))?;
        let state = if parsed.truncated {
            CoverageState::Truncated
        } else {
            CoverageState::Complete
        };
        collections.insert(
            "checks".into(),
            Coverage {
                state,
                scope: "status check rollup".into(),
                frontier: parsed.frontier.clone(),
                failure_reason: None,
            },
        );
        // Partial checks cannot claim zero checks or passed (forge_observation::validate).
        if !(parsed.truncated && parsed.count == 0) {
            let check_state = if parsed.truncated && parsed.state == ForgeCheckState::Passed {
                ForgeCheckState::Pending
            } else {
                parsed.state
            };
            let checks_canonical = serde_json::to_vec(&json!({
                "checks": parsed.entries,
                "count": parsed.count,
                "state": check_state.as_str(),
            }))?;
            raw_items.push(RawItem {
                kind: "checks".into(),
                node_id: "checks-rollup".into(),
                handle: format!("checks:pr-{pr_number}"),
                media_type: "application/json".into(),
                capture: CaptureState::Unpinned,
                bytes: checks_canonical,
                facts: ItemFacts::Checks {
                    state: check_state,
                    count: parsed.count,
                },
            });
        }
    }

    // 3. Comments, reviews, and review threads
    let mut saw_comments_collection = false;
    let mut comments_truncated = truncated_flags.contains("comments")
        || truncated_flags.contains("reviews")
        || root
            .get("hasMore")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || root
            .get("nextCursor")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
    let mut comment_frontiers: Vec<String> = Vec::new();
    let mut seen_comment_nodes: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    let mut push_comment = |node_id: String, canonical: Vec<u8>, handle: String| {
        if let Some(existing) = seen_comment_nodes.get(&node_id) {
            if existing == &canonical {
                return;
            }
        }
        let mut unique_id = node_id;
        let mut suffix = 2usize;
        while seen_comment_nodes.contains_key(&unique_id) {
            unique_id = format!("{unique_id}-{suffix}");
            suffix += 1;
        }
        seen_comment_nodes.insert(unique_id.clone(), canonical.clone());
        raw_items.push(RawItem {
            kind: "comment".into(),
            node_id: unique_id,
            handle,
            media_type: "application/json".into(),
            capture: CaptureState::Pinned,
            bytes: canonical,
            facts: ItemFacts::Comment,
        });
    };

    // Issue / PR comments
    let comments_sources: Vec<&Value> = [
        comments_extra.as_ref(),
        root.get("comments").filter(|_| comments_extra.is_none()),
        pr_obj
            .get("comments")
            .filter(|v| comments_extra.is_none() && root.get("comments") != Some(*v)),
        root.get("review_comments"),
        pr_obj
            .get("review_comments")
            .filter(|v| root.get("review_comments") != Some(*v)),
    ]
    .into_iter()
    .flatten()
    .filter(|v| !v.is_null())
    .collect();

    for source in comments_sources {
        saw_comments_collection = true;
        let (entries, truncated, frontier) = extract_collection_nodes(source)?;
        comments_truncated |= truncated;
        if let Some(f) = frontier {
            comment_frontiers.push(f);
        }
        for entry in entries {
            let (node_id, canonical, handle) = normalize_comment(entry, "comment")?;
            push_comment(node_id, canonical, handle);
        }
    }

    // Reviews
    let reviews_sources: Vec<&Value> = [
        reviews_extra.as_ref(),
        root.get("reviews").filter(|_| reviews_extra.is_none()),
        pr_obj
            .get("reviews")
            .filter(|v| reviews_extra.is_none() && root.get("reviews") != Some(*v)),
    ]
    .into_iter()
    .flatten()
    .filter(|v| !v.is_null())
    .collect();

    for source in reviews_sources {
        saw_comments_collection = true;
        let (entries, truncated, frontier) = extract_collection_nodes(source)?;
        comments_truncated |= truncated;
        if let Some(f) = frontier {
            comment_frontiers.push(f);
        }
        for entry in entries {
            if let Some((node_id, canonical, handle)) = normalize_review(entry)? {
                push_comment(node_id, canonical, handle);
            }
        }
    }

    // Review threads (MCP `pull_request_read` / GraphQL `reviewThreads`)
    let threads_sources: Vec<&Value> = [
        root.get("threads"),
        pr_obj
            .get("threads")
            .filter(|v| root.get("threads") != Some(*v)),
        root.get("reviewThreads"),
        pr_obj
            .get("reviewThreads")
            .filter(|v| root.get("reviewThreads") != Some(*v)),
    ]
    .into_iter()
    .flatten()
    .filter(|v| !v.is_null())
    .collect();

    for source in threads_sources {
        saw_comments_collection = true;
        let (threads, truncated, frontier) = extract_collection_nodes(source)?;
        comments_truncated |= truncated;
        if let Some(f) = frontier {
            comment_frontiers.push(f);
        }
        for thread in threads {
            let thread_id = id_field(thread, &["id", "node_id"]).unwrap_or_default();
            let path = str_field(thread, &["path"]);
            let line = u64_field(thread, &["line", "originalLine"]);
            let resolved = thread
                .get("isResolved")
                .or_else(|| thread.get("resolved"))
                .and_then(Value::as_bool);
            if let Some(thread_comments) = thread.get("comments") {
                let (entries, c_trunc, c_front) = extract_collection_nodes(thread_comments)?;
                comments_truncated |= c_trunc;
                if let Some(f) = c_front {
                    comment_frontiers.push(f);
                }
                for entry in entries {
                    let (node_id, canonical, handle) = normalize_thread_comment(
                        entry,
                        &thread_id,
                        path.as_deref(),
                        line,
                        resolved,
                    )?;
                    push_comment(node_id, canonical, handle);
                }
            }
        }
    }

    if saw_comments_collection {
        collections.insert(
            "comments".into(),
            Coverage {
                state: if comments_truncated {
                    CoverageState::Truncated
                } else {
                    CoverageState::Complete
                },
                scope: "pull request comments and reviews".into(),
                frontier: (!comment_frontiers.is_empty()).then(|| comment_frontiers.join("; ")),
                failure_reason: None,
            },
        );
    }

    let overall_truncated = collections
        .values()
        .any(|c| c.state != CoverageState::Complete);
    let overall_scope = collections.keys().cloned().collect::<Vec<_>>().join(", ");
    let coverage = ReadCoverage {
        overall: Coverage {
            state: if overall_truncated {
                CoverageState::Truncated
            } else {
                CoverageState::Complete
            },
            scope: overall_scope,
            frontier: None,
            failure_reason: None,
        },
        collections,
    };

    // 4. Chain `previous_observation` and resolve generations against prior
    // usable admitted reads.
    let usable_history: Vec<&ObservationRecord> = history.iter().filter(|r| r.usable()).collect();
    let previous_record = usable_history.last().copied();
    let previous_observation = previous_record.map(|r| r.event_id.clone());

    let (items, generations) = resolve_generations(
        raw_items,
        &provider,
        &provider_identity,
        previous_record,
        &usable_history,
        &mut blobs,
    );

    let read_id = overrides
        .read_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("read-{}", ulid::Ulid::new().to_string().to_lowercase()));
    let read_at = overrides.read_at.unwrap_or_else(Utc::now);
    let adapter = overrides
        .adapter
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("arc-forge-ingest/1 ({})", detected.adapter_tag()));

    let document = ForgeObservation {
        schema: "arc-forge-observation/1".into(),
        read_id,
        adapter,
        provider_identity,
        observed_head_sha: Some(observed_head_sha),
        observed_base_sha: Some(observed_base_sha),
        read_at,
        provider_updated_at,
        provider_event_id: None,
        coverage,
        responses,
        items,
        generations,
        previous_observation,
    };
    document.validate(history)?;

    Ok(BuiltObservation { document, blobs })
}

struct RawItem {
    kind: String,
    node_id: String,
    handle: String,
    media_type: String,
    capture: CaptureState,
    bytes: Vec<u8>,
    facts: ItemFacts,
}

fn resolve_generations(
    raw_items: Vec<RawItem>,
    provider: &str,
    identity: &ProviderIdentity,
    previous_record: Option<&ObservationRecord>,
    usable_history: &[&ObservationRecord],
    blobs: &mut BTreeMap<String, Vec<u8>>,
) -> (Vec<Item>, Vec<Generation>) {
    // Map every recorded generation ID to its definition for reuse and
    // collision avoidance.
    let mut prior_gens: BTreeMap<&str, &Generation> = BTreeMap::new();
    // Count how many distinct generations have ever been recorded for each
    // `(kind, node_id)` on this PR identity so newly minted versions increment
    // cleanly even across reversions.
    let mut prior_count: BTreeMap<(&str, &str), BTreeSet<&str>> = BTreeMap::new();
    // Latest recorded generation for `(kind, node_id)` on this PR identity.
    let mut latest_for_node: BTreeMap<(&str, &str), &Generation> = BTreeMap::new();

    for record in usable_history {
        let same_pr = record
            .document
            .provider_identity
            .same_pr_identity_as(identity);
        for g in &record.document.generations {
            prior_gens.insert(g.generation_id.as_str(), g);
            if same_pr {
                prior_count
                    .entry((g.kind.as_str(), g.node_id.as_str()))
                    .or_default()
                    .insert(g.generation_id.as_str());
                latest_for_node.insert((g.kind.as_str(), g.node_id.as_str()), g);
            }
        }
    }

    let previous_same_pr = previous_record
        .map(|r| &r.document)
        .filter(|d| d.provider_identity.same_pr_identity_as(identity));

    let mut used_ids: BTreeSet<String> = prior_gens.keys().map(|s| (*s).to_string()).collect();
    let mut items = Vec::with_capacity(raw_items.len());
    let mut generations = Vec::with_capacity(raw_items.len());

    for raw in raw_items {
        let sha256 = sha256_hex(&raw.bytes);
        let byte_length = raw.bytes.len() as u64;
        blobs.entry(sha256.clone()).or_insert(raw.bytes);

        let previous_item = previous_same_pr.and_then(|prev| {
            prev.items
                .iter()
                .find(|old| old.kind == raw.kind && old.node_id == raw.node_id)
        });

        let generation = if let Some(old) = previous_item {
            if old.sha256 == sha256 {
                // Exact unchanged item from previous observation: reuse its
                // exact recorded generation (preserving its predecessor).
                prior_gens
                    .get(old.generation_id.as_str())
                    .map(|g| (*g).clone())
                    .unwrap_or_else(|| Generation {
                        generation_id: old.generation_id.clone(),
                        predecessor: None,
                        kind: raw.kind.clone(),
                        node_id: raw.node_id.clone(),
                        sha256: sha256.clone(),
                    })
            } else {
                // Changed or reverted bytes relative to previous observation:
                // mint a brand-new generation naming the previous item's
                // generation as predecessor.
                let version = prior_count
                    .get(&(raw.kind.as_str(), raw.node_id.as_str()))
                    .map_or(1, BTreeSet::len)
                    + 1;
                let generation_id =
                    mint_generation_id(&raw.kind, &raw.node_id, version, &sha256, &mut used_ids);
                Generation {
                    generation_id,
                    predecessor: Some(old.generation_id.clone()),
                    kind: raw.kind.clone(),
                    node_id: raw.node_id.clone(),
                    sha256: sha256.clone(),
                }
            }
        } else if let Some(earlier) =
            latest_for_node.get(&(raw.kind.as_str(), raw.node_id.as_str()))
        {
            if earlier.sha256 == sha256 {
                (*earlier).clone()
            } else {
                let version = prior_count
                    .get(&(raw.kind.as_str(), raw.node_id.as_str()))
                    .map_or(1, BTreeSet::len)
                    + 1;
                let generation_id =
                    mint_generation_id(&raw.kind, &raw.node_id, version, &sha256, &mut used_ids);
                Generation {
                    generation_id,
                    predecessor: Some(earlier.generation_id.clone()),
                    kind: raw.kind.clone(),
                    node_id: raw.node_id.clone(),
                    sha256: sha256.clone(),
                }
            }
        } else {
            let generation_id =
                mint_generation_id(&raw.kind, &raw.node_id, 1, &sha256, &mut used_ids);
            Generation {
                generation_id,
                predecessor: None,
                kind: raw.kind.clone(),
                node_id: raw.node_id.clone(),
                sha256: sha256.clone(),
            }
        };

        items.push(Item {
            kind: raw.kind,
            node_id: raw.node_id,
            generation_id: generation.generation_id.clone(),
            sha256: sha256.clone(),
            byte_length,
            content: RetainedContentRef {
                provider: provider.into(),
                handle: raw.handle,
                sha256,
                byte_length,
                media_type: raw.media_type,
                capture: raw.capture,
            },
            facts: raw.facts,
        });
        generations.push(generation);
    }

    (items, generations)
}

fn mint_generation_id(
    kind: &str,
    node_id: &str,
    version: usize,
    sha256: &str,
    used_ids: &mut BTreeSet<String>,
) -> String {
    let short_kind = match kind {
        "pull-request" => "pr",
        other => other,
    };
    let node_slug: String = node_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    let base = format!("gen-{short_kind}-{node_slug}-v{version}-{}", &sha256[..8]);
    if used_ids.insert(base.clone()) {
        return base;
    }
    let mut n = 2usize;
    loop {
        let candidate = format!("{base}-{n}");
        if used_ids.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

fn detect_format(pr_obj: &Value, root: &Value) -> InputFormat {
    if pr_obj.get("headRefOid").is_some()
        || pr_obj.get("statusCheckRollup").is_some()
        || root.get("statusCheckRollup").is_some()
    {
        return InputFormat::Gh;
    }
    if pr_obj.get("head").is_some_and(Value::is_object)
        || pr_obj.get("html_url").is_some()
        || pr_obj.get("merge_commit_sha").is_some()
    {
        return InputFormat::GithubRest;
    }
    if pr_obj.get("headSha").is_some()
        || pr_obj.get("headBranch").is_some()
        || root.get("threads").is_some()
        || pr_obj.get("threads").is_some()
    {
        return InputFormat::Mcp;
    }
    InputFormat::Gh
}

struct ParsedUrl {
    host: String,
    repository: String,
    pr_number: u64,
}

fn parse_pr_url(raw: &str) -> Option<ParsedUrl> {
    let without_scheme = raw
        .trim()
        .strip_prefix("https://")
        .or_else(|| raw.trim().strip_prefix("http://"))?;
    let path_only = without_scheme
        .split(['?', '#'])
        .next()?
        .trim_end_matches('/');
    let parts: Vec<&str> = path_only.split('/').collect();
    if parts.len() < 5 {
        return None;
    }
    let host = parts[0];
    let owner = parts[1];
    let repo = parts[2];
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    let marker = parts[3];
    if !matches!(marker, "pull" | "pulls" | "merge_requests") {
        return None;
    }
    let pr_number = parts[4].parse::<u64>().ok().filter(|n| *n > 0)?;
    Some(ParsedUrl {
        host: host.into(),
        repository: format!("{owner}/{repo}"),
        pr_number,
    })
}

fn extract_pr_state(pr_obj: &Value) -> Result<(ForgePrState, Option<String>)> {
    let raw_state = str_field(pr_obj, &["state"]).unwrap_or_else(|| "open".into());
    let merged_flag = pr_obj
        .get("merged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || pr_obj
            .get("mergedAt")
            .or_else(|| pr_obj.get("merged_at"))
            .is_some_and(|v| !v.is_null());

    let state = if merged_flag || raw_state.eq_ignore_ascii_case("merged") {
        ForgePrState::Merged
    } else if raw_state.eq_ignore_ascii_case("closed") {
        ForgePrState::Closed
    } else if raw_state.eq_ignore_ascii_case("open") || raw_state.eq_ignore_ascii_case("draft") {
        ForgePrState::Open
    } else {
        bail!("unrecognized pull request state {raw_state:?}");
    };

    let merge_sha = if state == ForgePrState::Merged {
        let raw = pr_obj
            .get("mergeCommit")
            .and_then(|m| str_field(m, &["oid", "sha"]))
            .or_else(|| {
                str_field(
                    pr_obj,
                    &["mergeCommitSha", "merge_commit_sha", "mergeSha", "merge_sha"],
                )
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "merged pull request payload has no merge commit SHA (include `mergeCommit`, `merge_commit_sha`, or `mergeSha`)"
                )
            })?;
        Some(normalize_sha("merge commit SHA", &raw)?)
    } else {
        None
    };

    Ok((state, merge_sha))
}

struct ParsedChecks {
    state: ForgeCheckState,
    count: u64,
    truncated: bool,
    frontier: Option<String>,
    entries: Vec<Value>,
}

fn parse_checks(value: &Value, force_truncated: bool) -> Result<ParsedChecks> {
    let mut truncated = force_truncated;
    let mut frontier: Option<String> = None;

    // MCP summary object shape: { total, passed, failed, pending, [items/runs/checks] }
    if let Some(obj) = value.as_object() {
        let nested_items = obj
            .get("check_runs")
            .or_else(|| obj.get("statuses"))
            .or_else(|| obj.get("nodes"))
            .or_else(|| obj.get("items"))
            .or_else(|| obj.get("runs"))
            .or_else(|| obj.get("checks"))
            .and_then(Value::as_array);

        if obj
            .get("incomplete_results")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || obj
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            || obj
                .get("pageInfo")
                .and_then(|p| p.get("hasNextPage"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            truncated = true;
        }

        if let Some(items) = nested_items {
            if let Some(total) = obj
                .get("total_count")
                .or_else(|| obj.get("total"))
                .and_then(Value::as_u64)
            {
                if (items.len() as u64) < total {
                    truncated = true;
                    frontier = Some(format!("{}/{}", items.len(), total));
                }
            }
            return rollup_check_entries(items, truncated, frontier);
        }

        // Counts-only MCP summary object: { total, passed, failed, pending, ... }
        let total = obj.get("total").and_then(Value::as_u64);
        let passed = obj
            .get("passed")
            .or_else(|| obj.get("passing"))
            .or_else(|| obj.get("succeeded"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let failed = obj
            .get("failed")
            .or_else(|| obj.get("failing"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let pending = obj
            .get("pending")
            .or_else(|| obj.get("queued"))
            .or_else(|| obj.get("running"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let skipped = obj.get("skipped").and_then(Value::as_u64).unwrap_or(0);
        let neutral = obj.get("neutral").and_then(Value::as_u64).unwrap_or(0);
        if total.is_some()
            || obj.contains_key("passed")
            || obj.contains_key("failed")
            || obj.contains_key("pending")
        {
            let count = total.unwrap_or(passed + failed + pending + skipped + neutral);
            let state = if count == 0 {
                ForgeCheckState::NotConfigured
            } else if failed > 0 {
                ForgeCheckState::Failed
            } else if pending > 0 || truncated {
                ForgeCheckState::Pending
            } else if passed + skipped + neutral >= count {
                ForgeCheckState::Passed
            } else {
                ForgeCheckState::Pending
            };
            return Ok(ParsedChecks {
                state,
                count,
                truncated,
                frontier,
                entries: vec![json!({
                    "failed": failed,
                    "neutral": neutral,
                    "passed": passed,
                    "pending": pending,
                    "skipped": skipped,
                    "total": count,
                })],
            });
        }
    }

    if let Some(items) = value.as_array() {
        return rollup_check_entries(items, truncated, frontier);
    }

    bail!("unsupported checks payload shape; expected an array of checks or a check-runs/summary object")
}

fn rollup_check_entries(
    items: &[Value],
    truncated: bool,
    frontier: Option<String>,
) -> Result<ParsedChecks> {
    let mut normalized = Vec::with_capacity(items.len());
    let mut any_failed = false;
    let mut any_pending = false;

    for item in items {
        let name =
            str_field(item, &["name", "context", "workflowName"]).unwrap_or_else(|| "check".into());
        let status = str_field(item, &["status"]).map(|s| s.to_ascii_uppercase());
        let conclusion = str_field(item, &["conclusion"]).map(|s| s.to_ascii_uppercase());
        let state = str_field(item, &["state"]).map(|s| s.to_ascii_uppercase());

        let bucket = classify_check(status.as_deref(), conclusion.as_deref(), state.as_deref());
        match bucket {
            "failed" => any_failed = true,
            "pending" => any_pending = true,
            _ => {}
        }

        normalized.push(json!({
            "bucket": bucket,
            "conclusion": conclusion,
            "name": name,
            "state": state,
            "status": status,
        }));
    }

    normalized.sort_by_key(|a| a.to_string());
    let count = normalized.len() as u64;
    let rollup = if count == 0 {
        ForgeCheckState::NotConfigured
    } else if any_failed {
        ForgeCheckState::Failed
    } else if any_pending || truncated {
        ForgeCheckState::Pending
    } else {
        ForgeCheckState::Passed
    };

    Ok(ParsedChecks {
        state: rollup,
        count,
        truncated,
        frontier,
        entries: normalized,
    })
}

fn classify_check(
    status: Option<&str>,
    conclusion: Option<&str>,
    state: Option<&str>,
) -> &'static str {
    if let Some(c) = conclusion {
        return match c {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" | "PASSED" => "passed",
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
            | "STALE" | "ERROR" | "FAILED" => "failed",
            _ => "pending",
        };
    }
    if let Some(st) = status {
        if st != "COMPLETED" {
            return match st {
                "FAILED" | "FAILURE" | "ERROR" => "failed",
                "PASSED" | "SUCCESS" | "SKIPPED" | "NEUTRAL" => "passed",
                _ => "pending",
            };
        }
    }
    if let Some(st) = state {
        return match st {
            "SUCCESS" | "PASSED" | "NEUTRAL" | "SKIPPED" => "passed",
            "FAILURE" | "FAILED" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" => {
                "failed"
            }
            _ => "pending",
        };
    }
    "pending"
}

fn extract_collection_nodes(value: &Value) -> Result<(Vec<&Value>, bool, Option<String>)> {
    if let Some(arr) = value.as_array() {
        return Ok((arr.iter().collect(), false, None));
    }
    if let Some(obj) = value.as_object() {
        let mut truncated = obj
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || obj.get("hasMore").and_then(Value::as_bool).unwrap_or(false);
        let mut frontier = str_field(value, &["nextCursor", "cursor"]);
        if let Some(page_info) = obj.get("pageInfo").and_then(Value::as_object) {
            if page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                truncated = true;
                frontier = page_info
                    .get("endCursor")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(frontier);
            }
        }
        if let Some(nodes) = obj
            .get("nodes")
            .or_else(|| obj.get("items"))
            .and_then(Value::as_array)
        {
            return Ok((nodes.iter().collect(), truncated, frontier));
        }
    }
    bail!("expected a JSON array or paginated {{nodes}} collection")
}

fn normalize_comment(entry: &Value, prefix: &str) -> Result<(String, Vec<u8>, String)> {
    let body = str_field(entry, &["body"]).unwrap_or_default();
    let author = extract_author(entry);
    let path = str_field(entry, &["path"]);
    let line = u64_field(entry, &["line", "original_line", "originalLine"]);
    let raw_id = id_field(entry, &["node_id", "id", "databaseId"]);
    let canonical = serde_json::to_vec(&json!({
        "author": author,
        "body": body,
        "kind": prefix,
        "line": line,
        "path": path,
    }))?;
    let node_id = match raw_id {
        Some(id) if id.chars().all(|c| c.is_ascii_digit()) => format!("{prefix}-{id}"),
        Some(id) => id,
        None => format!("{prefix}-{}", &sha256_hex(&canonical)[..12]),
    };
    let handle =
        str_field(entry, &["url", "html_url"]).unwrap_or_else(|| format!("{prefix}:{node_id}"));
    Ok((node_id, canonical, handle))
}

fn normalize_review(entry: &Value) -> Result<Option<(String, Vec<u8>, String)>> {
    let body = str_field(entry, &["body"]).unwrap_or_default();
    let state = str_field(entry, &["state"]).map(|s| s.to_ascii_uppercase());
    if body.trim().is_empty() && state.as_deref().is_none_or(|s| s == "PENDING") {
        return Ok(None);
    }
    let author = extract_author(entry);
    let commit = entry
        .get("commit")
        .and_then(|c| str_field(c, &["oid", "sha"]))
        .or_else(|| str_field(entry, &["commit_id", "commitId"]));
    let raw_id = id_field(entry, &["node_id", "id", "databaseId"]);
    let canonical = serde_json::to_vec(&json!({
        "author": author,
        "body": body,
        "commit": commit,
        "kind": "review",
        "state": state,
    }))?;
    let node_id = match raw_id {
        Some(id) if id.chars().all(|c| c.is_ascii_digit()) => format!("review-{id}"),
        Some(id) => id,
        None => format!("review-{}", &sha256_hex(&canonical)[..12]),
    };
    let handle =
        str_field(entry, &["url", "html_url"]).unwrap_or_else(|| format!("review:{node_id}"));
    Ok(Some((node_id, canonical, handle)))
}

fn normalize_thread_comment(
    entry: &Value,
    thread_id: &str,
    path: Option<&str>,
    line: Option<u64>,
    resolved: Option<bool>,
) -> Result<(String, Vec<u8>, String)> {
    let body = str_field(entry, &["body"]).unwrap_or_default();
    let author = extract_author(entry);
    let raw_id = id_field(entry, &["node_id", "id", "databaseId"]);
    let canonical = serde_json::to_vec(&json!({
        "author": author,
        "body": body,
        "is_resolved": resolved,
        "kind": "review-comment",
        "line": line,
        "path": path,
        "thread_id": (!thread_id.is_empty()).then_some(thread_id),
    }))?;
    let node_id = match raw_id {
        Some(id) if id.chars().all(|c| c.is_ascii_digit()) => format!("review-comment-{id}"),
        Some(id) => id,
        None if !thread_id.is_empty() => {
            format!("thread-{thread_id}-{}", &sha256_hex(&canonical)[..12])
        }
        None => format!("review-comment-{}", &sha256_hex(&canonical)[..12]),
    };
    let handle = str_field(entry, &["url", "html_url"])
        .unwrap_or_else(|| format!("review-comment:{node_id}"));
    Ok((node_id, canonical, handle))
}

fn extract_author(entry: &Value) -> Option<String> {
    if let Some(s) = entry.get("author").and_then(Value::as_str) {
        return (!s.trim().is_empty()).then(|| s.to_string());
    }
    entry
        .get("author")
        .or_else(|| entry.get("user"))
        .and_then(|u| str_field(u, &["login", "name", "username"]))
}

fn str_field(value: &Value, names: &[&str]) -> Option<String> {
    for name in names {
        if let Some(s) = value.get(*name).and_then(Value::as_str) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn id_field(value: &Value, names: &[&str]) -> Option<String> {
    for name in names {
        match value.get(*name) {
            Some(Value::String(s)) if !s.trim().is_empty() => return Some(s.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

fn u64_field(value: &Value, names: &[&str]) -> Option<u64> {
    for name in names {
        if let Some(n) = value.get(*name).and_then(Value::as_u64) {
            return Some(n);
        }
    }
    None
}

fn normalize_sha(label: &str, raw: &str) -> Result<String> {
    let lower = raw.trim().to_ascii_lowercase();
    ensure!(
        matches!(lower.len(), 40 | 64)
            && lower
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid {label}: {raw:?} (expected 40- or 64-character hex SHA)"
    );
    Ok(lower)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh_payload(
        head: &str,
        state: &str,
        merge: Option<&str>,
        check_conclusion: &str,
        comment_body: &str,
    ) -> Vec<u8> {
        let mut doc = json!({
            "number": 42,
            "id": "PR_kwDO_example",
            "url": "https://github.com/acme/widget/pull/42",
            "state": state,
            "title": "Improve widget",
            "body": "Initial PR body",
            "baseRefName": "main",
            "baseRefOid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "headRefName": "feat/widget",
            "headRefOid": head,
            "headRepository": { "name": "widget", "nameWithOwner": "contrib/widget" },
            "updatedAt": "2026-11-25T10:00:00Z",
            "comments": [
                { "id": "IC_1", "author": { "login": "alice" }, "body": comment_body }
            ],
            "statusCheckRollup": [
                { "__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": check_conclusion }
            ]
        });
        if let Some(sha) = merge {
            doc["mergeCommit"] = json!({ "oid": sha });
        }
        serde_json::to_vec(&doc).unwrap()
    }

    #[test]
    fn example_observation_is_valid() {
        example_observation().validate(&[]).unwrap();
    }

    #[test]
    fn multi_read_reuses_unchanged_generations_and_chains_predecessors() {
        let first_bytes = gh_payload(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "OPEN",
            None,
            "FAILURE",
            "Please fix edge case",
        );
        let first = build_observation(
            &first_bytes,
            "read1.json",
            None,
            None,
            None,
            InputFormat::Auto,
            &IngestOverrides {
                read_id: Some("r1".into()),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
        assert_eq!(first.document.previous_observation, None);
        assert_eq!(first.document.items.len(), 3);
        let first_comment_gen = first
            .document
            .items
            .iter()
            .find(|i| i.node_id == "IC_1")
            .unwrap()
            .generation_id
            .clone();
        let first_checks_gen = first
            .document
            .items
            .iter()
            .find(|i| i.kind == "checks")
            .unwrap()
            .generation_id
            .clone();

        let history = vec![ObservationRecord {
            aliases: Vec::new(),
            conflict: false,
            invalid: None,
            event_id: "ev-obs-1".into(),
            created_at: Utc::now(),
            document: first.document.clone(),
        }];

        // Second read: comment unchanged, checks flip to SUCCESS, PR merged.
        let second_bytes = gh_payload(
            "cccccccccccccccccccccccccccccccccccccccc",
            "MERGED",
            Some("dddddddddddddddddddddddddddddddddddddddd"),
            "SUCCESS",
            "Please fix edge case",
        );
        let second = build_observation(
            &second_bytes,
            "read2.json",
            None,
            None,
            None,
            InputFormat::Auto,
            &IngestOverrides {
                read_id: Some("r2".into()),
                ..Default::default()
            },
            &history,
        )
        .unwrap();
        assert_eq!(
            second.document.previous_observation.as_deref(),
            Some("ev-obs-1")
        );
        let second_comment = second
            .document
            .items
            .iter()
            .find(|i| i.node_id == "IC_1")
            .unwrap();
        assert_eq!(second_comment.generation_id, first_comment_gen);

        let second_checks = second
            .document
            .generations
            .iter()
            .find(|g| g.kind == "checks")
            .unwrap();
        assert_ne!(second_checks.generation_id, first_checks_gen);
        assert_eq!(
            second_checks.predecessor.as_deref(),
            Some(first_checks_gen.as_str())
        );

        let pr_item = second
            .document
            .items
            .iter()
            .find(|i| i.kind == "pull-request")
            .unwrap();
        assert_eq!(
            pr_item.facts,
            ItemFacts::PullRequest {
                state: ForgePrState::Merged,
                merge_sha: Some("dddddddddddddddddddddddddddddddddddddddd".into()),
            }
        );
    }

    #[test]
    fn truncated_checks_never_claim_passed() {
        let payload = serde_json::to_vec(&json!({
            "number": 7,
            "url": "https://github.com/acme/widget/pull/7",
            "state": "open",
            "baseBranch": "main",
            "headBranch": "topic",
            "headSha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "baseSha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "checks": {
                "total": 2,
                "items": [{ "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS" }]
            }
        }))
        .unwrap();
        let built = build_observation(
            &payload,
            "mcp.json",
            None,
            None,
            None,
            InputFormat::Mcp,
            &IngestOverrides::default(),
            &[],
        )
        .unwrap();
        assert_eq!(
            built.document.coverage.overall.state,
            CoverageState::Truncated
        );
        let checks = built
            .document
            .items
            .iter()
            .find(|i| i.kind == "checks")
            .unwrap();
        assert_eq!(
            checks.facts,
            ItemFacts::Checks {
                state: ForgeCheckState::Pending,
                count: 1,
            }
        );
    }
}
