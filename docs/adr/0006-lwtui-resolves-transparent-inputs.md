# lwtui resolves transparent input values, reversing ADR 0003

**Status:** accepted (supersedes the fee omission of ADR 0003)

ADR 0003 kept the Live monitor identity-free and omitted any value that needs a
prevout lookup, since a transparent input's amount lives off-chain in the output
it spends, not in the spending tx. lwtui now follows those references: each
transparent input's `OutPoint` is resolved to its funding output and its value
read, so a row shows the transparent input total and an exact fee instead of
omitting it. This trades the monitor's identity-free property for value
completeness. Resolution issues `GetTransaction` per on-chain funding txid, an
identity-bearing RPC (ADR 0001), so lwtui gains an Identity client and names
funding txids to the indexer. It is eager (every transparent-input tx as it
streams) and always on.

Two shortcuts keep the block-boundary burst survivable. A funding-txid cache
holds each fetched funder's output values. Confirmed outputs are immutable, so
it never invalidates and a refill mostly hits warm entries. An in-stream index
resolves an input funded by an unconfirmed parent already in the pending set
with no RPC and no identity exposure at all. A bounded worker pool caps
concurrent lookups so the burst queues rather than opening a socket per input.

## Considered Options

- **Lazy resolution (focused row only).** Resolve just the transaction a human
  inspects, bounding both cost and identity exposure to deliberate attention.
  Rejected here in favor of eager: every row carries its fee the moment it
  arrives. The cache and the concurrency bound are what make eager affordable.
- **Stay identity-free, resolve in-stream parents only.** No `GetTransaction`,
  so the monitor keeps ADR 0003's guarantee. Rejected: most inputs fund from
  confirmed chain, not the mempool, so this leaves the input side blank for
  nearly every real transaction.

## Consequences

lwtui is no longer a chain-wide, identity-free viewer: launched plain, it
reveals which funding txids it inspects. A funding lookup that fails renders the
input total and fee as unresolvable, never a wrong number, extending ADR 0003's
"never a silent drop" from rows to values. Over a serializing transport
(Tor/Nym) a refill's lookups queue behind the worker-pool bound. The
child-arrives-before-parent case within a cycle falls through to a
`GetTransaction` rather than waiting for the parent to stream in. A full
park-and-redispatch scheduler is left for later.
