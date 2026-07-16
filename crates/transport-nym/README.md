# lightwallet-transport-nym

Nym mixnet transport for the lightwallet indexers: a SOCKS5 connector to a
running `nym-socks5-client` that yields a plain tonic `Channel`, the same
`channel` / `channel_lazy` constructors as the Tor transport.

```rust
let socks: SocketAddr = "127.0.0.1:1080".parse()?;
let endpoint = Endpoint::from_static("https://zec.rocks:443")
    .tls_config(ClientTlsConfig::new().with_webpki_roots())?;
let indexer = CanonicalIndexer::new(channel(&endpoint, socks).await?, params);
```

The proxy carries the stream through the mixnet to a network requester,
which egresses to the endpoint. Hostnames go into the SOCKS5 request as
domain addresses and resolve at the requester, so no DNS leaves the client
(pinned by the offline suite against an in-process SOCKS5 mock,
`tests/tunnel.rs`). TLS runs end-to-end to the real lightwalletd when the
`Endpoint` carries a `tls_config`.

This crate does not embed a Nym client or handle bandwidth credentials.
Running and funding the `nym-socks5-client` is the operator's setup.

Status: experimental until mixnet sync throughput clears the promotion gate
(`just live-check-nym`, criteria in `nym-plan.md` at the workspace root).
