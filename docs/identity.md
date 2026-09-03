# Identity

## Identity

Every event records an actor, and optionally a harness and native
session ID: `--actor/--harness/--session` or `ARC_ACTOR`, `ARC_HARNESS`,
`ARC_SESSION`. Actor defaults to `git config user.name`. `claim`,
`release-claim`, and `stage` require nonempty harness and session values;
identity is the actor + harness + session tuple.

Explicit identity always wins. Set `[identity] detect = true` in the config
file to fill omitted harness, session, and model values from the running
harness's own session store. Detection is off by default and does not mix a
detected session into a different explicitly selected harness.

Journal events additionally record the acting model via `--model` or
`ARC_MODEL`, a `model-slug[#effort]` string (e.g. `kimi-k3#high`,
`gpt-5.6-sol#low`) matching the `Assisted-by: Harness:Model#Effort` grammar.
It is optional everywhere: an empty value is treated as unset, and an absent
model is serialized as absent — never stamped "unknown".

When a lead runs ceremony for a sandboxed executor — committing its staged
work, then claiming, snapshotting, or reviewing — `--on-behalf-of <subject>`
(or `ARC_ON_BEHALF_OF`) records who the action is *for* while `actor` stays the
invoker who ran it. The **effective author** of an event is
`on_behalf_of.unwrap_or(actor)`. `forbid_self_approval` compares effective
authors, so a lead snapshotting on behalf of an executor and then approving as
itself is not self-approval, whereas approving `--on-behalf-of` that same
executor is. A declared subject is always somebody's claim, so it satisfies
`require_declared_actor` however the invoker was identified. Claim ownership is unaffected: it still matches on the invoker's
actor + harness + session tuple, and `on_behalf_of` is recorded and rendered
but never changes who owns or may release a claim. The field is additive and
serialized only when set, so existing events and bundles round-trip unchanged.

Journal events carry the same pair. A `journal-events/1` event records
`on_behalf_of` beside `actor`, and the structured views show both: `journal
events`, each position and answer in `journal discussion --json`, each question
in `journal questions --json`, and the verification stamps on `journal open`
and `catchup`. Prose headings name the identity that argued and never the
subject, so who a lead recorded work for is a question for the structured view.

