//! Rewriting this repository's history, and carrying every recorded revision
//! forward across it.
//!
//! A rewrite performed with an external tool leaves arc with a commit map and
//! a ledger full of revisions that no longer resolve. Performing it here
//! closes that gap in one act: the map is arc's own output, the refs arc keeps
//! move with the commits they point at, and the rewrite is recorded where
//! every reader already follows revisions forward.
//!
//! Only the loop belongs here. Recreating a commit is `gitio`, following a
//! revision forward is `rewrite`, and recording the map is `history`.

use super::*;
use crate::rewrite::{RefMove, RewriteIntent};
use std::collections::BTreeMap;

pub struct SignArgs {
    /// The key to sign with. Absent signs with the key Git is configured to
    /// use.
    pub key: Option<String>,
    /// The oldest commit to recreate. Absent starts at the oldest commit whose
    /// signature is missing or made by another key.
    pub from: Option<String>,
    /// Compute and print the map, then stop.
    pub dry_run: bool,
    /// Recreate the commits without signing them.
    ///
    /// Every commit still gets a new identity, because its parents do, so this
    /// exercises the whole rewrite — the walk, the map, the ref moves and the
    /// record — where no signing key is available.
    pub no_sign: bool,
    /// Recreate each annotated tag whose target the rewrite moved, on the
    /// commit that replaced it.
    pub retag: bool,
}

/// Re-sign this branch's history, or recreate it unsigned, carrying every
/// recorded revision forward.
pub fn sign(ctx: &Ctx, args: SignArgs) -> Result<i32> {
    let cwd = ctx.cwd.clone();
    let store = ctx.store()?;
    let branch = match open(ctx, &store)? {
        Opened::Finished(code) => return Ok(code),
        Opened::Branch(branch) => branch,
    };
    let head = gitio::branch_head(&cwd, &branch)?;
    let survey = gitio::signature_survey(&cwd, &head)?;
    let from = match &args.from {
        Some(from) => gitio::rev_parse(&cwd, from)?,
        None => match oldest_unsigned(&survey, args.key.as_deref()) {
            Some(from) => from,
            None => {
                println!("every commit on {branch} is signed by the requested key; nothing to do");
                return Ok(0);
            }
        },
    };
    let range = gitio::commits_from(&cwd, &from, &head)?;
    if range.is_empty() {
        bail!("{from} is not an ancestor of {branch}");
    }

    let sign = (!args.no_sign).then_some(args.key.as_deref());
    let mut walked = Walked::over(&range, branch, from, head, "arc rewrite sign");
    walked.reason = format!("re-signed {} from {}", walked.branch, short(&walked.from));
    for old in &range {
        let mut commit = gitio::read_commit(&cwd, old)?;
        commit.parents = map_parents(&cwd, old, &commit.parents, &walked.mapping)?;
        walked.recreated(&cwd, old, &commit, sign)?;
    }
    complete(ctx, &store, walked, sign, args.dry_run, args.retag)
}

pub struct TrailerArgs {
    /// The key to sign a recreated commit with. Absent signs with the key Git
    /// is configured to use.
    pub key: Option<String>,
    /// The oldest commit whose trailers are edited. The mode has nothing to
    /// infer a range from, so it is named rather than defaulted.
    pub from: String,
    /// Trailer keys to remove, matched without case.
    pub drop: Vec<String>,
    /// Whole trailer lines to add where the block does not carry them.
    pub append: Vec<String>,
    /// Compute and print the map, then stop.
    pub dry_run: bool,
    /// Recreate the edited commits without signing them.
    pub no_sign: bool,
    /// Recreate each annotated tag whose target the rewrite moved, on the
    /// commit that replaced it.
    pub retag: bool,
}

/// Edit the trailers of this branch's commit messages, carrying every
/// recorded revision forward.
///
/// A commit the edit did not reach, sitting under nothing the edit reached,
/// is left exactly as it is: no new object, and no entry in the map. That is
/// what separates this from re-signing, where every commit in range is
/// recreated because the one below it was.
pub fn trailers(ctx: &Ctx, args: TrailerArgs) -> Result<i32> {
    let cwd = ctx.cwd.clone();
    let store = ctx.store()?;
    let branch = match open(ctx, &store)? {
        Opened::Finished(code) => return Ok(code),
        Opened::Branch(branch) => branch,
    };
    let head = gitio::branch_head(&cwd, &branch)?;
    let from = gitio::rev_parse(&cwd, &args.from)?;
    let range = gitio::commits_from(&cwd, &from, &head)?;
    if range.is_empty() {
        bail!("{from} is not an ancestor of {branch}");
    }

    let sign = (!args.no_sign).then_some(args.key.as_deref());
    let mut walked = Walked::over(&range, branch, from, head, "arc rewrite trailers");
    walked.reason = edit_reason(&args, &walked.branch, &walked.from);
    for old in &range {
        let mut commit = gitio::read_commit(&cwd, old)?;
        let message = crate::trailers::edit(&commit.message, &args.drop, &args.append);
        let parents = map_parents(&cwd, old, &commit.parents, &walked.mapping)?;
        // Nothing to say differently and nowhere else to say it from: this is
        // the commit it already is, and recreating it would sign a fresh
        // object and record a move nothing asked for.
        if message == commit.message && parents == commit.parents {
            continue;
        }
        commit.message = message;
        commit.parents = parents;
        walked.recreated(&cwd, old, &commit, sign)?;
    }
    complete(ctx, &store, walked, sign, args.dry_run, args.retag)
}

/// What a trailer rewrite did, for the record it leaves: which keys went,
/// how many lines arrived, and over what.
fn edit_reason(args: &TrailerArgs, branch: &str, from: &str) -> String {
    let mut edits: Vec<String> = Vec::new();
    if !args.drop.is_empty() {
        edits.push(format!("dropped {}", args.drop.join(", ")));
    }
    if !args.append.is_empty() {
        edits.push(format!("appended {}", plural(args.append.len(), "line")));
    }
    format!("{}, on {branch} from {}", edits.join(", "), short(from))
}

/// The branch a rewrite is to run on, or the exit code of the unfinished one
/// that was finished instead.
enum Opened {
    Branch(String),
    Finished(i32),
}

/// Where every rewrite starts: the branch it runs on, an unfinished rewrite
/// finished before another is begun, and a worktree with nothing in it to
/// strand.
fn open(ctx: &Ctx, store: &Store) -> Result<Opened> {
    let branch = gitio::current_branch(&ctx.cwd)?
        .context("HEAD is detached; check out the branch whose history is being rewritten")?;
    // An interrupted rewrite is finished rather than repeated. Repeating it
    // recreates the same commits with fresh signatures, so the map would name
    // a second successor for every commit the first run already moved a ref
    // onto — a record that contradicts itself about every commit in range.
    //
    // This comes before the cleanliness check because an interruption after
    // the refs moved leaves the index describing the commit the branch moved
    // off, which reads as a worktree full of edits.
    if let Some(intent) = store.rewrite_intent()? {
        return Ok(Opened::Finished(resume(ctx, store, &branch, &intent)?));
    }
    // A rewrite that moved the branch out from under uncommitted work would
    // leave the operator holding a diff against a commit that no longer
    // exists.
    if !gitio::is_clean(&ctx.cwd)? {
        bail!("the worktree has uncommitted changes; commit or stash them first");
    }
    Ok(Opened::Branch(branch))
}

/// One mode's walk, in the terms every mode's remainder is the same in: what
/// the walk considered, what it replaced, and what the record is to say.
struct Walked {
    branch: String,
    from: String,
    head: String,
    /// How many commits the walk read, which is what "changed none of them"
    /// counts.
    considered: usize,
    mapping: BTreeMap<String, Option<String>>,
    /// Commits recreated with a header this module does not interpret, and
    /// which headers those were.
    carried: Vec<(String, Vec<String>)>,
    reason: String,
    tool: &'static str,
}

impl Walked {
    fn over(
        range: &[String],
        branch: String,
        from: String,
        head: String,
        tool: &'static str,
    ) -> Self {
        Walked {
            branch,
            from,
            head,
            considered: range.len(),
            mapping: BTreeMap::new(),
            carried: Vec::new(),
            reason: String::new(),
            tool,
        }
    }

    /// Write one commit as the walk now has it, and record the move unless
    /// the object that came back is the object it started as.
    fn recreated(
        &mut self,
        cwd: &Path,
        old: &str,
        commit: &gitio::RawCommit,
        sign: Option<Option<&str>>,
    ) -> Result<()> {
        // `commit-tree` writes a fixed set of headers in a fixed order from
        // the parts it is given, so a commit whose header text it cannot
        // spell back — one carrying `mergetag`, one whose headers sit in
        // another order, one whose identity line has whitespace of its own —
        // is assembled here instead. The common case stays with
        // `commit-tree`, where Git makes the signature itself.
        let new = if commit.commit_tree_writes_it_back() {
            gitio::commit_tree_as(cwd, commit, sign)?
        } else {
            let fields = commit.extra_header_fields();
            if !fields.is_empty() {
                self.carried.push((old.to_string(), fields));
            }
            gitio::write_commit_object(cwd, commit, sign)?
        };
        // A commit whose parents and content are unchanged and which gained no
        // signature is the commit it started as. Recording that as a rewrite
        // would claim a move that did not happen.
        if new != old {
            self.mapping.insert(old.to_string(), Some(new));
        }
        Ok(())
    }
}

/// Everything a rewrite does once its commits exist: plan the ref moves,
/// report a dry run, write the intent, apply it, and say what happened.
///
/// This is the whole of a rewrite that has nothing to do with what the mode
/// changed about a commit, which is why every mode ends here: a second way to
/// move refs or record a map is a second set of answers to how an
/// interruption is finished.
fn complete(
    ctx: &Ctx,
    store: &Store,
    walked: Walked,
    sign: Option<Option<&str>>,
    dry_run: bool,
    retag: bool,
) -> Result<i32> {
    let cwd = ctx.cwd.clone();
    let Walked {
        branch,
        from,
        head,
        considered,
        mapping,
        carried,
        reason,
        tool,
    } = walked;
    for (old, fields) in &carried {
        println!(
            "carried through on {}: the {} header",
            short(old),
            fields.join(" header and the ")
        );
    }
    if mapping.is_empty() {
        println!(
            "recreating {} changed none of them",
            plural(considered, "commit")
        );
        return Ok(0);
    }

    let mut moves = plan_ref_moves(&cwd, &branch, &from, &mapping, retag)?;
    if dry_run {
        for (old, new) in &mapping {
            println!("{old} {}", new.as_deref().unwrap_or("dropped"));
        }
        println!(
            "{} would be rewritten; nothing was moved or recorded",
            plural(mapping.len(), "commit")
        );
        // The tags are the half of a rewrite that is a decision rather than a
        // consequence, so a preview that stopped at the map would leave the
        // operator to run the real thing to find out what it does to a
        // release. No tag object is written to say it: the plan names the
        // commit each tag would be recreated on, which is what the decision
        // is about.
        for planned in &moves.retags {
            println!(
                "would re-point {}: tag object {} onto {}{}",
                planned.name,
                short(&planned.old),
                short(&planned.target),
                unsigned_note(planned.signed && sign.is_none())
            );
        }
        for name in &moves.left {
            println!("would leave alone: {name}");
        }
        return Ok(0);
    }

    let new_head = mapping
        .get(&head)
        .and_then(|new| new.clone())
        .unwrap_or_else(|| head.clone());
    let retagged = write_retags(&cwd, &mut moves, sign)?;
    // The branch moves inside the same transaction as everything else. Moving
    // it last, on its own, is what leaves the refs arc keeps on one history
    // and the branch on another.
    moves.moves.insert(
        format!("refs/heads/{branch}"),
        RefMove {
            old: head.clone(),
            new: new_head.clone(),
        },
    );
    let intent = RewriteIntent {
        schema_version: crate::model::SCHEMA_VERSION,
        branch: branch.clone(),
        from: from.clone(),
        reason,
        tool: Some(tool.to_string()),
        mapping: mapping.clone(),
        refs: moves.moves.clone(),
        created_at: chrono::Utc::now(),
    };
    // Everything above only wrote objects nothing points at, which change
    // nothing about what the repository says. From here the rewrite becomes
    // visible, so what it is about to do is on disk first.
    store.write_rewrite_intent(&intent)?;
    interrupted_after("intent")?;
    let event_id = apply(ctx, store, &intent)?;

    println!(
        "{} rewritten on {branch}; head is now {}",
        plural(mapping.len(), "commit"),
        short(&new_head)
    );
    println!("{} moved", plural(intent.refs.len(), "ref"));
    println!("rewrite recorded as {event_id}");
    for retag in &retagged {
        println!(
            "re-pointed {}: tag object {} is now {}{}",
            retag.name,
            short(&retag.old),
            short(&retag.new),
            unsigned_note(retag.signature_dropped)
        );
    }
    for name in &moves.left {
        println!("left alone: {name}");
    }
    // A stranded branch shares no commit with the branch it was cut from, so
    // every question that compares the two — how far behind it is, what
    // merging it would ship — has no answer until it is replayed. Saying which
    // branches and how to replay them is the difference between a rewrite the
    // operator finishes and one they discover half-done.
    for name in &moves.stranded {
        println!(
            "stranded: {name} sits on the replaced line; replay it with `git rebase --onto \
             {} {} {}`",
            short(&new_head),
            short(&from),
            name.trim_start_matches("refs/heads/")
        );
    }
    Ok(0)
}

/// What a tag recreation says about a signature it could not make.
fn unsigned_note(dropped: bool) -> &'static str {
    match dropped {
        true => ", unsigned because the rewrite signs nothing",
        false => "",
    }
}

/// Finish a rewrite an earlier run left half-applied.
///
/// The intent names every commit the rewrite produced, so nothing is
/// recreated: whatever remains is a ref to move, a map to record, or both.
fn resume(ctx: &Ctx, store: &Store, branch: &str, intent: &RewriteIntent) -> Result<i32> {
    if branch != intent.branch {
        bail!(
            "an unfinished rewrite of {} is on record; check that branch out to finish it, or \
             remove {}",
            intent.branch,
            store.rewrite_intent_path().display()
        );
    }
    println!(
        "finishing the rewrite of {} from {}, recorded {}",
        intent.branch,
        short(&intent.from),
        intent.created_at.to_rfc3339()
    );
    let event_id = apply(ctx, store, intent)?;
    println!("rewrite recorded as {event_id}");
    Ok(0)
}

/// Apply an intent, from wherever it got to: move the refs that have not
/// moved, record the map unless it is recorded already, then drop the intent.
///
/// Answers with the event holding the map, whether this call recorded it or
/// an interrupted one already had.
fn apply(ctx: &Ctx, store: &Store, intent: &RewriteIntent) -> Result<String> {
    let cwd = &ctx.cwd;
    // A commit the intent names has to still be here. Objects nothing points
    // at are pruned, and a map recorded over pruned successors would name
    // commits no reader can reach.
    let mut pruned: Vec<&str> = Vec::new();
    for successor in intent.mapping.values().flatten() {
        if !gitio::commit_exists(cwd, successor)? {
            pruned.push(successor);
        }
    }
    if !pruned.is_empty() {
        bail!(
            "{} of the commits this rewrite produced are no longer in this repository ({}); the \
             rewrite cannot be finished. Remove {} and run it again.",
            pruned.len(),
            pruned
                .iter()
                .take(3)
                .map(|revision| short(revision))
                .collect::<Vec<_>>()
                .join(", "),
            store.rewrite_intent_path().display()
        );
    }

    let mut pending: Vec<gitio::RefUpdate> = Vec::new();
    for (name, RefMove { old, new }) in &intent.refs {
        match gitio::ref_value(cwd, name)?.as_deref() {
            // Already where the rewrite is taking it, from a run that got
            // this far.
            Some(value) if value == new => {}
            Some(value) if value == old => pending.push(gitio::RefUpdate {
                name: name.clone(),
                old: old.clone(),
                new: new.clone(),
            }),
            // Something other than this rewrite decided where this ref
            // points. Choosing between that decision and the rewrite's is
            // not arc's to make.
            other => bail!(
                "{name} holds {}, which is neither what the rewrite read ({}) nor what it writes \
                 ({}); settle it by hand, then remove {}",
                other.map_or("nothing".to_string(), |value| short(value).to_string()),
                short(old),
                short(new),
                store.rewrite_intent_path().display()
            ),
        }
    }
    gitio::update_refs(cwd, &pending)?;
    interrupted_after("refs")?;

    let event_id = match recorded_as(store, intent)? {
        Some(event_id) => event_id,
        None => history::record_mapping(
            ctx,
            intent.mapping.clone(),
            intent.reason.clone(),
            intent.tool.clone(),
        )?,
    };
    // The map is recorded and every ref names a commit it describes, so
    // nothing is left to finish.
    store.clear_rewrite_intent()?;

    // The branch moved under the checkout, whose index still describes the
    // commit it moved off. Nothing about the tree changed, so this only
    // teaches the index which commit it is looking at.
    let branch_ref = format!("refs/heads/{}", intent.branch);
    if let Some(new_head) = intent.refs.get(&branch_ref) {
        gitio::git(cwd, &["reset", "--mixed", "--quiet", &new_head.new])?;
    }
    Ok(event_id)
}

/// The event already holding this intent's map, if an interrupted run
/// recorded it. Recording it a second time would put two events on record
/// claiming one rewrite.
fn recorded_as(store: &Store, intent: &RewriteIntent) -> Result<Option<String>> {
    Ok(store
        .load_repository_events()?
        .into_iter()
        .find(|event| {
            matches!(
                &event.payload,
                Payload::HistoryRewritten { mapping, .. } if mapping == &intent.mapping
            )
        })
        .map(|event| event.event_id))
}

/// A fault the test suite injects to stop a rewrite between two of its
/// phases, so the resume path is exercised on a repository a real
/// interruption could have left. Unset, which is every run that is not a
/// test, this does nothing.
fn interrupted_after(phase: &str) -> Result<()> {
    if std::env::var("ARC_REWRITE_INTERRUPT").as_deref() == Ok(phase) {
        bail!("interrupted after {phase} by ARC_REWRITE_INTERRUPT");
    }
    Ok(())
}

/// The oldest commit that is not signed by the key being signed with.
///
/// `G` and `U` are Git's letters for a good signature; both mean a signature
/// arc can read a key off. Anything else — no signature, one that cannot be
/// checked, one from a revoked or expired key — is a commit to recreate.
/// Without a named key, the newest signature's key is the one to match: the
/// point is one key over the whole history, and the key in use is the one Git
/// most recently signed with.
fn oldest_unsigned(survey: &[(String, String, String)], key: Option<&str>) -> Option<String> {
    let wanted = key
        .map(|key| key.trim_end_matches('!').to_string())
        .or_else(|| {
            survey
                .iter()
                .find(|(_, verification, _)| is_good(verification))
                .map(|(_, _, signer)| signer.clone())
        });
    // The survey is newest first, so the last row that needs signing is the
    // oldest commit that does.
    survey
        .iter()
        .rfind(|(_, verification, signer)| match &wanted {
            // A key that signed nothing in this history cannot be matched
            // against, so every commit is one to sign.
            Some(wanted) => !is_good(verification) || !same_key(signer, wanted),
            None => true,
        })
        .map(|(commit, _, _)| commit.clone())
}

/// Whether Git read a signature it could check.
fn is_good(verification: &str) -> bool {
    matches!(verification, "G" | "U")
}

/// Whether two spellings name one key.
///
/// A key is named by its fingerprint, by a long id, or by a short one, and
/// each shorter form is a suffix of the longer. Git reports whichever the
/// signature carries, which is rarely the spelling the operator typed, so
/// demanding they match exactly would re-sign a history that is already
/// signed by the requested key — and gpg produces a different signature every
/// time, so that rewrite would look like progress and never converge.
fn same_key(left: &str, right: &str) -> bool {
    let (left, right) = (left.to_ascii_uppercase(), right.to_ascii_uppercase());
    !left.is_empty() && !right.is_empty() && (left.ends_with(&right) || right.ends_with(&left))
}

/// Each parent as it stands after the rewrite.
///
/// A parent outside the range keeps its identity, which is what makes the
/// range a range. A parent that is neither rewritten nor still a commit here
/// cannot be carried forward at all, and a rewrite that picked something is
/// guessing at ancestry.
fn map_parents(
    cwd: &Path,
    commit: &str,
    parents: &[String],
    mapping: &BTreeMap<String, Option<String>>,
) -> Result<Vec<String>> {
    parents
        .iter()
        .map(|parent| match mapping.get(parent) {
            Some(Some(new)) => Ok(new.clone()),
            Some(None) => bail!(
                "{} is a parent of {} and the rewrite dropped it; the ancestry cannot be rebuilt",
                short(parent),
                short(commit)
            ),
            None if gitio::commit_exists(cwd, parent)? => Ok(parent.clone()),
            None => bail!(
                "{} is a parent of {} and is not a commit in this repository; the ancestry \
                 cannot be rebuilt",
                short(parent),
                short(commit)
            ),
        })
        .collect()
}

/// Which refs a rewrite moves, and which it reports instead.
struct RefMoves {
    /// Every ref that moves, refname to the values it moves between. The
    /// branch being rewritten is added by the caller, so the plan and the
    /// transaction that applies it are the same set.
    moves: BTreeMap<String, RefMove>,
    /// Refs naming a rewritten commit that arc will not move, each with why.
    left: Vec<String>,
    /// Branches whose own commits sit on top of the rewritten range, so
    /// moving the ref would not help: they need replaying.
    stranded: Vec<String>,
    /// Annotated tags to recreate on the commit that replaced their target.
    retags: Vec<PlannedRetag>,
}

/// One annotated tag a rewrite is to recreate, as the plan knows it: the tag
/// object the ref names, the commit it is to name instead, and whether it
/// carries a signature a recreation would have to make again.
struct PlannedRetag {
    name: String,
    old: String,
    target: String,
    signed: bool,
}

/// One annotated tag recreated on a rewritten commit.
struct Retag {
    name: String,
    /// The tag object the ref named, and the one it is to name.
    old: String,
    new: String,
    /// Whether the original carried a signature this recreation could not
    /// make, which is the case when the rewrite signs nothing.
    signature_dropped: bool,
}

/// Every ref that points at a rewritten commit, and what becomes of it.
///
/// arc's own refs pin evidence — reviewed patchset heads, evaluated trees,
/// synthesized merges — and evidence that stops being reachable stops being
/// evidence, so they all move. Branches and tags move too, since a rewrite
/// nobody's branch survived is a rewrite that stranded the work.
///
/// An annotated tag is re-pointed only when asked for: re-pointing one means
/// writing a new tag object, which is a decision about somebody's release
/// rather than a consequence of rewriting a branch. Left alone, it keeps
/// naming the commit it named, which is on the line the rewrite replaced.
///
/// Nothing here writes an object, so a rewrite can be described before it is
/// performed. The tag objects a re-pointing needs are written by
/// [`write_retags`], in the phase that is allowed to change what the
/// repository holds.
fn plan_ref_moves(
    cwd: &Path,
    branch: &str,
    from: &str,
    mapping: &BTreeMap<String, Option<String>>,
    retag: bool,
) -> Result<RefMoves> {
    let mut moves = RefMoves {
        moves: BTreeMap::new(),
        left: Vec::new(),
        stranded: Vec::new(),
        retags: Vec::new(),
    };
    let successor = |old: &str| mapping.get(old).and_then(|new| new.clone());
    for (name, value) in gitio::list_refs(cwd, "refs/arc/")? {
        if let Some(new) = successor(&value) {
            moves.moves.insert(name, RefMove { old: value, new });
        }
    }
    let branch_ref = format!("refs/heads/{branch}");
    for reference in gitio::local_refs(cwd)? {
        let gitio::LocalRef {
            name,
            object,
            kind,
            commit,
        } = reference;
        if name == branch_ref {
            continue;
        }
        // An annotated tag names its commit through its own object, so the
        // commit it leads to is what the map answers for.
        let Some(new) = successor(&commit) else {
            // A branch with commits of its own on top of the range has a tip
            // the rewrite never reached, so there is no successor to move it
            // to. Its commits are still built on the line that was replaced,
            // and only replaying them onto the new one reunites the two.
            if kind == "commit" && gitio::is_ancestor(cwd, from, &commit).unwrap_or(false) {
                moves.stranded.push(name);
            }
            continue;
        };
        if kind == "commit" {
            moves.moves.insert(name, RefMove { old: object, new });
        } else if retag {
            moves.retags.push(PlannedRetag {
                name,
                signed: gitio::read_tag(cwd, &object)?.signed,
                old: object,
                target: new,
            });
        } else {
            moves.left.push(format!(
                "{name} is an annotated tag naming a replaced commit; `git describe` and the \
                 changelog projection have no release boundary until it is re-pointed, which \
                 `--retag` does"
            ));
        }
    }
    Ok(moves)
}

/// Recreate each planned tag on the commit that replaced its target, and add
/// the ref move that names the object written for it.
///
/// The tag name, message, tagger and date are the original's, so the only
/// thing a new object says differently is which commit it names. It is signed
/// when the original was and a key is available; a rewrite that signs nothing
/// cannot sign a tag either, and dropping the signature is reported rather
/// than passed off as the tag it recreated.
///
/// The objects are written here and the refs moved later, alongside every
/// other ref: an unreferenced tag object changes nothing about what the
/// repository says, so a rewrite that fails between the two leaves no tag
/// half-moved.
fn write_retags(
    cwd: &Path,
    moves: &mut RefMoves,
    sign: Option<Option<&str>>,
) -> Result<Vec<Retag>> {
    let mut written = Vec::new();
    for planned in &moves.retags {
        let mut tag = gitio::read_tag(cwd, &planned.old)?;
        tag.object = planned.target.clone();
        let sign = if tag.signed { sign } else { None };
        let retag = Retag {
            name: planned.name.clone(),
            old: planned.old.clone(),
            new: gitio::write_tag_object(cwd, &tag, sign)?,
            signature_dropped: tag.signed && sign.is_none(),
        };
        moves.moves.insert(
            retag.name.clone(),
            RefMove {
                old: retag.old.clone(),
                new: retag.new.clone(),
            },
        );
        written.push(retag);
    }
    Ok(written)
}

fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        count => format!("{count} {noun}s"),
    }
}

fn short(revision: &str) -> &str {
    &revision[..revision.len().min(8)]
}
