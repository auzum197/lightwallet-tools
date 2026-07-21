# darkside

The `darkside` binary: `darkside serve` runs a live synthetic chain over gRPC.

It stands in for one deployment with
`--deployment zcash-mainnet|zcash-testnet|crosslink-featurenet|custom`
(default `zcash-mainnet`). Each named deployment pins its encoding, its
served variant, and a default schedule: mainnet and testnet carry the real
schedule and prefixes and boot at NU5, and the crosslink featurenet carries
Shielded Labs' schedule (everything through NU6 at height 1, NU6.1 onward
off) under testnet prefixes and boots at 1. `custom` opens `--encoding
main|test|regtest` (default regtest, everything at height 1) and `--variant
canonical|crosslink` (default canonical). Passing either to a named
deployment is an error, since a named deployment pins them.

`--start-height` moves the boot height and `--activation-heights "all=1,
nu6.3=on"` rewrites the schedule over whatever base the deployment sets, both
honored for every deployment. `--seed` fixes the deterministic fabrication.

Live mode mines forward on a wall clock. `--tick` sets the cadence in
seconds, either a fixed `N` or a range `LOW..HIGH` drawn afresh per block.
Timestamps are always the real clock. `--instamine` mines on submission,
`--withhold` accepts wallet submissions but never mines them.

The server binds `--listen` (default `127.0.0.1:9067`) and runs until you
stop it. It serves fabricated chain state and verifies nothing, so
`GetLightdInfo` reports a vendor and chain name that unmistakably mark it as
darkside. Binding a non-loopback address exposes that fabricated state to
the network.

## Control

`darkside serve` also binds an HTTP control surface, `127.0.0.1:9068` by
default and moved with `--control-listen`. It is loopback only and refuses to
bind anything else, because it fabricates value and rewrites history with
nothing to authenticate against yet. Every route takes and returns JSON, so
curl drives it with no client to install.

```
POST /fund     {address, zec, receivers?}
POST /mine     {blocks}
POST /to       {height}
POST /advance  {upgrade}
POST /reorg    {depth}
POST /tick     {tick}
POST /withhold {on}
POST /next | /pause | /resume | /reset
GET  /status
```

`fund` fabricates value to any address valid for the served network, declared
or not, and mines the block carrying it. `receivers` is a string of letters
picking which receivers of the address get paid: `t` transparent, `s` sapling,
`o` orchard, `i` ironwood. Several letters split the amount into equal parts,
with the remainder going to the letter typed first. Omit it and the whole
amount goes to the newest receiver the address carries and the chain has
active.

```
curl -s localhost:9068/fund -H 'content-type: application/json' \
  -d '{"address":"u1...","zec":"12","receivers":"os"}'
```

A receiver the address does not carry, or a pool the chain has not activated,
is skipped and its share re-split over the rest, so the amount asked for is
the amount that lands. Skips come back in the response's `warnings` and go to
the log. A fund where no requested receiver survives fails instead, since a
silent no-op is worse than an error.

`--max-blocks` caps how many blocks one command may mine, 10,000 by default,
so a mistyped height fails rather than holding the connection open.

The cap is also where `/advance` switches strategy. A named upgrade further
away than the cap is reached by mining its activation block alone and leaving
the heights beneath it unmined, computed on request the way prehistory is
(ADR 0002). That turns mainnet's 1.7M-block climb to ironwood from about an
hour and several gigabytes into a moment. Everything already mined keeps its
height and its transactions, so this is not a reorg, and the response's
`detail` carries the height it jumped from. A jump is refused only when work is
scheduled inside the range it would skip.

Each RPC logs to stderr on arrival and again on return, through `tracing`,
in three tiers `RUST_LOG` tunes. Info, the default, is a glance: method and
key parameters on the way in, method and result identifiers on the way out
(`GetSubtreeRoots -> roots=12 hashes=[338169a9…, …]`), always capped and
collapsed, with failures surfaced here too
(`SendTransaction -> rejected error=…`). `RUST_LOG=darkside=debug` adds every
identifier, uncapped, still no payloads. `RUST_LOG=darkside=trace` dumps the
full decoded request and response structs. `RUST_LOG=warn` quiets the
per-request lines.

Block ranges are the exception at every tier, reporting a count and a height
span and nothing else (`GetBlockRange -> blocks=1741039
heights=1687105..3428143`). Their blocks are built as the client reads them,
so at return time none exist to name, and a range that crosses a skipped span
is millions long. The chain lock is taken a chunk at a time while such a range
streams, which is what lets the miner keep mining and `/fund` keep answering
through a long sync.

The declaration language and scenario runner are library-only, in
`darkside-decl` and `darkside-serve::run_scenario`.
