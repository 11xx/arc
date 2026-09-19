# Forks

## Forks and worktree cost

A fork is a branch deliberately outside the change lifecycle: no ledger
change, no gates, nothing merged. `arc fork begin <slug>` creates one on a
`fork/<slug>` branch with its own worktree, and `arc fork adopt <slug>` records
a branch the operator made by hand — any local branch, named by `--branch` and
defaulting to `fork/<slug>`, keeping whatever name it carries. It records the
checkout Git reports holding that branch, the path `fork list` names, and a
branch with no checkout is recorded with none. An open change's branch is
refused: a marker over it would make the change unintegrable, and nothing
un-adopts a marker. The marker is what makes an adopted branch a fork, and it
is also what makes the branch unintegrable: a change on it is refused from
every directory, whoever named the branch. `arc fork list` reports every fork from markers and branches together,
with when it opened, its head, the uncommitted and untracked counts in its
checkout, and the changes promoted from it; counts come from Git's own status,
never from reading the work. `arc fork retire <slug> <outcome>` records the
disposition and removes the worktree while keeping the branch; removal refuses
while the checkout holds work arc cannot see, and `--force` is the operator's
decision to discard it. A fork's work is unintegrable, and that boundary binds
to the change: a change whose branch is a fork's is refused by `arc integrate`
and `arc check` from every directory, and `arc begin` refuses to open one.
Where the caller stands decides nothing — an ordinary change integrates from
inside a fork's worktree.

`arc begin <slug> --from-fork <fork>` is how fork work becomes a change: the
new branch starts at the integration target and the fork's own commits are
replayed onto it, and the recorded link names the fork slug and the source
base, head, and tree. The fork keeps its branch, its worktree, and its
marker, and one fork may feed several changes. The link is not review
coverage: a fork's review evidence grants the change no credit, and the change
still snapshots its own patchset, runs its own gates, and takes its own
verdict. `arc resume` on a promoted change names the source and lists the
artifacts filed under the fork's topic, where the fork's review evidence and
the findings it left open live.

`arc fork thread <slug>` prints the identity the marker recorded — harness,
session, model, actor — and, for a harness with a stable resume form, the
command that reopens that session. A field the marker does not carry prints
as absent: arc never infers an identity from a branch or a directory name.

`arc catchup` and `arc inbox` also report the branches and worktrees no owner
names: local branches no open change and no active fork holds, split into the
unmerged and the already-merged cleanup candidates, and registered worktrees
no owner names with their uncommitted and untracked file counts. Each row
names the action that gives it an owner — `arc begin <slug> --adopt <branch>`,
`arc fork adopt <slug> --branch <branch>`, or `git branch -d <merged ref>`.
Reading refs and `git worktree list` is the whole scan: no tree is walked and
no file content is read.

`arc catchup` and `arc doctor` report what the open changes' worktrees occupy
and, separately, what the fork checkouts occupy. The two are never summed:
nothing in the change lifecycle retires a fork, so a fork is routinely the
longest-lived checkout on the disk, and a retired fork whose worktree was
kept stays in the accounting even after it leaves the live-fork listing.

Every total names the method that produced it. Sizes come from `du`, which
sums apparent size — the bytes files claim, not the blocks the filesystem
spent. Where the mount compresses or deduplicates the two diverge without
bound, so physical cost is reported as `unknown`, with the reason named when
the filesystem gives one. The mount holding the worktrees root is reported
with its free space; an absent `findmnt` leaves the filesystem type unknown
rather than guessed.

`begin` and `fork begin` print what the worktrees root has left before
creating a worktree, and warn when the mount is close to full. Both are
advice. arc never refuses to create a worktree over disk space. A project
can declare its own floor with `worktree_free_floor_bytes` in policy, which
adds a second warning against that threshold.

Inside a fork worktree, `arc journal verified <file>` stamps the fork's own
head and records the scope that names it, and the queue row reads `[verified
at <rev> in fork <slug> ...]`. The anchor's head and a fork's head are
different code; a fork-scoped stamp makes no claim about whether the anchor
has moved, because a revision off the anchor's line of history cannot be
compared with it.

