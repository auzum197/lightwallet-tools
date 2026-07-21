# darkside-chain

The deterministic chain state machine at the bottom of the darkside.
Pure state: no network, no tonic, no I/O, and no clock. Block time is an input
to `mine`, supplied by whichever driver sits above (`darkside-serve`).

Chains are values. A reorg is serving a different chain that shares a prefix,
so commitment trees, nullifier sets, and the UTXO set are per-chain and
derived from transactions. There is no API to set tree state directly. The
deliberately-inconsistent variants live behind loudly named `corrupt_*`
methods.

Same seed, same chain: all randomness (note rseeds, ephemeral keys,
value-commitment trapdoors) comes from the seeded RNG the chain was
constructed with, so fabricated transactions reproduce byte-identically.

This crate depends on the zcash protocol stack only, never on the generated
proto crates. Conversion to wire types lives in `darkside-serve`.
