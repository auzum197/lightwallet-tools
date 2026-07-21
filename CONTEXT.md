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
deployments are (zaino, lightwalletd, darkside). Not a generic
"backend", and never the client-side handle, which is the Indexer client.

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

The one-shot command-line consumer of the protocol layer (`lwcli`): it issues
a single RPC against one variant's endpoint and prints the response. A
diagnostic tool for humans pointing at real deployments, not a wallet.

## Network parameters

Per-deployment runtime data: activation heights, consensus branch ID, chain
name. Deployment description, not wire format. Always exposed as mutable
runtime state (never read directly off a wallet-facing constant), because
even a schedule darkside ships as a built-in default must remain
overridable via `--activation-heights` or a Declaration. Mainnet and testnet
presets track `zcash_protocol`'s published schedule automatically. A
Crosslink featurenet preset has no such upstream crate to track, so it is a
hand-maintained snapshot, updated here if Shielded Labs' schedule ever
changes. "Season" is Shielded Labs' own milestone label for their project,
not a chain-reset boundary, so a season bump alone is not a reason to expect
drift.

## Deployment

A specific network darkside stands in for, drawn from a closed named set:
Zcash mainnet, Zcash testnet, the Crosslink featurenet, and later possibly a
ZSAs testnet. A deployment fixes its Encoding, its Variant, and a default
activation schedule, so naming one cannot produce an impossible pairing. The
set grows by one entry when a new real network appears. Distinct from a custom
chain, where encoding, variant, and schedule are chosen directly rather than
pinned by a name.

## Encoding

The address-prefix and consensus-branch scheme a chain uses: main (`u1`,
`t1`), test (`utest`, `uviewtest`, `tm`), or regtest. What `SyntheticNetwork`'s
three cases carry. A property of a Deployment, and the one thing a custom
chain names directly. Not a synonym for deployment: the Crosslink featurenet
and Zcash testnet share the test encoding but are different deployments. The
word "network" was long overloaded to mean both this and the deployment, which
is what made encoding/variant pairings look like invalid networks.

## Custom chain

A chain darkside fabricates without a named Deployment: an encoding, a
schedule (from activation overrides or a Declaration), and a variant, all
chosen directly. Regtest with every upgrade at height 1 is its common case,
not a separate concept. The escape hatch where unusual encoding, variant, and
schedule combinations are deliberately allowed, in contrast to the safe closed
set of named deployments.

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

## Promotion gate

The live measurement that decides whether a transport graduates from
experiment to promised. Whether the connector works is settled offline
against mocks and is never the question the gate answers. The gate asks
whether the generic sync loop, run through the real network, completes at
an acceptable rate (the bar milestone 3.6 set against real servers). Until
a transport clears it, consumers get it as an experiment only.

## Darkside

The darkside process: an Indexer serving a synthetic chain over
one variant's lightwalletd surface, indistinguishable from a real deployment
to any client. Presents as mainnet, testnet, regtest, or a Crosslink
featurenet, honoring that network's activation schedule, so an unmodified
wallet configured for the network accepts it. Mainnet and testnet track the
real `zcash_protocol` schedule. The Crosslink featurenet preset carries a
hand-maintained schedule matching Shielded Labs' deployment (upgrades through
NU6 collapsed to height 1, later upgrades off), reported under testnet
prefixes since that is the network kind it runs as. Named after upstream
lightwalletd's darkside mode. Avoid: "emulator" (it implies faithful
reproduction, but darkside fabricates chain state and verifies nothing) and
"mock server" (the mock is `lightwallet-test-support`'s scripted fixture, a
different tier).

## Driver

The component that decides when darkside's chain advances, and that
supplies each block's timestamp. Two exist: live (wall clock, dev
environment) and scenario (barrier-driven, deterministic test). The chain
itself never reads a clock.

## Declaration

The authored file describing darkside's world: accounts, chains,
funding, sends, scenarios. Literal by design, so an external harness can
derive Ground truth by reading it.

## Declared account

An account named in a Declaration: a seed-derived key lineage, not a
wallet. Darkside retains its viewing keys only, which admits the
account to Ground truth and makes it fundable and sendable-from. A real
wallet becomes the account by importing the declared seed. Declaration is
not required to participate: any wallet can sync and transact through
darkside. Avoid: "scenario wallet", "darkside wallet".

## Transaction fabricator

Darkside's machinery for authoring transactions without a wallet:
real notes and real nullifiers (derivable from retained viewing keys),
dummy proofs and signatures (nothing verifies them). It is what `fund`
and `send` run on. Distinct from a wallet, which additionally holds spend
keys, signs, and proves.

## External wallet

Any wallet whose keys darkside does not hold. It syncs, receives, and
spends through darkside like any light client. It appears in Ground
truth only through transparent state, or through payments a Declared
account sent to it.

## Ground truth

Darkside's own account of expected wallet-visible state (balances,
notes, UTXOs), available because darkside fabricated the chain. What
test assertions compare a wallet against. Always states what a rigorous
wallet should conclude, including about corrupt or never-mined
transactions.

## Replay point

The height separating pre-built history from blocks that unfold per tick
when darkside serves live. Defaults to the declared tip (the whole
Declaration is history at startup). At zero, the entire Declaration
replays in real time.

## Boot height

The height darkside presents as its tip when it starts, standing in for
the network's present. A fresh wallet sets its birthday at or above it and
anchors its note-commitment tree there via `GetTreeState`. On mainnet and
testnet it sits past NU5 so Orchard is live from the first mined block. On
regtest it is zero. The tip advances from here as the live Driver ticks.

## Empty prehistory

Every height below darkside's first fabricated activity, served as
deterministic empty blocks computed on request rather than stored. Coinbase
outputs are transparent, so they never enter the shielded trees, leaving every
prehistory tree state empty. This lets a wallet anchor at any post-Sapling
birthday without darkside materializing the millions of real blocks a
network's true height implies. Real activation heights still hold throughout,
the only expectation an anchoring wallet carries. Sapling is the anchoring
floor because the lightwalletd wire format carries no Sprout representation,
so nothing older can reach a light client.

## Skipped span

A run of heights above the boot height that no block was ever mined at,
because a jump mined its target directly instead of climbing to it. Served the
same way Empty prehistory is, as deterministic empty blocks computed on
request. Distinct from prehistory in what the trees hold: a skipped span
carries whatever state the last mined block below it left, since an empty block
appends no commitments. It is what lets a chain booted at NU5 reach Ironwood,
1.7M heights up, without storing a block per height. Everything below the span
keeps its height and its transactions, so a jump is not a reorg. Avoid: "gap"
(too vague about whether the heights exist, and they do).

## Activation override

A per-network adjustment of when upgrades activate, defaulting to the
network's real schedule and settable to any height, `off` (never), or `on`
(the earliest height the non-decreasing order allows). Darkside's schedule
is its own claim, so this is a lie in the same family as dummy proofs and
corrupt commitments, aimed at consensus timing rather than note contents. It
is what lets mainnet address prefixes carry a compressed schedule, or Ironwood
ride a mainnet the real network has not reached.
