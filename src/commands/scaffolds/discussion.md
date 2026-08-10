> Advisory discussion. Argue in positions; resolve by `arc journal consume`.
> The ledger records what happened and what is allowed; this file records
> what people think.

## How to append a position

- Add a position with `arc journal position <this-file> --body-file -`: it
  writes the `### Position pos-<ulid> (<model[#effort]> via <harness>, <utc-ts>)` heading
  for you (timestamp tool-computed, never hand-authored) and emits the typed
  `position` event. Pass your argument as the body, below the heading.
- First line of the body states your stance: `Position: for | against | amend`.
  It is what the `arc journal discussion` tally counts; a block that states no
  stance is reported as unstated rather than silently uncounted.
- Answer a specific claim with `--ref <position id>` (the machine-readable
  half) and a quoting line directly under the stance:
  `> replying to <position id>: <the line you answer>`.
- Append-only: do not rewrite others' positions; add your own.

## How it resolves

- `arc journal consume <file> --outcome done --decision <decision-file>` —
  decided, no code. The terminal `decision` artifact is the citable verdict;
  this file stays the argument record.
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
