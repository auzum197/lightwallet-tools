# Zcash Lightwallet Protocol Layer

Rust client tooling for lightwalletd-style Zcash indexers. The client crates
put two protocol variants behind one generic API, CANONICAL
([`zcash/lightwallet-protocol`](https://github.com/zcash/lightwallet-protocol))
and CROSSLINK (the Crosslink fork's additive mirror), and route them directly
or through Tor or Nym without changing call sites.

> **Status: heavily experimental, heavy LLM-authored code right now.** Treat
> most of this repo as a moving target that can change or break without notice.
> The only stable-ish crates are the three library ones you would depend on:
> [`lightwallet-core`](crates/core),
> [`lightwallet-proto-canonical`](crates/canonical), and
> [`lightwallet-proto-crosslink`](crates/crosslink). Everything else (the
> transports and CLI) is in flux.

```mermaid
flowchart LR
  client["client stack<br/>lwcli, lwtui, lightwallet-core"]
  bindings["variant bindings<br/>canonical, crosslink"]
  endpoints["indexer endpoints<br/>lightwalletd / zaino"]

  client --> bindings
  client ==>|"gRPC over direct, Tor, or Nym"| endpoints
```

## Crates

- [`lightwallet-proto-canonical`](crates/canonical): generated tonic/prost bindings for the canonical lightwalletd protocol.
- [`lightwallet-proto-crosslink`](crates/crosslink): the same generated bindings, for the Crosslink variant.
- [`lightwallet-core`](crates/core): the client layer proper, typed access to indexers that is generic over both variant and transport. Its README covers taking it as a dependency.
- [`lightwallet-transport-tor`](crates/transport-tor): tonic channels over Tor via arti, each channel its own circuit-isolation domain.
- [`lightwallet-transport-nym`](crates/transport-nym): tonic channels through the Nym mixnet via a running `nym-socks5-client` (experimental).
- [`lightwallet-cli`](tools/cli): `lwcli`, a one-shot point-at-anything debug client covering the full RPC surface.
- [`lightwallet-tui`](tools/tui): `lwtui`, a live terminal monitor for indexers: mempool tail, block explorer, and tx detail.
- [`lightwallet-txview`](tools/txview): shared transaction projection (`ParsedTx`) used by `lwcli` and `lwtui`.
- [`lightwallet-test-support`](crates/test-support): in-memory mock endpoints with fault injection, plus a SOCKS5 test server.

## Development

`just` lists the recipes. The main ones:

```
just check       # protos compile, mirror is additive, workspace + feature matrix build
just test        # offline suite: unit tests + the in-memory mock harness
just live-check  # conformance against real endpoints (nightly, not per-commit)
just coverage    # llvm-cov over the offline suite
```

The mock suite proves self-consistency (both ends share the generated
types). The live suite is the conformance check. The load-bearing decisions
live in [docs/adr/](docs/adr/).
