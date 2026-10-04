//! Observed preparation effects, including writes preceding ledger publication.
use super::*;
use crate::rewrite::RewriteIntent;

#[derive(Default, Serialize)]
pub(super) struct Effects {
    pub refs: Vec<RefEffect>,
    pub objects: Vec<ObjectEffect>,
    pub worktrees_added: Vec<CheckoutEffect>,
    pub worktrees_removed: Vec<CheckoutEffect>,
    pub pending_rewrite: Option<PendingRewrite>,
    pub observation_errors: Vec<String>,
}

impl Effects {
    pub fn retained(&self) -> bool {
        !self.refs.is_empty()
            || !self.objects.is_empty()
            || !self.worktrees_added.is_empty()
            || !self.worktrees_removed.is_empty()
            || self.pending_rewrite.is_some()
    }
}

#[derive(Serialize)]
pub(super) struct RefEffect {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ObjectEffect {
    pub oid: String,
    pub kind: String,
}

#[derive(Serialize)]
pub(super) struct CheckoutEffect {
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Invocation {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Serialize)]
pub(super) struct PendingRewrite {
    pub path: PathBuf,
    pub intent: RewriteIntent,
    pub ref_states: BTreeMap<String, Option<String>>,
    pub recorded_event_id: Option<String>,
    pub continuation: Invocation,
}

#[derive(Default)]
pub(super) struct Observation {
    refs: Option<BTreeMap<String, String>>,
    objects: Option<BTreeMap<String, String>>,
    worktrees: Option<BTreeMap<PathBuf, Option<String>>>,
    pub events: Option<BTreeSet<String>>,
    intent: Option<RewriteIntent>,
    pub errors: Vec<String>,
}

fn observe<T>(
    errors: &mut Vec<String>,
    label: &str,
    read: impl FnOnce() -> Result<T>,
) -> Option<T> {
    match read() {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(format!("cannot observe {label}: {error:#}"));
            None
        }
    }
}

pub(super) fn observation(ctx: &Ctx, change: Option<&str>) -> Observation {
    let mut observed = Observation::default();
    observed.refs = observe(&mut observed.errors, "Git refs", || {
        let output = gitio::git(
            &ctx.cwd,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )?;
        output
            .lines()
            .map(|line| {
                let (name, oid) = line.split_once(' ').context("malformed ref observation")?;
                Ok((name.to_string(), oid.to_string()))
            })
            .collect()
    });
    observed.objects = observe(&mut observed.errors, "Git objects", || {
        let output = gitio::git(
            &ctx.cwd,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname) %(objecttype)",
            ],
        )?;
        output
            .lines()
            .map(|line| {
                let (oid, kind) = line
                    .split_once(' ')
                    .context("malformed object observation")?;
                Ok((oid.to_string(), kind.to_string()))
            })
            .collect()
    });
    observed.worktrees = observe(&mut observed.errors, "Git checkouts", || {
        Ok(gitio::worktree_inventory(&ctx.cwd)?
            .into_iter()
            .map(|entry| (entry.path, entry.branch))
            .collect())
    });
    // An uninitialized Arc store has no event or rewrite effects to observe.
    let root = observe(&mut observed.errors, "ledger location", || {
        Store::resolve_root(&ctx.cwd)
    });
    if let Some(root) = root {
        let repository_id = observe(&mut observed.errors, "receiver identity", || {
            Store::repository_id_at(&root)
        });
        match repository_id {
            Some(Some(_)) => {
                if let Some(store) = observe(&mut observed.errors, "ledger", || {
                    Store::discover_for_reading(&ctx.cwd)
                }) {
                    observed.events = observe(&mut observed.errors, "ledger events", || {
                        super::accept::event_ids(&store, change)
                    });
                    observed.intent = observe(&mut observed.errors, "rewrite intent", || {
                        store.rewrite_intent()
                    })
                    .flatten();
                }
            }
            Some(None) => {
                observed.events = Some(BTreeSet::new());
            }
            None => {}
        }
    }
    observed
}

pub(super) fn effects(
    ctx: &Ctx,
    before: &Observation,
    mut after: Observation,
    argv: &[String],
) -> (Effects, Vec<String>) {
    let mut effects = Effects::default();
    if let (Some(before), Some(after)) = (&before.refs, &after.refs) {
        for name in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            if before.get(name) != after.get(name) {
                effects.refs.push(RefEffect {
                    name: name.clone(),
                    before: before.get(name).cloned(),
                    after: after.get(name).cloned(),
                });
            }
        }
    }
    if let (Some(before), Some(after)) = (&before.objects, &after.objects) {
        for (oid, kind) in after.iter().filter(|(oid, _)| !before.contains_key(*oid)) {
            effects.objects.push(ObjectEffect {
                oid: oid.clone(),
                kind: kind.clone(),
            });
        }
    }
    if let (Some(before), Some(after)) = (&before.worktrees, &after.worktrees) {
        effects.worktrees_added = after
            .iter()
            .filter(|(path, _)| !before.contains_key(*path))
            .map(|(path, branch)| CheckoutEffect {
                path: path.clone(),
                branch: branch.clone(),
            })
            .collect();
        effects.worktrees_removed = before
            .iter()
            .filter(|(path, _)| !after.contains_key(*path))
            .map(|(path, branch)| CheckoutEffect {
                path: path.clone(),
                branch: branch.clone(),
            })
            .collect();
    }
    let event_ids = match (&before.events, &after.events) {
        (Some(before), Some(after)) => after.difference(before).cloned().collect(),
        _ => Vec::new(),
    };
    if let Some(intent) = after.intent {
        let store = observe(&mut after.errors, "pending rewrite ledger", || {
            Store::discover_for_reading(&ctx.cwd)
        });
        if let Some(store) = store {
            let recorded_event_id =
                observe(&mut after.errors, "pending rewrite publication", || {
                    rewrite::recorded_as(&store, &intent)
                })
                .flatten();
            let ref_states = intent
                .refs
                .keys()
                .map(|name| {
                    (
                        name.clone(),
                        after.refs.as_ref().and_then(|refs| refs.get(name).cloned()),
                    )
                })
                .collect();
            let continuation_argv =
                if argv.starts_with(&["arc".into(), "rewrite".into(), "sign".into()]) {
                    argv.to_vec()
                } else {
                    vec!["arc".into(), "rewrite".into(), "sign".into()]
                };
            let cwd = after
                .worktrees
                .as_ref()
                .and_then(|checkouts| {
                    checkouts
                        .iter()
                        .find(|(_, branch)| branch.as_deref() == Some(&intent.branch))
                        .map(|(path, _)| path.clone())
                })
                .unwrap_or_else(|| ctx.cwd.clone());
            effects.pending_rewrite = Some(PendingRewrite {
                path: store.rewrite_intent_path(),
                intent,
                ref_states,
                recorded_event_id,
                continuation: Invocation {
                    argv: continuation_argv,
                    cwd,
                },
            });
        }
    }
    effects.observation_errors = after.errors;
    (effects, event_ids)
}
