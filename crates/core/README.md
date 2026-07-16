# lightwallet-core

Client layer for Zcash lightwalletd-style indexers, generic over protocol
variant (CANONICAL, CROSSLINK) and transport.

The wire surface splits three ways. `TestnetIndexer` is the block-sync path
(latest height, blocks, ranges, tree state), the one place consumers are
genuinely variant-agnostic, made generic by the `CompactBlockHeader`
capability and associated types rather than a shared block struct. The rest
of the chain-wide surface (mempool stream, subtree roots, server info) is
identical inherent methods on `CanonicalIndexer` and `CrosslinkIndexer`,
returning each variant's own generated types, with Crosslink-only RPCs
(roster, bond info) concrete on `CrosslinkIndexer`. Identity-bearing RPCs
(transactions, transparent addresses, utxos) live on
`CanonicalIdentityClient` / `CrosslinkIdentityClient`, each over a transport
of its own so they cannot ride the sync channel. One client per identity the
server should see as a stranger (docs/adr/0001).

```rust
let channel = Endpoint::from_static("https://zec.rocks:443").connect().await?;
let indexer = CanonicalIndexer::new(channel, params);
let tip = indexer.get_latest_height().await?;
```

Anything satisfying `GrpcTransport` (any tonic `Channel`, including the ones
from `lightwallet-transport-tor` and `lightwallet-transport-nym`) plugs into
the same constructors. Errors are `Error` with `code()`/`retryable()`,
streams are `BoxStream<'static, Result<T>>` with individually fallible
items, and retry/timeout/backoff policy is left to consumer-side tower
layers on purpose.

Features: `canonical` (default) and `crosslink`, additive, each pulling in
its generated proto crate. With neither, the traits, `NetworkParams`, and
`Error` remain.
