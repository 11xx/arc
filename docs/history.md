# History rewrites

## History rewrites

Git history is occasionally rewritten for reasons unrelated to any change:
signing an old commit, purging a secret, correcting an author, an upstream
force-push. Every revision the ledger recorded then names an object that no
longer exists.

The ledger is append-only, so migrating those events is unavailable by
construction — and should stay unavailable. A ledger that rewrites itself when
Git moves is a projection of whatever Git currently says, which is the thing
arc exists to supplement rather than mirror. So the rewrite is recorded as the
fact it is, and every derived reading follows revisions forward through it:

```sh
arc rewrite sign --dry-run                   # the map this would record
arc rewrite sign --key <id>                  # re-sign, move the refs, record
arc rewrite sign --key <id> --retag          # and recreate the annotated tags
arc history resolve <old-sha>                # exit 2 when nothing moved it
```

`arc rewrite sign` recreates every commit from `--from` through the branch head
so a single key signs them all. `--from` defaults to the oldest commit whose
signature is missing or made by another key. Only the signature and the commit
ids change: tree, parents, author, committer, dates, encoding, and message
bytes travel through untouched, so a commit recreated without signing
(`--no-sign`) has the object id it started with — which is what makes "only
the signature changed" checkable rather than hoped for. Trees are identical either
way, so tree-keyed gate evidence counts the same on both sides.

A commit may carry a header outside that set — `mergetag`, holding the whole
tag object, on a merge of a signed tag. Those travel too, in the order the
commit holds them, and the rewrite names each commit that carried one. Such a
commit is assembled and hashed rather than written by `git commit-tree`, which
has no flag for a header it does not know; a commit with nothing extra to
carry goes through `commit-tree`, so the ordinary case is signed by Git
itself. Both paths produce the same object for the same commit, which is what
keeps the unsigned recreation identical either way. A `mergetag` is a copy of
the tag that was merged, so it is carried byte for byte: it records what was
merged rather than pointing at something the map could follow forward.

It refuses a dirty worktree, a detached HEAD, and an ancestry it cannot
rebuild because a parent is neither in the map nor a commit here. It moves the
branch, every ref under `refs/arc/` that pins evidence, and the local branches
and tags whose tips are rewritten commits.

An annotated tag names its commit through a tag object of its own, so moving
the ref is not enough and re-pointing it means writing a new tag object —
which is a decision about a release rather than a consequence of re-signing a
branch. Left alone, the tag keeps naming a commit on the line that was
replaced, so `git describe` and the changelog projection have no release
boundary on the branch; the rewrite says so for each tag it leaves. `--retag`
recreates each one on the commit that replaced its target, carrying the tag
name, message, tagger and date, and signing it with the key the commits were
signed with where the original was signed. The old and the new tag object are
both named in the summary, so a release can be checked against what it was. A
branch
holding commits of its own on top of the range has no successor to move to —
its commits are still built on the line that was replaced — so the rewrite
names each such branch and the `git rebase --onto` that replays it. Until a
stranded branch is replayed it shares no commit with the branch it was cut
from, and every question comparing the two is unanswerable.

A rewrite performed elsewhere is recorded from its commit map instead:

```sh
git filter-repo --...
arc history rewrite --map .git/filter-repo/commit-map \
  --reason "purged a secret" --tool git-filter-repo
```

The commit map is `<old> <new>` per line, as `git filter-repo` writes it,
including its `old`/`new` column header. A new revision of all zeroes records a
commit the rewrite dropped, which survives at nothing; a line mapping a commit
to itself is not a move and is not recorded; and a revision mapped twice to
different successors is refused. A supplied map is judged by the same rules as
one arc computed. arc refuses a rewrite map claiming a revision survives as a
commit this repository does not hold. A map from somewhere else, or one naming
an object that is not a commit, is refused rather than recorded as fact.

Revisions match by prefix in either direction, so a map may abbreviate what
the ledger records in full and a caller may abbreviate what the map records;
an abbreviation matching more than one recorded revision names none of them.

Nothing already written changes. An old event keeps saying exactly what it
said, and every derived reading answers in the revisions this repository holds:
a change's status, its patchsets, gate evidence, waivers, closures, forge
observations, and a journal artifact's `verified` stamp all name the commit
their recorded revision is called here. `arc doctor` reports a
moved revision as `revision-rewritten` naming its successor, and a dropped one
as `revision-dropped`, instead of `dangling-revision`. `arc diff` renders the
surviving commit when a recorded one no longer resolves — every range it
renders, the patchset one, the head its finding anchors are checked against,
and the `--integrated` audit range alike — saying so on stderr. `arc events
--repository` reads the repository's own events, and a bundle carries
them so an imported change's revisions stay followable. An import refuses a
bundle whose rewrites contradict this repository's: two events with different
IDs can still disagree about one revision, so it is the combined map that must
hold together, and it is checked before anything is written. Recording a
rewrite locally is checked the same way and takes the same repository-scoped
lock, so two imports of different changes cannot each accept half of a
contradiction.

Both write paths also refuse a mapping that cannot mean what it says — a
rewrite to the zero object, where a dropped commit is spelled as no successor
at all; a revision mapped to itself; anything that is not a revision. A
mapping that reached the ledger another way is skipped by readers rather than
making the whole map unreadable, and `arc doctor` reports it as
`invalid-rewrite-mapping`.

Approval does not survive translation, and should not: a verdict binds to an
exact patchset head, and only a content comparison could say whether a
rewritten head is the same work. Re-approving a rewrite that preserved the
tree is cheap; one that did not must be re-reviewed.

A rewrite is not change-scoped — it happens to every recorded revision at once
— so it is stored as a repository-scoped event under `repository/events/`
rather than filed under a change that did not cause it. `arc doctor` checks
those events like any other, reporting a malformed or misscoped one instead of
letting it surface as a failure inside whatever first tried to read it.

