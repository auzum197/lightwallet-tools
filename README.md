# Zcash Lightwallet Protocol Layer

Rust client crates for lightwalletd-style Zcash indexers. Two protocol
variants sit behind one generic API: CANONICAL, the stock surface defined by
[`zcash/lightwallet-protocol`](https://github.com/zcash/lightwallet-protocol),
and CROSSLINK, the Crosslink fork's additive mirror (finalizer roster, bond
info, faucet). Tor and Nym transports plug in without changing any call
sites, and an in-memory mock harness covers the whole surface offline.

## Crates

| Crate | What it is |
|---|---|
| [`lightwallet-core`](crates/core) | Capability traits, per-variant indexers, per-domain identity clients |
| [`lightwallet-proto-canonical`](crates/canonical) | Generated bindings for the canonical protocol |
| [`lightwallet-proto-crosslink`](crates/crosslink) | Generated bindings for the Crosslink variant |
| [`lightwallet-transport-tor`](crates/transport-tor) | arti-backed tonic channels, each its own circuit-isolation domain |
| [`lightwallet-transport-nym`](crates/transport-nym) | tonic channels through a running `nym-socks5-client` (experimental) |
| [`lightwallet-test-support`](crates/test-support) | In-memory mock endpoints with fault injection, plus a SOCKS5 test server |
| [`lightwallet-cli`](crates/cli) | `lwcli`, a one-shot debug client covering the full RPC surface |

## Design in brief

There is no shared normalized block type. Each variant keeps its generated
types, and genericity comes from a narrow `CompactBlockHeader` capability
plus associated types on `IndexerClient`, which carries only the block-sync
path. The rest of the shared RPC surface is emitted as identical inherent
methods on both indexers, and variant-only RPCs exist only on
`CrosslinkIndexerClient`, never as an `Option` on something shared.

RPCs that name a wallet-specific identifier (txids, transparent addresses,
held transactions) live on separate identity clients, each over a transport
of its own, so they cannot ride the sync channel. How many identity clients
a wallet constructs is its unlinkability partition (docs/adr/0001).

Transports yield plain tonic `Channel`s, so construction is the only thing
that changes between direct, Tor, and Nym routes. Errors are
`lightwallet_core::Error` with `code()`/`retryable()`. Retries, timeouts,
and backoff are consumer-side tower layers, never built in.

## Protos

`proto/canonical/` is a pristine git subtree of `zcash/lightwallet-protocol`
at a pinned tag. `proto/overlay/crosslink.proto` is a full mirror of the
canonical service plus the CROSSLINK additions, because proto3 cannot extend
services and Crosslink has no separable proto source. `just mirror-check`
enforces that the overlay remains purely additive, and codegen runs `protox`
in-process, so no `protoc` install is needed to build.

## Development

`just` lists the recipes. The main ones:

```
just check       # protos compile, mirror is additive, workspace + feature matrix build
just test        # offline suite: unit tests + the in-memory mock harness
just live-check  # conformance against real endpoints (nightly, not per-commit)
just coverage    # llvm-cov over the offline suite
```

The mock suite proves self-consistency (both ends share the generated
types). The live suite is the conformance check. Implementation status and
the load-bearing decisions live in [high-level.md](high-level.md).
