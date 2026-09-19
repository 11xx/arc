# Contribution trailers

Convention version: `arc-contribution-trailers/1`.

Git commit messages may name who materially contributed what through trailers:
`Key: value` lines at the end of the message, which
`git interpret-trailers --parse` reads and Git tooling already understands. The
convention is optional. It adds no approval authority, changes no commit's
author or committer, and depends on no ledger, journal, harness, or network
service.

## Keys

| Key | What it asserts |
| --- | --- |
| `Planned-by` | Materially shaped the plan the change was built from. |
| `Implemented-by` | Materially authored the change. |
| `Reviewed-by` | Examined the change's content and judged it acceptable. |
| `Orchestrated-by` | Coordinated the effort materially. Merely dispatching work, or containing a process that ran, is not coordination. |

Casing is as written above. Keys are matched by Git without regard to case, so
spelling a role key differently is a compatibility choice, not a second role.

One line per role and materially contributing identity. A participant serving
several roles appears once under each key. Repeated lines are allowed: several
people or agents may occupy any role. Exact `Key: value` pairs are written once;
do not repeat an identical line.

Contribution comes from an explicit participant or source assertion. Process
ancestry, a worker merely being spawned, and a plan merely being read do not
make anyone a contributor.

## Value forms

Agent, with the harness that recorded it and the model it recorded:

    Planned-by: codex:planner-model#high
    Implemented-by: codex:executor-model#medium
    Reviewed-by: codex:reviewer-model#high
    Orchestrated-by: claude:lead-model#high

`<harness>:<model>` is required and `#<effort>` is optional. Preserve the
recorded model and effort; omit a coordinate the source does not carry rather
than guessing one. Never invent an email for a model.

Human, as Git writes names and addresses:

    Reviewed-by: Ada Example <ada@example.invalid>

Both forms may appear under one key. A model or harness string names an
attributed execution class, not a unique or authenticated session: two
invocations of one model with different sessions share it unless a richer
coordinate is recorded. Trailer values alone never prove identity, and never
prove that a reviewer was independent of an author.

Values must be a single line. Reject a value containing a line feed or carriage
return, and reject an agent value whose harness, model, or effort is empty.

## What does not belong in a commit

Session identifiers, local filesystem paths, private evidence locations,
ticket URLs behind authentication, and plan JSON are not public commit
metadata. A model or harness string is the portable part of the recorded
identity; the session is the local part.

## Review credit

`Reviewed-by` asserts that the reviewed content was acceptable, not that
somebody looked at something. It is never synthesized from a completed read, a
negative audit, an unrelated review, or a discharged obligation. `Tested-by`,
if a repository supports it, describes successful declared testing, not a
review.

References to prior patchsets, integration commits, or review evidence say
which content was reviewed. Review credit does not transfer to content that
changed after it was given: an integration commit must not imply review of an
unexamined merge result, and a later review is not backfilled by rewriting an
already-reviewed commit, because a trailer changes the commit id.

Importing these strings never grants approval, discharges an obligation, or
waives a gate. A self-review does not become independent because its trailer
says `Reviewed-by`. Human attestations require the person's authorization;
agent attestations require recorded work and its outcome.

## Compatibility

The keys parse under plain `git interpret-trailers --parse` with no
configuration, and a repository's own trailer rules are honoured. Where a
project requires `Assisted-by` for disclosure, it is preserved as an orthogonal
compatibility and disclosure line; it is never silently replaced by a role key
the receiver does not recognize. Unknown role keys and legacy `Assisted-by`
values are retained as written, and unknown legacy assistance is never promoted
to a specific role by guesswork.

`Co-authored-by` and `Signed-off-by` keep their own semantics. Role credit does
not assert legal certification, Git authorship, or the transfer of any
responsibility.

## Order

Role keys are written in the order `Planned-by`, `Implemented-by`,
`Reviewed-by`, `Orchestrated-by`, after trailers of other kinds. Values under
one key keep the order they were first recorded. Unrelated trailers keep their
own order and are not moved.

## Checking without rewriting

    arc instructions git --check <message-file>

reports two different things: a value a role key cannot read (malformed), and a
key outside this convention (unsupported, retained untouched). It never
rewrites a message, and exits 1 when a role key carries a malformed value.
`arc instructions git` prints this specification.

## Not in this convention

A `Review-deferred:` marker is deliberately not part of version 1. Its scope and
reason would need a second obligation lifecycle to be useful, and absence of an
assertion must not read as an outstanding obligation.
