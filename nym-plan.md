# lightwallet-transport-nym: mixnet transport for the indexers

high-level.md's open question asked whether Nym is a connector or a research
project. The current SDK answers it: both shapes exist, and one of them is a
connector we can ship the same way `lightwallet-transport-tor` shipped. This
plan builds that connector (shape A) and parks the cleaner-but-unreleased
direct-stream path (shape B) as a follow-up.

## What Nym offers (as of nym-sdk v1.21.2, June 2026)

- `mixnet`: raw datagram payloads, best-effort, no ordering, SURBs. Published.
- `stream`: `MixnetStream`, a `AsyncRead + AsyncWrite` byte channel with framed
  sequencing. Nym's recommended path, but git-only, not yet on crates.io.
- `tcp_proxy`: the old session-managed proxy, deprecated in favour of `stream`.
- `client_pool`: warm `MixnetClient` instances.

Nym has no clearnet-exit-by-hostname the way arti does. `open_stream` dials a
Nym address, not `host:port`, so reaching a normal lightwalletd like
zec.rocks:443 goes through a network requester that egresses from the mixnet.

## Decisions

- Ship shape A: a SOCKS5 connector. The crate yields a tonic `Channel` built
  with `connect_with_connector`, where the connector opens a SOCKS5 stream to a
  running `nym-socks5-client`, which carries the request through a network
  requester out to the endpoint. Construction is the only thing that changes
  for a consumer, matching the Tor crate's `channel` / `channel_lazy` shape.
- The connector satisfies `GrpcTransport` for free: a SOCKS5 TCP stream is
  `AsyncRead + AsyncWrite`, wrapped in `TokioIo` exactly like arti's
  `DataStream`. TLS composes on top when the `Endpoint` carries a `tls_config`,
  so the handshake runs end-to-end to the real lightwalletd through the tunnel.
- Do not embed the Nym client in-process for the first cut. Point at an already
  running `nym-socks5-client` by its local SOCKS5 address. Embedding
  (`nym-sdk` + `client_pool`) is a later refinement once the perf story holds.
- No bandwidth-credential handling in this crate. If the network requires
  zk-nym credentials, that is the operator's setup, not the transport's.

## Steps

1. New crate `crates/transport-nym` (`lightwallet-transport-nym`), mirroring the
   Tor crate's layout. Depend on a maintained SOCKS5 connector (e.g.
   `tokio-socks`) rather than reimplementing the handshake.
2. `channel(&endpoint, socks_addr)` and `channel_lazy(...)`: same signatures as
   Tor minus the `TorClient`, plus the local SOCKS5 proxy address. Reuse the
   `authority` host/port parsing verbatim from the Tor crate.
3. Unit-test the connector's URI parsing (ports, scheme defaults) as the Tor
   crate does. No live server needed for these.
4. Offline tunnel test (`tests/tunnel.rs`): a hand-rolled in-process SOCKS5
   server bridges the connector to the test-support mock over loopback TCP.
   It asserts the CONNECT request carries the hostname as a domain address
   (so no DNS resolves locally), the generic sync loop completes through the
   tunnel, `channel_lazy` opens nothing before first use, and a proxy refusal
   surfaces as an error. Runs under plain `just test`, so proving the
   connector works never waits on an operator.
5. `tests/live.rs`, gated like the Tor and core live suites: run the generic
   sync loop through a real `nym-socks5-client` against a real lightwalletd,
   reading the proxy address and endpoint from env, skipping when unset.
6. Add a `just` recipe alongside `live-check` for the Nym live run, and a line
   in high-level.md's status table once step 5 passes.

## The gate before promoting it

Latency and throughput, not correctness, decide whether this is usable. The
mixnet is 5-hop Sphinx with cover traffic and reordering, so latency runs to
seconds. Block sync streams thousands of `CompactBlock`s. Before this crate
graduates from research to a promised transport, the live suite must show the
sync loop completing at an acceptable rate over the mixnet, the same bar §3.6
set against real servers. If it does not clear that bar, the crate stays as an
experiment for the `SendTransaction` path alone, where the metadata win is
sharpest and the payload is one message rather than a stream.

## Shape B, deferred

Once the `stream` module lands on crates.io and a Nym-native lightwalletd
exists to dial, `MixnetStream` plugs into the same connector seam with almost
the same code and drops the SOCKS5 layer entirely. Until both hold, it stays a
research tail, not a milestone.
