# Synthetic chains serve real networks over an empty prehistory

Darkside needs to present as mainnet or testnet to an unmodified wallet,
whose activation heights are hardcoded (Orchard at mainnet 1687104). Honoring
that schedule literally would mean mining past 1.6M blocks before a shielded
note can exist, which is infeasible for an interactive process. We resolved it
by separating the chain's identity from its materialized history. The chain is
conceptually genesis-at-0, but blocks below the first fabricated activity are
empty blocks computed on demand, with `hash(lineage, height, seed)` a pure
function so `prev_hash(h) = hash(h-1)` holds without recursion. Coinbase
outputs are transparent, so the shielded trees remain empty across the whole
prehistory, and `GetTreeState` at any post-Sapling height returns an empty
anchor. A new wallet sets its birthday at a chosen boot height, fetches that
empty anchor, and believes no notes predate it, which for its own keys is
true. Every fabricated note darkside mines above the boot height witnesses
against the same empty anchor, so wallet and darkside never disagree. The only
promise darkside keeps is where activations land.

Sapling is the floor because the protocol darkside serves cannot express
anything older. A compact transaction carries transparent inputs, Sapling
spends and outputs, and Orchard actions, and `TreeState` carries the Sapling
and Orchard trees. Sprout appears nowhere in the wire format, so a Sprout note
is invisible to every light client and a pre-Sapling birthday has no tree to
anchor into. This is stronger than deprecation: a server backed by a full node
holding complete Sprout state still has no field to put a JoinSplit in. The
heights below Sapling activation still exist and serve as empty prehistory
blocks. The floor excludes shielded activity and wallet anchoring there, not
the heights themselves.

The same reasoning applies above the boot height. A chain booted at NU5 that
wants to reach Ironwood is 1.7M heights away, and mining them costs roughly an
hour and several gigabytes for blocks that carry nothing. So materialized
blocks ascend by height without having to be consecutive: `jump_to` mines one
block at an arbitrary height above the tip and leaves the heights beneath it
unmined, computed by the same `empty_block` path prehistory uses. Height to
index is a search over `Block.height` rather than an offset from the boot
height. Continuity needs no special case, because `hash_at` already prefers a
materialized hash and falls back to the computed one, so both seams chain.
Tree state resolves to the frontier at the nearest mined height at or below the
query, which is correct because an empty block appends no commitments. A jump
is refused when scheduled work sits inside the span it would skip, since those
events would never fire.

This is what separates a jump from a reorg. Nothing below the span moves:
blocks keep their heights and their transactions, and a wallet already synced
sees new empty blocks rather than a rewrite.

Networks are a `SyntheticNetwork` enum (`Main(Table)`, `Test(Table)`,
`Regtest(LocalNetwork)`) that implements `zcash_protocol`'s `Parameters` by
delegation. Main and test default to the real schedule but carry a mutable
activation table, so a height can be shifted, turned `off`, or set `on` (the
earliest height the non-decreasing order allows). That override is a lie in the
same family as dummy proofs and corrupt commitments, aimed at consensus timing.
It is what lets mainnet prefixes carry a compressed schedule, or Ironwood
(NU6.3, unreached on the real mainnet) ride a mainnet chain.

## Considered Options

- **Honor the real schedule with a chain mined from 0.** Rejected: reaching
  Orchard means materializing over 1.6M blocks. No wallet or darkside wants to
  sync that.
- **Genesis pinned at a high start height, nothing below it.** Simpler storage,
  but a wallet with any birthday below the start height gets an empty range
  instead of the deterministic prehistory. We wanted any post-Sapling birthday
  to sync, so the prehistory has to be reachable.
- **Delegate main/test to `zcash_protocol::consensus::Network` directly.** Its
  activation heights are fixed constants, so a darkside built on it cannot
  shift a height or switch Ironwood on for mainnet. The per-variant table
  exists to make that lie representable.
- **Reach a distant upgrade by rebuilding the chain booted at it.** Cheaper to
  write than skipped heights, and it was the first attempt. It discards every
  materialized block, so funding a wallet and then crossing into Ironwood
  destroys the funding. Rejected once that turned out to be the ordinary way
  to use it.
- **Cap how far one command may mine and leave it there.** Correct as a guard
  against a mistyped height, and it is still in place, but on its own it only
  reports that the distance is too far. The distance is real, so something has
  to cross it.
- **Fork by replaying blocks 0..fork_point.** The prior regtest approach. At a
  mainnet boot height it replays over a million blocks per fork. Replaced by
  lineage segments `[(fork_point, fork_id)]`, where `hash` picks the owning
  segment's id, prehistory is shared by construction, and forking costs
  O(materialized blocks).

## Consequences

darkside-chain moves off the contiguous mined Vec to a materialized-block map
plus a tip pointer plus lineage segments. The regtest path remains
behavior-equivalent (start height 0, dense activity, everything materialized),
which the existing chain, decl, and scenario-reorg tests hold to. With real
main/test parameters Ironwood is reachable only through an activation override,
so three-pool testing on default parameters is a regtest property. The boot
height defaults to the effective NU5 activation after overrides, so flattening
the schedule with `all=1` drops the default boot height to 1 as well.

Because materialized heights can skip, nothing may derive an index from
`height - start_height` again. `tip_height` reads the last block's own height,
`block_index` searches, and `fork_at` counts the blocks at or below its fork
point. `tree_states` gained an explicit upper bound at the tip, which the
frontier lookup used to supply by finding nothing.

The declaration language is unwired from the CLI but kept as a library. The
flag-driven live server now carries an HTTP control surface for funding,
mining, jumping, and reorging, which the declaration language and the scenario
runner remain independent of.
