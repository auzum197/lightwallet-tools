# lwcli interprets payloads, not just renders them

`lwcli` was specified as a faithful proto renderer: each RPC prints its response
as the wire carried it, `bytes` as hex, no interpretation (cli-plan.md). We are
changing that. `lwcli` becomes an inspector that can also read opaque payloads,
parsing a transaction's bytes into its txid, pools, and values, so a human
pointing it at any transaction-bearing RPC sees meaning rather than a hex blob.
Output is one parsed projection per RPC, rendered three ways: `--output human`
(readable text, the new default), `--output json` (the same projection as
JSON/NDJSON, for programs), and `--output debug` (the faithful proto render,
`{:#?}`, the wire-truth escape hatch). Human and json share the projection so
they cannot drift; debug bypasses it. This pulls `zcash_primitives` into
`lwcli`.

This is a deliberate split from the Live monitor (`lwtui`), not a duplication.
Both parse transactions; they differ in stance. `lwcli` is the low-opinion
per-RPC inspector, covering the whole surface one call at a time. `lwtui` is the
opinionated live dashboard, curating one view of the mempool (censored values,
pool naming, block flash, clear-and-repopulate). Same parsing underneath, two
use cases.

## Considered Options

- **Keep `lwcli` faithful-only, all parsing in `lwtui`.** The prior design. It
  keeps `lwcli` light and its output a pure mirror of the wire. Rejected because
  payload interpretation is useful per-RPC, not only inside a dashboard:
  `get-transaction` and the mempool RPCs return the same opaque `RawTransaction`
  a human wants cracked open, and forcing every such need through an opinionated
  full-screen view is the wrong tool.
- **Only `human` parses; `json` stays faithful proto.** Rejected: it makes
  `--output json` mean "raw proto" for the whole surface and "parsed" nowhere,
  so a program wanting the parsed fields would have to scrape human text. The
  projection must feed both renderings.
- **A separate binary for the interpreting inspector.** Rejected: interpreting
  is a stance `lwcli` takes, not a different tool. Splitting inspector-raw from
  inspector-parsed into two binaries is a mode masquerading as a boundary.

## Consequences

`zcash_primitives` (orchard/halo2) enters `lwcli`'s build, roughly tripling it,
the cost `lwtui` already pays for the same reason (ADR 0003). The faithful
proto render is not lost: it moves to `--output debug`, which stays the
wire-truth diagnostic.

The output contract changes, which is breaking. The default was `--output json`
rendering faithful proto JSON; it becomes `--output human` rendering the parsed
projection, and `--output json` now means the parsed projection as JSON, not the
raw proto. A caller that parsed `lwcli`'s old JSON must move to `--output debug`
for the faithful form, or adopt the new parsed schema.

Every transaction-bearing RPC (`get-transaction`, `send-transaction`'s echoed
tx, `get-mempool-tx`, `get-mempool-stream`) now needs a defined parsed
projection: what its simplified form actually is. RPCs whose responses are
already structured (`get-block`, `get-lightd-info`, the balance and utxo
queries) get near-faithful human forms, since there is no opaque payload to
crack. Defining those projections is the work this direction signs up for; it is
the same `Row`-style modeling done for the mempool, extended across the surface.
