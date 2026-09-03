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
}

/// Re-sign this branch's history, or recreate it unsigned, carrying every
/// recorded revision forward.
pub fn sign(ctx: &Ctx, args: SignArgs) -> Result<i32> {
    let cwd = ctx.cwd.clone();
    let branch = gitio::current_branch(&cwd)?
        .context("HEAD is detached; check out the branch whose history is being rewritten")?;
    // A rewrite that moved the branch out from under uncommitted work would
    // leave the operator holding a diff against a commit that no longer
    // exists.
    if !gitio::is_clean(&cwd)? {
        bail!("the worktree has uncommitted changes; commit or stash them first");
    }
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
    let mut mapping: BTreeMap<String, Option<String>> = BTreeMap::new();
    for old in &range {
        let mut commit = gitio::read_commit(&cwd, old)?;
        commit.parents = map_parents(&cwd, old, &commit.parents, &mapping)?;
        let new = gitio::commit_tree_as(&cwd, &commit, sign)?;
        // A commit whose parents and content are unchanged and which gained no
        // signature is the commit it started as. Recording that as a rewrite
        // would claim a move that did not happen.
        if &new != old {
            mapping.insert(old.clone(), Some(new));
        }
    }
    if mapping.is_empty() {
        println!(
            "recreating {} changed none of them",
            plural(range.len(), "commit")
        );
        return Ok(0);
    }

    if args.dry_run {
        for (old, new) in &mapping {
            println!("{old} {}", new.as_deref().unwrap_or("dropped"));
        }
        println!(
            "{} would be rewritten; nothing was moved or recorded",
            plural(mapping.len(), "commit")
        );
        return Ok(0);
    }

    let new_head = mapping
        .get(&head)
        .and_then(|new| new.clone())
        .unwrap_or_else(|| head.clone());
    let moves = plan_ref_moves(&cwd, &branch, &from, &mapping)?;
    let event_id = history::record_mapping(
        ctx,
        mapping.clone(),
        format!("re-signed {branch} from {}", short(&from)),
        Some("arc rewrite sign".to_string()),
    )?;
    // The record goes in before the refs move, so an interruption leaves a
    // repository whose recorded rewrite describes commits that are all
    // present. The other order leaves refs pointing at commits nothing
    // resolves back to.
    for (name, value) in &moves.moved {
        gitio::update_ref(&cwd, name, value)?;
    }
    gitio::update_ref(&cwd, &format!("refs/heads/{branch}"), &new_head)?;
    // The branch just moved under the checkout, whose index still describes
    // the commit it moved off. Nothing about the tree changed, so this only
    // teaches the index which commit it is looking at.
    gitio::git(&cwd, &["reset", "--mixed", "--quiet", &new_head])?;

    println!(
        "{} rewritten on {branch}; head is now {}",
        plural(mapping.len(), "commit"),
        short(&new_head)
    );
    println!("{} moved", plural(moves.moved.len() + 1, "ref"));
    println!("rewrite recorded as {event_id}");
    for name in &moves.left {
        println!("left alone: {name}");
    }
    Ok(0)
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
    /// Refname to the commit it moves to. The branch being rewritten is not
    /// here: it moves last, after everything else is in place.
    moved: Vec<(String, String)>,
    /// Refs naming a rewritten commit that arc will not move, each with why.
    left: Vec<String>,
}

/// Every ref that points at a rewritten commit, and what becomes of it.
///
/// arc's own refs pin evidence — reviewed patchset heads, evaluated trees,
/// synthesized merges — and evidence that stops being reachable stops being
/// evidence, so they all move. Branches and tags move too, since a rewrite
/// nobody's branch survived is a rewrite that stranded the work.
///
/// An annotated tag is reported rather than moved: its object carries its own
/// message and possibly its own signature, and re-pointing it means writing a
/// new tag object, which is a decision about somebody's release rather than a
/// consequence of re-signing a branch.
fn plan_ref_moves(
    cwd: &Path,
    branch: &str,
    from: &str,
    mapping: &BTreeMap<String, Option<String>>,
) -> Result<RefMoves> {
    let mut moves = RefMoves {
        moved: Vec::new(),
        left: Vec::new(),
    };
    let successor = |old: &str| mapping.get(old).and_then(|new| new.clone());
    for (name, value) in gitio::list_refs(cwd, "refs/arc/")? {
        if let Some(new) = successor(&value) {
            moves.moved.push((name, new));
        }
    }
    let branch_ref = format!("refs/heads/{branch}");
    for (name, value, kind) in gitio::local_refs(cwd)? {
        if name == branch_ref {
            continue;
        }
        let Some(new) = successor(&value) else {
            // A branch or tag whose tip was not rewritten still descends from
            // the range whenever the rewrite reached its history, and it is
            // then pointing into a line nothing else references.
            if kind == "commit" && gitio::is_ancestor(cwd, from, &value).unwrap_or(false) {
                moves.left.push(format!(
                    "{name} descends from the range but its tip was not rewritten"
                ));
            }
            continue;
        };
        if kind == "commit" {
            moves.moved.push((name, new));
        } else {
            moves.left.push(format!(
                "{name} is an annotated tag; re-point it deliberately"
            ));
        }
    }
    Ok(moves)
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
