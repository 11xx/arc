> Advisory discussion. Argue in positions; resolve by `arc journal consume`.
> The ledger records what happened and what is allowed; this file records
> what people think.

## How to append a position

- One `### Position (<model[#effort]> via <harness>, <utc-ts>)` heading per
  position, appended at the end. Take the timestamp from `arc journal
  stamp`; never author a date by hand.
- First line states your stance: `Position: for | against | amend`.
- Answer a specific claim with a quoting first line:
  `> replying to <position timestamp>: <the line you answer>`.
- Append-only: do not rewrite others' positions; add your own.

## How it resolves

- `arc journal consume <file> --outcome done` — decided, no code. Write a
  companion `conclusion` artifact in the same breath and point at it with
  `--note`; this file stays the argument record, the conclusion is the
  citable verdict.
- `--outcome superseded` — the decision becomes work. Open the change with
  `arc begin --from-journal <file>` (it consumes this file as superseded
  itself).
- `--outcome discarded` — rejected; say why in `--note`.
- Shelving needs no outcome: leave open (warm) or `arc journal archive`
  (cold). Re-litigation needs no reopen verb: open a successor discussion
  whose first line is `> supersedes <file>: <what changed>`.
- `consume` is unilateral and advisory. Norm: a contested discussion is
  resolved by an agent that did not author the winning position, or by the
  user.

## Positions
