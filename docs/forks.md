# Forks

## Forks and worktree cost

A fork is a worktree on a `fork/<slug>` branch, deliberately outside the
change lifecycle: no ledger change, no gates, nothing merged. `arc fork begin
<slug>` creates it and journals a marker, `arc fork adopt <slug>` records a
hand-made one, `arc fork list` reports every fork from markers and branches
together, and `arc fork retire <slug> <outcome>` records the disposition and
removes the worktree while keeping the branch. `arc integrate` refuses inside
a fork worktree.

`arc fork thread <slug>` prints the identity the marker recorded — harness,
session, model, actor — and, for a harness with a stable resume form, the
command that reopens that session. A field the marker does not carry prints
as absent; no identity is inferred from a branch or directory name.

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
advice: arc never refuses to create a worktree over disk space. A project
can declare its own floor with `worktree_free_floor_bytes` in policy, which
adds a second warning against that threshold.

Inside a fork worktree, `arc journal verified <file>` stamps the fork's own
head and records the scope that names it, and the queue row reads `[verified
at <rev> in fork <slug> ...]`. The anchor's head and a fork's head are
different code; a fork-scoped stamp makes no claim about whether the anchor
has moved, because a revision off the anchor's line of history cannot be
compared with it.

