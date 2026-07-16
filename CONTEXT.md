# Ubiquitous language

## Variant

A distinct lightwalletd protocol surface requiring its own generated bindings.
Two exist: **Canonical** and **Crosslink**. A network upgrade (e.g. Ironwood)
is not a variant, it lands inside Canonical and is carried as deployment
parameters, not new bindings.

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

A handle to one variant's endpoint: the generated client plus the network
parameters for the deployment it points at. Generic over transport. Named for
what lightwalletd-style servers are (chain indexers, e.g. zaino), not a generic
"backend". The trait is `TestnetIndexer`; the two implementations are
`CanonicalIndexer` and `CrosslinkIndexer`.

## Capability trait

A narrow trait asserting that a variant's generated type supports one specific
operation (e.g. header continuity). Variants that lack the capability simply
don't implement the trait. There is no shared normalized block type.

## Debug client

The one-shot command-line consumer of the protocol layer (`lwcli`): it issues
a single RPC against one variant's endpoint and prints the response. A
diagnostic tool for humans pointing at real deployments, not a wallet.

## Network parameters

Per-deployment runtime data: activation heights, consensus branch ID, chain
name. Deployment description, not wire format. Never compile-time constants,
because Crosslink featurenets reset each season.

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
the partition: each address, broadcast, or confirmation poll it wants to look
like a stranger gets a domain of its own. The layer's job is to make domains
explicit and cheap to mint, never to choose the partition.

## Identity-bearing RPC

An RPC whose request content names a wallet-specific identifier, e.g.
`SendTransaction` (the raw transaction), `GetTransaction` (a txid), the
transparent-address and utxo queries (addresses), mempool queries carrying
exclude lists. These are structurally excluded from the sync surface: the
indexer type cannot issue them, only an identity client can. Timing or
access-pattern fingerprints do not make an RPC identity-bearing, content
does, otherwise block sync itself would qualify.

## Identity client

The per-variant handle that realizes one unlinkability domain and carries the
identity-bearing RPCs (`CanonicalIdentityClient`, `CrosslinkIdentityClient`).
One instance per identity the wallet wants kept apart: each transparent
address, each broadcast, each confirmation poll that should look like a
stranger. Cheap to mint, so the wallet's partition of its own activity is
expressed by how many it constructs. The unlinkability it delivers is only as
strong as the transport underneath: structural on all transports, meaningful
on privacy transports.

## Promotion gate

The live measurement that decides whether a transport graduates from
experiment to promised. Whether the connector works is settled offline
against mocks and is never the question the gate answers. The gate asks
whether the generic sync loop, run through the real network, completes at
an acceptable rate (the bar milestone 3.6 set against real servers). Until
a transport clears it, consumers get it as an experiment only.
