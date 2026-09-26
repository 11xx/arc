# Replica pairing and integration authority

Arc pairs independent stores under one logical project ID. Every store keeps
its own repository ID, and every event keeps the ID of the store that recorded
it. A replica name is unique within its logical project.

## Pair stores

Read the repository ID on the recipient store:

```sh
arc replica id --json
```

The first store starts the project and records a peer by its repository ID:

```sh
arc replica init workstation
arc replica pair agent-host --repository-id <recipient-repository-id>
arc replica export --output pairing.json
```

The recipient previews and imports the pairing record:

```sh
arc replica import pairing.json --dry-run
arc replica import pairing.json
arc replica status
```

The imported record names both replicas. The recipient adopts the project
identity only through that import. Equal paths, Git remotes, and journal names
do not pair stores.

Replica bundles contain replica identities and authority events. Checkout
paths, worktree bindings, and journal directories stay local. A path carried by
an imported change event remains provenance in the ledger; it does not become
the importing store's worktree binding or a location Arc resolves.

## Move authority

The first replica holds integration authority. It can offer authority to a
paired recipient:

```sh
arc replica authority offer --to agent-host
arc replica export --output authority-offer.json
```

The offering replica relinquishes authority when it records the offer. Its
status, catchup, and doctor views show the offer in flight. The named recipient
acquires authority when it imports the offer:

```sh
arc replica import authority-offer.json --dry-run
arc replica import authority-offer.json
```

Importing the same file again writes nothing. A paired replica that lacks
authority cannot integrate a change.

Arc exits 17 when a paired replica does not hold integration authority; the refusal names the holder or an offer in flight.

The recipient can export its known replica events and the original store can
import that file to learn that the offer was accepted. Arc makes no network
call; operators move the files over an authenticated path they already use.
Each view reflects the events in its local store. A replica learns about a
remote offer or return request only when it imports a file carrying that event.

The offering replica can request return of an active offer by recording a reason:

```sh
arc replica authority reclaim --because "The transfer file was reported missing."
arc replica export --output authority-return-request.json
```

The request grants nothing to the origin. The recipient imports it, explicitly
confirms relinquishment, and exports the confirmation:

```sh
arc replica import authority-return-request.json
arc replica authority confirm-return
arc replica export --output authority-return-confirmation.json
```

The recipient is blocked as soon as it confirms. The origin remains blocked
until it imports `authority-return-confirmation.json`. A recipient that has
forwarded authority cannot confirm return of the earlier offer. An offline
recipient cannot be assumed to have stopped integrating, so its missing
confirmation leaves the origin blocked. Repeated and delayed imports preserve
the recorded authority chain. A replica that later reacquires authority can
confirm only a request for the offer that granted its current authority;
requests for earlier forwarded offers do not select a return.

Replica event, bundle, and import receipt schemas use version 2 for confirmed
returns. Safe version 1 identity, pairing, offer, and acquisition events remain
readable. A version 1 reclaim event is refused because it does not prove the
recipient relinquished authority. Version 1 bundles are refused explicitly.

## Exchange journal artifacts

A change bundle carries a ledger, not a journal. Selected artifacts travel as
their own versioned file:

```sh
arc journal export 20260923T173916Z-topic-discussion.md --output discussion.json
arc journal import discussion.json --dry-run
arc journal import discussion.json
```

The bundle carries each artifact's body, the events the exporting journal
recorded about it, its body digest, the exporting replica, and the artifacts
the selection references by filename. `transition` links and `decision`
references are the filename references that count; each is followed until the
closure is complete, and an artifact this journal cannot produce refuses the
export by name. A decision recorded in another project's journal cannot travel
in a bundle for this one and refuses the export too.

The whole bundle is validated before anything is written. A bundle naming
another logical project, or a source replica that is not paired here, is
refused and writes nothing. An artifact the receiving journal already holds in
a different storage tier, or with a different digest, refuses the whole
import. An artifact that is absent lands in the hot journal or the cold
archive the bundle records, and the events arrive with the provenance they
were recorded under. Importing the same bundle twice changes nothing: the
first import records a receipt naming the source replica and bundle digest.

A live claim arriving on an artifact a live local claim holds is reported as a
contest naming both replicas, and nothing on either claim's behalf is
recorded. Resolve it through the ordinary takeover path.

## Claims and local liveness

Claims remain local liveness facts. When an imported change bundle brings a
live peer claim for a change that also has a distinct live local claim, Arc
reports both replica names and claim owners, then imports no events. Resolve
the conflict through the ordinary claim takeover path.

Replica imports support `--dry-run`, validate the whole bundle before writing,
and record a receipt naming the source replica and bundle digest. Imported
event provenance is preserved.
