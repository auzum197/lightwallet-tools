# Ubiquitous language

## Variant

A distinct lightwalletd protocol surface requiring its own generated bindings.
Two exist: **Canonical** and **Crosslink**. A network upgrade (e.g. Ironwood)
is not a variant, it lands inside Canonical and is carried as deployment
parameters, not new bindings. A variant is a property of a Deployment, not a
free axis: the Crosslink variant has one real home, the Crosslink featurenet.
It is also transitional. When Crosslink upstreams into Zcash it becomes a
network upgrade carried inside Canonical (the Ironwood path), its added RPCs
join Canonical's proto, and the separate variant retires.

## Canonical

The variant defined by `zcash/lightwallet-protocol`, the upstream source of
truth for the lightwalletd gRPC surface. Formerly called MAIN in early drafts,
renamed because "main" read as mainnet (wrong: the variant serves any network)
or a git branch.

## Crosslink

The variant served by Shielded Labs' Crosslink featurenets. Its surface is
Canonical plus additive RPCs (currently roster, bond info, faucet). Expected
to grow a finality-reporting surface.

## Overlay

A hand-written proto file mirroring the Canonical service plus the Crosslink
additions. Exists because Crosslink has no separable proto source of its own,
only a fork with in-place edits.

## Reference snapshot

A dated, provenance-annotated copy of Crosslink's upstream `service.proto`,
committed so drift between the overlay and Crosslink's actual surface can be
diffed on demand.

## Indexer

The indexing server a light client syncs from: what lightwalletd-style
deployments are (zaino, lightwalletd). Not a generic "backend", and never the
client-side handle, which is the Indexer client.

## Indexer client

The per-variant handle to an Indexer: the generated client plus the network
parameters for the deployment it points at, generic over transport. In code
`CanonicalIndexerClient` and `CrosslinkIndexerClient`, with trait
`IndexerClient`. Carries only the block-sync path. Identity-bearing RPCs
live on the Identity client.

## Light client

The consuming application: a wallet that holds keys and syncs compact
blocks through an Indexer. Built on Indexer clients and Identity clients,
and not a synonym for either.

## Capability trait

A narrow trait asserting that a variant's generated type supports one specific
operation (e.g. header continuity). Variants that lack the capability simply
don't implement the trait. There is no shared normalized block type.

## Debug client

The command-line inspector of the protocol layer (`lwcli`): aim it at a single
RPC on one variant's endpoint and it shows what came back. Two stances, chosen
per invocation: faithful, rendering the proto response as the wire carried it,
or interpreted, cracking opaque payloads (a transaction's bytes into its txid,
pools, and values) into a human reading. Low-opinion and per-RPC, covering the
whole surface one call at a time, unlike the Live monitor's opinionated
curation. A diagnostic for humans pointing at real deployments, not a wallet.

## Live monitor

The interactive terminal consumer of the protocol layer (`lwtui`, crate
`lightwallet-tui`): it holds a connection open and renders a variant's mempool
as a live list that repopulates at each block boundary. Sibling to the Debug
client, sharing the same `lightwallet-core` calls, one-shot print replaced by
an event loop. A diagnostic viewer for humans, not a wallet. Starts
mempool-only and grows other live views later.

## Pending set

The mempool contents relative to the current chain tip: the transactions the
Live monitor shows. Not a running history. When a block arrives the stream
ends, transactions mined into it leave the set, and the view rebuilds from the
new tip. "Clear and repopulate" is this boundary made visible.

## Prevout resolution

Following a transparent input's outpoint (funding txid plus output index) to the
output it spends, to read the input's value. A transaction's bytes carry the
input's reference but not its amount, so the amount is recoverable only from the
funding output. A funder still in the Pending set resolves locally. A confirmed
funder needs a `GetTransaction` lookup, which is identity-bearing. A funder in a
shielded pool has no readable value without keys, so resolution covers
transparent inputs only. What turns the Live monitor's fee from omitted (the
ADR 0003 stance) into exact.

## Network parameters

Per-deployment runtime data: activation heights, consensus branch ID, chain
name. Deployment description, not wire format. Always exposed as runtime state
rather than read off a wallet-facing constant, so a client can point at a
network whose schedule differs from any built-in default (via
`--activation-heights`). Mainnet and testnet presets track `zcash_protocol`'s
published schedule automatically. A Crosslink featurenet preset has no such
upstream crate to track, so it is a hand-maintained snapshot, updated here if
Shielded Labs' schedule ever changes. "Season" is Shielded Labs' own milestone
label for their project, not a chain-reset boundary, so a season bump alone is
not a reason to expect drift.

## Deployment

A specific network a client points at, drawn from a closed named set: Zcash
mainnet, Zcash testnet, the Crosslink featurenet, and later possibly a ZSAs
testnet. A deployment fixes its Encoding, its Variant, and a default
activation schedule, so naming one cannot produce an impossible pairing. The
set grows by one entry when a new real network appears.

## Encoding

The address-prefix and consensus-branch scheme a chain uses: main (`u1`,
`t1`), test (`utest`, `uviewtest`, `tm`), or regtest. A property of a
Deployment. Not a synonym for deployment: the Crosslink featurenet and Zcash
testnet share the test encoding but are different deployments. The word
"network" was long overloaded to mean both this and the deployment, which is
what made encoding/variant pairings look like invalid networks.

## Transport

The route a connection to an indexer's endpoint takes: direct, or tunneled
through a privacy network (Tor, Nym). A construction-time choice. The protocol
surface is identical over every transport, so nothing downstream of
construction knows which one is in use.

## Unlinkability domain

A partition of a consumer's network activity. Connections inside one domain
may be correlated by a network observer without harm, because they already
belong to one identity. Connections in different domains must never be
linkable, so they must not share transport-level identifiers such as a Tor
circuit. The sync stream is one domain per wallet. Every RPC whose request
content names a wallet-specific identifier (a txid, a transparent address, a
held-transaction list) belongs to some other domain, and the wallet decides
the partition: each address, broadcast, or confirmation poll it wants kept
unlinkable from the rest gets a domain of its own. The layer's job is to make
domains explicit and inexpensive to mint, never to choose the partition.

## Identity-bearing RPC

An RPC whose request content names a wallet-specific identifier, e.g.
`SendTransaction` (the raw transaction), `GetTransaction` (a txid), the
transparent-address and utxo queries (addresses), mempool queries carrying
exclude lists. These are structurally excluded from the sync surface: an
Indexer client cannot issue them, only an Identity client can. Timing or
access-pattern fingerprints do not make an RPC identity-bearing, content
does, otherwise block sync itself would qualify.

## Identity client

The per-variant handle that realizes one unlinkability domain and carries the
identity-bearing RPCs (`CanonicalIdentityClient`, `CrosslinkIdentityClient`).
One instance per identity the wallet wants kept apart: each transparent
address, each broadcast, each confirmation poll that should remain
unlinkable from the rest. Inexpensive to mint, so the wallet's partition of
its own activity is expressed by how many it constructs. The unlinkability
it delivers is only as strong as the transport underneath: structural on all
transports, meaningful on privacy transports.

## Chain health

The Live monitor's one-word reading of chain block-production as seen through
the Indexer, assuming a live connection: `healthy` (tip advancing, interblock
spacing near target), `syncing` (node behind its own estimate, catching up), or
`stalled` (no block for well over target, node believes it is at tip). A
statement about the chain, not the connection: an unreachable Indexer is a
separate indicator, not a fourth state. `quiet` was the early word for the
`healthy` case, dropped because quiet describes the chain and healthy describes
the reading.

## Promotion gate

The live measurement that decides whether a transport graduates from
experiment to promised. Whether the connector works is settled offline
against mocks and is never the question the gate answers. The gate asks
whether the generic sync loop, run through the real network, completes at
an acceptable rate (the bar milestone 3.6 set against real servers). Until
a transport clears it, consumers get it as an experiment only.

## Staking action

A Crosslink v7 (VCrosslink) transaction's optional participation in the PoW
chain's delegation-staking surface, one of seven kinds
(create/withdraw/retarget a delegation bond, begin unbonding, register a
finalizer, convert a finalizer reward, update a finalizer key). Public on the
wire: it rides in the full transaction bytes and needs no keys to read, so the
diagnostic viewers decode and display it. Observable only in the full-tx
drill-down, never in compact streams. The Live monitor and Debug client render
it through the shared txview parser; neither builds nor signs one. Bond and
finalizer state lookups (roster, bond info) stay opaque bytes, out of scope.
