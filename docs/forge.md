# Forge projection

## Forge projection

For `forge`-profile changes that project onto a hosted pull request, arc
records and validates the forge facts an agent observed — it makes no
network call, invokes no `gh`, and autodetects nothing. Declare the
explicit tuple and policy up front, then record the observed link, checks,
and lifecycle:

```sh
arc forge declare tidal-fix --host github.com \
  --base-repo 11xx/streamrip --base-ref dev \
  --head-repo 11xx/streamrip --head-ref arc/tidal-fix \
  --policy same-repository-only        # or allowed-base-repo=<owner/name>
arc forge link tidal-fix --pr 1 --url https://github.com/11xx/streamrip/pull/1 \
  --base-repo 11xx/streamrip --base-ref dev \
  --head-repo 11xx/streamrip --head-ref arc/tidal-fix --head-sha <sha>
arc forge checks tidal-fix --pr-head <sha> --state not-configured
arc forge pr-state tidal-fix --state open   # merged requires --merge-sha
```

`arc forge link` fails closed with exit 10, appending no event, when the
observed tuple differs from the declaration on any axis or violates the
declared policy (`same-repository-only` requires base repo == head repo;
`allowed-base-repo=X` requires base repo == X). The `forge` status block
reports `projection` (undeclared/declared/linked), the observed link,
`head_match` against the current approved patchset head (the exact-head
rule), the recorded checks state (`stale` for an older head, `unknown`
when none exists at the linked head), `pr_state`, and `forge_ready` —
true only when linked, head-matched, checks in {passed, not-configured},
and the PR is open, with `not-configured` surfaced as an explicit caveat.
A lifecycle fact binds to the link it was read at: `arc forge pr-state`
requires a recorded link, takes the head from it rather than from the
caller, and `--link <EVENT>` refuses anything but the current link.
`--link` resolves against every link the change recorded, so a prefix
shared by a superseded link and the current one names neither.
Observations accumulate, and the reported `pr_state` is the newest one
matching the current link and head — so after a relink the state is
`unknown` (with a caveat) and `forge_ready` false until the new PR is
observed. Re-recording the same link is a second reading rather than a
relink, and does not invalidate what was observed at it. A fact recorded
before this binding names no link, and is read as current only while the
change has recorded one distinct link; a fact naming half a binding, or a
link this change never recorded, describes nothing and is never current.
A held, linked change renders an `awaiting_user` fact carrying the PR URL.
These facts are advisory rendering plus the fail-closed link validation;
they never change local `integrate` semantics. Close an externally merged
PR through `arc close --assert-integrated <merge-sha>`, which needs a
recorded patchset to name what landed.

