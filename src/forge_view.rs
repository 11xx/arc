//! Offline reconstruction. Forge selection, local resolution and custody are
//! independent reads; none of these projections grants workflow authority.
use crate::{
    commands::Ctx,
    forge_observation as observation,
    state::{ChangeState, VerdictEntry},
    store::Store,
};
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Options {
    pub change_at: Option<String>,
    pub history: bool,
    pub observation: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub provider: String,
    pub host: String,
    pub repository: String,
    pub pr: u64,
}
impl Identity {
    pub fn parse(raw: &str) -> Result<Self> {
        let (provider, rest) = raw
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("expected PROVIDER:HOST/OWNER/REPO#NUMBER"))?;
        let (location, number) = rest
            .rsplit_once('#')
            .ok_or_else(|| anyhow::anyhow!("forge identity requires #NUMBER"))?;
        let (host, repository) = location
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("forge identity requires HOST/OWNER/REPO"))?;
        ensure!(
            !provider.is_empty()
                && !host.is_empty()
                && repository.split('/').count() == 2
                && repository.split('/').all(|p| !p.is_empty()),
            "invalid forge identity"
        );
        let pr = number.parse()?;
        ensure!(pr > 0, "PR number must be positive");
        Ok(Self {
            provider: provider.into(),
            host: host.into(),
            repository: repository.into(),
            pr,
        })
    }
    pub fn matches(&self, state: &ChangeState) -> bool {
        state
            .forge
            .observations
            .iter()
            .any(|r| self.matches_provider(&r.document.provider_identity))
            || state.forge.links.iter().any(|link| {
                link.provider_identity
                    .as_ref()
                    .is_some_and(|p| self.matches_provider(p))
            })
    }
    fn matches_provider(&self, p: &observation::ProviderIdentity) -> bool {
        p.provider == self.provider
            && p.host == self.host
            && p.repository == self.repository
            && p.pr_number == self.pr
    }
}

pub fn drift(state: &ChangeState, verdict: &VerdictEntry) -> Option<Vec<String>> {
    verdict.forge_frontier.as_ref().map(|frontier| {
        state
            .forge
            .observations
            .iter()
            .filter(|r| !frontier.iter().any(|id| r.names_event(id)))
            .map(|r| r.event_id.clone())
            .collect()
    })
}

fn freshness(records: &[&observation::ObservationRecord], now: DateTime<Utc>) -> Value {
    let last = records.iter().rev().find(|r| {
        r.usable() && r.document.coverage.overall.state != observation::CoverageState::Failed
    });
    let latest = records.last();
    let age = last.map(|r| now.signed_duration_since(r.document.read_at).num_seconds());
    let anomalies: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            r.document.read_at > now
                || (*i > 0 && r.document.read_at < records[*i - 1].document.read_at)
        })
        .map(|(_, r)| r.event_id.clone())
        .collect();
    json!({"last_successful_read": last.map(|r| json!({"event_id":r.event_id,"read_at":r.document.read_at})),
        "latest_attempt": latest.map(|r| json!({"event_id":r.event_id,"read_at":r.document.read_at,"coverage":r.document.coverage})),
        "age_seconds": age.filter(|a| *a >= 0), "clock_anomalies": anomalies,
        "basis":"recorded acquisition times; provider agreement is not inferred"})
}

pub fn related_closure(
    states: &std::collections::BTreeMap<String, ChangeState>,
    mut selected: std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    loop {
        let before = selected.len();
        for state in states.values() {
            let references = crate::forge_round_trip::related_to(state);
            if selected.contains(&state.change_id)
                || references.iter().any(|id| selected.contains(id))
            {
                selected.insert(state.change_id.clone());
                selected.extend(references.into_iter().filter(|id| states.contains_key(id)));
            }
        }
        if selected.len() == before {
            return selected;
        }
    }
}

fn record_sources(
    payload: &crate::model::Payload,
) -> Vec<&crate::forge_round_trip::SourceGeneration> {
    use crate::model::Payload;
    let mut sources = Vec::new();
    match payload {
        Payload::Message {
            delivery: Some(delivery),
            ..
        } => sources.extend(delivery.sources()),
        Payload::ContextDeclared {
            disclosure, origin, ..
        } => {
            if let Some(disclosure) = disclosure {
                sources.extend(disclosure.sources());
            }
            sources.extend(origin);
        }
        _ => {}
    }
    sources
}

fn registration_ids<'a>(
    events: &'a [crate::model::Event],
    change: &'a str,
) -> impl Iterator<Item = &'a str> {
    events.iter().filter_map(move |e| match &e.payload {
        crate::model::Payload::CandidateRegistered {
            candidate_id,
            brief,
            ..
        } if brief.change_id == change => Some(candidate_id.as_str()),
        _ => None,
    })
}

fn workflow_facts(
    state: &ChangeState,
    events: &[crate::model::Event],
    repo_events: &[crate::model::Event],
) -> Result<Value> {
    let candidates = crate::candidate::Ledger::replay(repo_events.iter())?;
    let registrations: Vec<_> = candidates
        .registrations()
        .filter(|r| r.brief.change_id == state.change_id)
        .collect();
    let selections: Vec<_> = candidates
        .selections()
        .filter(|s| s.change_id == state.change_id)
        .collect();
    let received: Vec<_> = registrations
        .iter()
        .filter_map(|r| candidates.received_by(&r.candidate_id))
        .collect();
    let promotions: Vec<_> = selections
        .iter()
        .filter_map(|s| candidates.promotion_of(&s.event_id))
        .collect();
    let dispatches: Vec<_> = events
        .iter()
        .chain(repo_events.iter())
        .filter(|e| match &e.payload {
            crate::model::Payload::RunDispatched { change, .. } => {
                change.as_deref() == Some(state.change_id.as_str())
            }
            _ => false,
        })
        .collect();
    let runs: Vec<_> = events
        .iter()
        .chain(repo_events.iter())
        .filter(|e| {
            dispatches.iter().any(|d| d.event_id == e.event_id)
                || match &e.payload {
                    crate::model::Payload::RunEnded {
                        dispatch_event_id, ..
                    }
                    | crate::model::Payload::RunBound {
                        dispatch_event_id, ..
                    } => dispatches.iter().any(|d| &d.event_id == dispatch_event_id),
                    _ => false,
                }
        })
        .collect();
    Ok(
        json!({"candidates":registrations,"received_evidence":received,"run_references":runs,
        "acceptance":{"selections":selections,"promotions":promotions,"local_closure":state.closure,"provider_lifecycle":state.forge.pr_states}}),
    )
}

pub fn build(store: &Store, state: &ChangeState, options: &Options) -> Result<Value> {
    let now = Utc::now();
    let selected = options
        .observation
        .as_deref()
        .map(|id| observation::find_observation(&state.forge, id))
        .transpose()?;
    let boundary = selected.and_then(|selected| {
        state
            .forge
            .observations
            .iter()
            .position(|r| r.event_id == selected.event_id)
    });
    let history: Vec<_> = state
        .forge
        .observations
        .iter()
        .take(boundary.map_or(state.forge.observations.len(), |i| i + 1))
        .collect();
    let content: Vec<_> = if options.history {
        history.clone()
    } else {
        selected
            .or_else(|| history.last().copied())
            .into_iter()
            .collect()
    };
    let rewrites = store.rewrites()?;
    let resolved_observations: Vec<_> = content.iter().map(|r| {
        let resolve = |sha: &Option<String>| -> Result<Option<String>> {
            sha.as_deref().map(|sha| match rewrites.fate(sha)? {
                Some(crate::rewrite::Fate::Dropped) => Ok(None),
                Some(crate::rewrite::Fate::Rewritten(successor)) => Ok(Some(successor)),
                None => Ok(Some(sha.to_string())),
            }).transpose().map(Option::flatten)
        };
        Ok(json!({"observation_id":r.event_id,"observed_head":r.document.observed_head_sha,"resolved_head":resolve(&r.document.observed_head_sha)?,"observed_base":r.document.observed_base_sha,"resolved_base":resolve(&r.document.observed_base_sha)?}))
    }).collect::<Result<Vec<_>>>()?;
    let custody = crate::custody::Custody::at(&store.root);
    let availability: Vec<_> = content.iter().flat_map(|record| record.document.contents().map(|source| {
        let available = custody.availability(&source.into());
        json!({"observation_id":record.event_id,"supports":{"responses":record.document.responses.iter().filter(|r| &r.content == source).map(|r| &r.response_id).collect::<Vec<_>>(),"generations":record.document.items.iter().filter(|i| &i.content == source).map(|i| &i.generation_id).collect::<Vec<_>>()},"source":source,"availability":available.label(),"diagnostic":match available {crate::custody::Availability::Corrupt(reason)=>Some(reason), _=>None}})
    })).collect();
    let states = store.readable_states()?;
    let identities: Vec<_> = content
        .iter()
        .map(|r| &r.document.provider_identity)
        .collect();
    let related: Vec<_> = states
        .values()
        .filter(|other| {
            other.change_id == state.change_id
                || other.forge.observations.iter().any(|r| {
                    identities
                        .iter()
                        .any(|p| p.same_pr_identity_as(&r.document.provider_identity))
                })
        })
        .map(|s| s.change_id.clone())
        .collect();
    let repo_events = store.load_repository_events()?;
    let events = store.load_events(&state.change_id)?;
    let mut workflow = workflow_facts(state, &events, &repo_events)?;
    let mut round_trip = state.round_trip.clone();
    round_trip.join(state, &repo_events, &custody);
    let candidate_ids: std::collections::BTreeSet<_> = round_trip
        .candidate_paths
        .iter()
        .map(|p| p.candidate_id.as_str())
        .chain(registration_ids(&repo_events, &state.change_id))
        .collect();
    let candidate_context: Vec<_> = repo_events.iter().filter(|event| matches!(&event.payload,
        crate::model::Payload::ContextDeclared { subject: crate::model::RelationSubject::Candidate { candidate_id }, .. } if candidate_ids.contains(candidate_id.as_str()))).collect();
    let local_records: Vec<_> = events
        .iter()
        .chain(candidate_context.iter().copied())
        .filter(|event| {
            matches!(
                &event.payload,
                crate::model::Payload::Message {
                    delivery: Some(_),
                    ..
                } | crate::model::Payload::ContextDeclared {
                    disclosure: Some(_),
                    ..
                } | crate::model::Payload::ContextDeclared {
                    origin: Some(_),
                    ..
                }
            )
        })
        .collect();
    let wanted_sources: std::collections::BTreeSet<_> = local_records
        .iter()
        .flat_map(|event| record_sources(&event.payload))
        .map(|source| source.change_id.as_str())
        .collect();
    let mut wanted_sources = wanted_sources;
    for event in &local_records {
        if let crate::model::Payload::ContextDeclared {
            disclosure: Some(disclosure),
            ..
        } = &event.payload
        {
            for selection in &disclosure.selected {
                match &selection.source {
                    crate::forge_round_trip::DisclosureSource::Ledger(source) => {
                        if source.change_id != Store::REPOSITORY_SCOPE {
                            wanted_sources.insert(&source.change_id);
                        }
                    }
                    crate::forge_round_trip::DisclosureSource::Git(_)
                    | crate::forge_round_trip::DisclosureSource::Forge(_) => {}
                }
            }
        }
    }
    let mut source_history = repo_events.clone();
    let mut source_read_errors = Vec::new();
    for change in wanted_sources {
        match store.load_events(change) {
            Ok(events) => source_history.extend(events),
            Err(error) => {
                source_read_errors.push(json!({"change_id":change,"diagnostic":error.to_string()}))
            }
        }
    }
    let mut source_availability: Vec<_> = local_records
        .iter()
        .flat_map(|event| {
            record_sources(&event.payload).into_iter().map(|source| {
                let mut view =
                    crate::forge_round_trip::source_view(source, &source_history, &custody);
                view["record_event_id"] = json!(event.event_id);
                view
            })
        })
        .collect();
    for event in &local_records {
        if let crate::model::Payload::ContextDeclared {
            disclosure: Some(disclosure),
            ..
        } = &event.payload
        {
            for selection in &disclosure.selected {
                let available = match &selection.source {
                    crate::forge_round_trip::DisclosureSource::Ledger(source) => Some(
                        source_history
                            .iter()
                            .any(|e| e.change_id == source.change_id && e.event_id == source.event),
                    ),
                    crate::forge_round_trip::DisclosureSource::Git(source) => Some(
                        crate::gitio::commit_exists(&std::env::current_dir()?, &source.commit)?,
                    ),
                    crate::forge_round_trip::DisclosureSource::Forge(_) => None,
                };
                if let Some(available) = available {
                    source_availability.push(json!({"record_event_id":event.event_id,"reference":selection.source,"access_basis":selection.access_basis,"fields":selection.fields,"source_status":if available {"recorded"} else {"unresolved"},"availability":if available {"available"} else {"unavailable"}}));
                }
            }
        }
    }
    let byte_references: Vec<_> = local_records.iter().flat_map(|event| crate::custody::referenced(std::iter::once(&event.payload)).into_iter().map(|content| {
        let availability = custody.availability(&content);
        json!({"record_event_id":event.event_id,"sha256":content.sha256,"byte_length":content.byte_length,"availability":availability.label()})
    })).collect();
    workflow["round_trip"] = json!(round_trip);
    workflow["deliveries"] = json!(state
        .messages
        .iter()
        .filter(|m| m.delivery.is_some())
        .collect::<Vec<_>>());
    workflow["disclosures"] = json!(state.round_trip.disclosures);
    workflow["candidate_context"] = json!(candidate_context);
    workflow["source_availability"] = json!(source_availability);
    workflow["source_read_errors"] = json!(source_read_errors);
    workflow["byte_references"] = json!(byte_references);
    workflow["local_record_scope"] =
        json!("held local workflow records; source and byte availability are current reads");
    let seeds: std::collections::BTreeSet<_> = related.into_iter().collect();
    let related = related_closure(&states, seeds);
    let related_workflows: Vec<_> = related.iter().filter(|id| *id != &state.change_id).filter_map(|id| states.get(id)).map(|other| {
        Ok(json!({"change_id":other.change_id,"workflow":workflow_facts(other, &store.load_events(&other.change_id)?, &repo_events)?,"round_trip":other.round_trip}))
    }).collect::<Result<Vec<_>>>()?;
    workflow["related_workflows"] = json!(related_workflows);
    let projection_in_view = |reference: &Option<String>| {
        selected.is_none()
            || reference
                .as_deref()
                .is_some_and(|id| history.iter().any(|r| r.names_event(id)))
    };
    let links: Vec<_> = state
        .forge
        .links
        .iter()
        .filter(|link| projection_in_view(&link.observation))
        .collect();
    let checks = state
        .forge
        .checks
        .as_ref()
        .filter(|checks| projection_in_view(&checks.observation));
    workflow["acceptance"]["provider_lifecycle"] = json!(state
        .forge
        .pr_states
        .iter()
        .filter(|lifecycle| projection_in_view(&lifecycle.observation))
        .collect::<Vec<_>>());
    if selected.is_some() {
        let lifecycle = state
            .forge
            .pr_states
            .iter()
            .rev()
            .find(|l| projection_in_view(&l.observation) && !l.unresolved_binding);
        workflow["round_trip"]["provider_lifecycle"] =
            json!(
                lifecycle.map(|l| if l.state == crate::forge::ForgePrState::Closed {
                    "closed-unmerged"
                } else {
                    l.state.as_str()
                })
            );
        workflow["round_trip"]["observed_merge_sha"] =
            json!(lifecycle.and_then(|l| l.observed_merge_sha.as_ref()));
    }
    let verdicts: Vec<_> = state.verdicts.iter().map(|v| json!({"event_id":v.event_id,"sources":v.forge_sources,"frontier":v.forge_frontier,"later_observations":drift(state,v),"verdict":v.verdict})).collect();
    let mut view = json!({"schema":"arc-forge-view/1","change_id":state.change_id,
        "identity":identities,"observations":content,"generation_links":content.iter().flat_map(|r| &r.document.generations).collect::<Vec<_>>(),
        "related_changes":related,"patchsets":state.patchsets,
        "review_bindings":verdicts,"findings":state.findings,"coverage":content.iter().map(|r| &r.document.coverage).collect::<Vec<_>>(),
        "availability":availability,"freshness":freshness(&history,now),
        "frontiers":{"change_knowledge_at":options.change_at,"repository_scope":"current held history","change_events":events.iter().map(|e| &e.event_id).collect::<Vec<_>>(),"repository_events":repo_events.iter().map(|e| &e.event_id).collect::<Vec<_>>()},
        "reader_options":options,"local_resolution":{"read_at":now,"observations":resolved_observations,"links":links,"checks":checks},
        "current_byte_availability":{"read_at":now},"freshness_scope":if selected.is_some() {"selected observation history"} else if options.change_at.is_some() {"change knowledge at event"} else {"held history"}});
    view.as_object_mut()
        .expect("view is an object")
        .extend(workflow.as_object().expect("workflow is an object").clone());
    Ok(view)
}

pub fn render(view: &Value) -> String {
    let mut text = format!("Forge reconstruction: {}\n", view["change_id"]);
    if let Some(diagnostic) = view.get("diagnostic") {
        text.push_str(&format!("Unavailable: {diagnostic}\n"));
        return text;
    }
    for name in [
        "identity",
        "observations",
        "generation_links",
        "related_changes",
        "patchsets",
        "candidates",
        "received_evidence",
        "run_references",
        "acceptance",
        "coverage",
        "availability",
        "freshness",
        "freshness_scope",
        "review_bindings",
        "findings",
        "frontiers",
        "reader_options",
        "local_resolution",
        "current_byte_availability",
        "round_trip",
        "deliveries",
        "disclosures",
        "candidate_context",
        "source_availability",
        "source_read_errors",
        "byte_references",
        "related_workflows",
        "local_record_scope",
    ] {
        text.push_str(&format!("{name}: {}\n", view[name]));
    }
    text
}

pub fn show(
    ctx: &Ctx,
    change: Option<&str>,
    identity: Option<Identity>,
    options: Options,
    json_output: bool,
) -> Result<()> {
    use std::io::Write;
    ensure!(
        change.is_some() || identity.is_some(),
        "name a change or complete provider identity"
    );
    let Some(store) = Store::discover_existing_for_reading(&ctx.cwd)? else {
        ensure!(
            change.is_none() && options.body.is_none(),
            "no recorded forge history"
        );
        if json_output {
            println!("[]");
        }
        return Ok(());
    };
    let states = store.readable_states()?;
    let selected: Vec<_> = match change {
        Some(id) => vec![store.state(&store.resolve_change(id)?)?],
        None => {
            let identity = identity
                .ok_or_else(|| anyhow::anyhow!("name a change or complete provider identity"))?;
            let matches = related_closure(
                &states,
                states
                    .values()
                    .filter(|s| identity.matches(s))
                    .map(|s| s.change_id.clone())
                    .collect(),
            );
            states
                .into_values()
                .filter(|s| matches.contains(&s.change_id))
                .collect()
        }
    };
    ensure!(
        options.body.is_none() || selected.len() == 1,
        "--body requires exactly one change"
    );
    let mut views = Vec::new();
    for state in selected {
        let mut view = build(&store, &state, &options)?;
        if let Some(generation) = &options.body {
            let records: Vec<_> = if let Some(obs) = &options.observation {
                vec![observation::find_observation(&state.forge, obs)?]
            } else {
                state
                    .forge
                    .observations
                    .iter()
                    .filter(|r| r.usable())
                    .collect()
            };
            let items: Vec<_> = records
                .iter()
                .flat_map(|r| &r.document.items)
                .filter(|i| &i.generation_id == generation)
                .collect();
            ensure!(
                !items.is_empty(),
                "generation {generation} has no retained source in selected history"
            );
            ensure!(
                items
                    .iter()
                    .all(|i| i.sha256 == items[0].sha256 && i.byte_length == items[0].byte_length),
                "generation has conflicting source claims"
            );
            let source = &items[0].content;
            let custody = crate::custody::Custody::at(&store.root);
            let available = custody.availability(&source.into());
            let bytes = custody.read_bytes(&source.into());
            view["body"] = match &bytes {
                Ok(bytes) => {
                    json!({"generation_id":generation,"source":source,"availability":"available","encoding":"base64","bytes":base64(bytes)})
                }
                Err(error) => {
                    json!({"generation_id":generation,"source":source,"availability":if available.is_available() {"unavailable"} else {available.label()},"diagnostic":error.to_string(),"bytes":null})
                }
            };
            if !json_output {
                eprintln!(
                    "{}",
                    view["body"]
                        .as_object()
                        .map(|o| {
                            let mut o = o.clone();
                            o.remove("bytes");
                            Value::Object(o)
                        })
                        .unwrap()
                );
                std::io::stdout().write_all(&bytes?)?;
                return Ok(());
            }
        }
        views.push(view);
    }
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&if change.is_some() {
                views.remove(0)
            } else {
                json!(views)
            })?
        );
    } else {
        for view in views {
            print!("{}", render(&view));
        }
    }
    Ok(())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        for shift in [18, 12] {
            result.push(ALPHABET[((n >> shift) & 63) as usize] as char);
        }
        result.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    result
}
