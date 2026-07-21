# lightwallet-core

Clients for Zcash lightwalletd-style indexers, generic over protocol
variant (CANONICAL, CROSSLINK) and transport.

The wire surface splits three ways. `IndexerClient` is the block-sync path
(latest height, blocks, ranges, tree state), the one place consumers are
genuinely variant-agnostic, made generic by the `CompactBlockHeader`
capability and associated types rather than a shared block struct. The rest
of the chain-wide surface (mempool stream, subtree roots, pool-filtered
ranges, server info) is identical inherent methods on
`CanonicalIndexerClient` and `CrosslinkIndexerClient`, returning each variant's
own generated types, with Crosslink-only RPCs (roster, bond info) concrete
on `CrosslinkIndexerClient`. Identity-bearing RPCs (transactions,
transparent addresses, utxos) live on
`CanonicalIdentityClient` / `CrosslinkIdentityClient`: the types keep those
RPCs off the sync clients, and each identity client is built from a
non-`Clone` `IdentityTransport` token, so the sync channel cannot ride one
and one transport cannot back two identities (docs/adr/0001). A wallet mints
one token per identity the server should see as a distinct peer. The privacy
transports isolate every channel they mint, so the token wraps a fresh domain.

```rust
use lightwallet_core::tonic::transport::{ClientTlsConfig, Endpoint};
use lightwallet_core::{CanonicalIndexerClient, IndexerClient, NetworkParams};

let channel = Endpoint::from_static("https://zec.rocks:443")
    .tls_config(ClientTlsConfig::new().with_webpki_roots())?
    .connect()
    .await?;
let client = CanonicalIndexerClient::new(channel, params);
let tip = client.get_latest_height().await?;
```

tonic and the generated proto crates are re-exported
(`lightwallet_core::tonic`, `lightwallet_core::proto::{canonical, crosslink}`),
so besides an async runtime and `futures-util` to drive the streams this is
the only dependency a consumer needs.
Anything satisfying `GrpcTransport` (any tonic `Channel`, including the ones
from `lightwallet-transport-tor` and `lightwallet-transport-nym`) plugs into
the same constructors. Errors are `Error` with `code()`/`retryable()`,
streams are `BoxStream<'static, Result<T>>` with individually fallible
items, and retry/timeout/backoff policy is left to consumer-side tower
layers on purpose.

Features: `canonical` (default) and `crosslink`, additive, each pulling in
its generated proto crate. With neither, the traits, `NetworkParams`, and
`Error` remain. `tls` (default) forwards tonic's rustls and webpki-roots
features for direct `https` endpoints as in the example; drop it with
`default-features = false` if you bring your own tonic TLS.
