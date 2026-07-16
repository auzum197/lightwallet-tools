# Zcash Multi-Variant Lightwallet Protocol Layer

Scope: proto sourcing → codegen → `lightwallet-core`. Everything downstream (darkside
emulator, debug client, TUI, scenario DSL) consumes this layer and is out of scope here.

## Layout

```
proto/canonical/      pristine git subtree of zcash/lightwallet-protocol at a pinned tag
proto/overlay/        hand-written crosslink.proto: full mirror of canonical service.proto
                      plus the CROSSLINK additions (roster, bond info, faucet), with a dated
                      reference snapshot under reference/. A mirror because proto3 cannot
                      extend services and Crosslink has no separable proto source, only a
                      fork with in-place edits.
crates/canonical      lightwallet-proto-canonical: codegen only, protox + tonic-prost-build
crates/crosslink      lightwallet-proto-crosslink: codegen only
crates/core           lightwallet-core: capability traits + CanonicalIndexer/CrosslinkIndexer
crates/test-support   lightwallet-test-support: in-memory mock harness, dev-dependency only
crates/transport-tor  lightwallet-transport-tor: arti-backed connector, still yields Channel
crates/transport-nym  lightwallet-transport-nym: SOCKS5 connector through a running
                      nym-socks5-client, still yields Channel (see nym-plan.md)
```

`just` lists the check and maintenance recipes (mirror/coverage checks, `test`, `live-check`,
`upstream-check`, `canonical-pull`).

## Load-bearing decisions

- Two variants, CANONICAL and CROSSLINK. A network upgrade (Ironwood) is not a variant: pin
  the latest canonical tag and carry activation heights in `NetworkParams` (per-instance
  runtime data, never compile-time constants, because featurenets reset each season).
- No shared normalized block type. Each variant keeps its generated types. Genericity comes
  from narrow capability traits (`CompactBlockHeader`: height, hash, prev-hash continuity)
  plus associated types on `TestnetIndexer`, which carries only the block-sync path. Hashes
  come off the wire as `&[u8]` with a fallible `[u8; 32]` accessor, so no panic path on
  untrusted data. A variant the trait doesn't fit simply doesn't implement it.
- The rest of the shared RPC surface is inherent methods emitted identically on both indexers
  by `impl_streamer_methods!`. Variant-only RPCs (`get_bond_info`, `get_roster`,
  `request_faucet_donation`) exist only on `CrosslinkIndexer`, never as `Option` on something
  shared.
- Indexers are generic over `T: GrpcTransport`; no concrete transport in the public API.
  Errors are `lightwallet_core::Error` with `code()`/`retryable()`; tonic is a private
  dependency. Streams are `BoxStream<'static, Result<T>>`.
- Retries, timeouts, and backoff are consumer-side tower layers, never built in. Retry policy
  is per-method (`SendTransaction` retried on an ambiguous timeout can double-submit), and
  stream resumption is application logic.
- Transports are separate crates because cargo unifies features across the graph. Variants are
  strictly additive features on core.
- No CI drift check against Crosslink upstream: `just upstream-check` diffs the snapshot on
  demand, and the live suite is what catches wire-format changes.

## Status

| Milestone | Status |
|---|---|
| 1. Proto sourcing (subtree + overlay + snapshot) | done |
| 2. Codegen crates, clients and server traits, no protoc | done |
| 3. `lightwallet-core` traits + indexers | done |
| 3.5 Mock harness: generated server traits over a tokio duplex pipe, per-RPC and mid-stream fault injection (`crates/core/tests/mock_endpoint.rs`) | done |
| 3.6 Live validation: same generic sync loop against real servers (`just live-check`; zec.rocks + testnet passed 2026-07-15; CROSSLINK half reads `LIGHTWALLET_CROSSLINK_URL`, skips when unset) | done |
| 4. `lightwallet-transport-tor`: `channel(&endpoint, &tor)` / `channel_lazy`, TLS composes via the endpoint's `tls_config`, hostnames resolve at the exit (live run through zec.rocks passed 2026-07-16) | done |
| 5. `lightwallet-transport-nym`: SOCKS5 connector to a running `nym-socks5-client`, same `channel` / `channel_lazy` shape. The connector is proven offline under `just test` by an in-process SOCKS5 mock (`tests/tunnel.rs`, including the no-local-DNS assertion). Experiment until the mixnet sync rate clears the §3.6 bar (`just live-check-nym`), per nym-plan.md | built, promotion gate pending |

The mock suite proves self-consistency, not conformance: both ends share the generated types.
The live suite is the conformance check. Run it nightly, not per-commit.

## Open questions

- ~~What is the current Nym Rust SDK: stream-oriented or mixnet-datagram?~~ Answered in
  nym-plan.md: both shapes exist, and the SOCKS5 path is a shippable connector (milestone 5).
  Whether it graduates from experiment to promised transport is a latency question, decided
  by the promotion gate (`just live-check-nym`).
