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
remote offer or reclaim only when it imports a file carrying that event.

An offering replica can reclaim an unresolved offer by recording a reason:

```sh
arc replica authority reclaim --because "The transfer file was reported missing."
arc replica export --output authority-reclaim.json
```

A reclaim is an explicit operator decision. Importing the reclaim event makes
the offering replica the authority holder and records the reason. The file
must be exchanged for a recipient that already imported the offer to learn the
reclaim.

## Claims and local liveness

Claims remain local liveness facts. When an imported change bundle brings a
live peer claim for a change that also has a distinct live local claim, Arc
reports both replica names and claim owners, then imports no events. Resolve
the conflict through the ordinary claim takeover path.

Replica imports support `--dry-run`, validate the whole bundle before writing,
and record a receipt naming the source replica and bundle digest. Imported
event provenance is preserved.
