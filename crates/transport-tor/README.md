# lightwallet-transport-tor

Tor transport for the lightwallet indexers: an arti-backed connector that
yields a plain tonic `Channel`, so swapping a direct connection for Tor
changes construction and nothing else.

```rust
let tor = Arc::new(TorClient::create_bootstrapped(TorClientConfig::default()).await?);
let endpoint = Endpoint::from_static("https://zec.rocks:443")
    .tls_config(ClientTlsConfig::new().with_webpki_roots())?;
let indexer = CanonicalIndexerClient::new(channel(&endpoint, &tor).await?, params);
```

Name endpoints by hostname: arti resolves them at the Tor exit, so no DNS
leaves the client. TLS composes on top when the `Endpoint` carries a
`tls_config`, with tonic running the handshake over the Tor stream.

Every `channel` / `channel_lazy` call mints a fresh arti isolation token, so
two channels never share a circuit: each channel is its own unlinkability
domain (docs/adr/0001), which is exactly what the identity clients in
`lightwallet-core` need. Reconnects after a dropped connection reuse the
channel's token, landing in the same domain. To group several channels into
one domain deliberately, mint an `IsolationToken` and use the
`*_with_isolation` constructors. The lazy forms build no circuit until the
first RPC fires.

`just live-check` at the workspace root exercises this transport against a
real endpoint (bootstraps arti, so it is a nightly job, not a per-commit
one).
