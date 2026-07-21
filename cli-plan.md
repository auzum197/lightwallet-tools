# lwcli: one-shot gRPC client for the lightwallet indexers

CONTEXT.md names a "debug client" among the downstream consumers of the
protocol layer. This is it: `lwcli`, a clap binary in `crates/cli`
(crate `lightwallet-cli`) that issues a single RPC against a variant's
endpoint and prints the response. grpcurl-like in spirit, but typed: each RPC
is a subcommand calling `CanonicalIndexerClient`/`CrosslinkIndexerClient` from
`lightwallet-core`, so the CLI doubles as a dogfooding consumer of the crate's
public API.

## Decisions

- Typed subcommands, one per RPC, covering the full surface of both variants,
  deprecated RPCs included and marked "(deprecated upstream)" in help. No
  dynamic prost-reflect call path.
- `--variant <canonical|crosslink>`, default `canonical`. The flag name is the
  CONTEXT.md glossary term. Crosslink-only commands (`get-roster`,
  `get-bond-info`, `request-faucet-donation`) imply crosslink, and an explicit
  `--variant canonical` combined with one of them is an error. Cargo features
  were rejected for variant selection: compile-time choice fights a
  point-at-anything debug tool, so the binary enables both core features.
- `--url` flag only. No env fallback, no default endpoint. TLS follows the
  scheme (https means webpki roots, http means plaintext).
- `--timeout <secs>` covers connection establishment only, default 10. RPCs
  get no deadline: `get-mempool-stream` legitimately runs until the next
  block, and Ctrl-C is the kill switch.
- JSON output by default, rendered through a prost-reflect `DynamicMessage`
  round-trip against the proto crates' `FILE_DESCRIPTOR_SET`s, with proto
  field names preserved. `bytes` fields print as hex rather than base64.
  Streaming RPCs emit NDJSON as items arrive. `--output debug` prints `{:#?}`.
- Txids and block hashes cross the CLI boundary in display order (as explorers
  show them), reversed to and from wire order internally. Other byte fields
  (nullifiers, commitments, roster bytes) print in wire order.
- `send-transaction` takes a hex positional, with `-` reading hex from stdin.
- Shell completions come from `lwcli completions <shell>` (clap_complete,
  printed to stdout).

## Transports

The workspace transport crates already yield a plain tonic `Channel`, so the
CLI change is flag surface plus the connect path.

- `--transport <direct|tor|nym>`, default `direct`. The flag name is the
  CONTEXT.md glossary term. `tor` runs arti in-process, `nym` dials a running
  `nym-socks5-client` and is marked experimental in help until it clears the
  promotion gate (CONTEXT.md). An in-process Nym client that
  would drop the external binary is sketched in nym-embedded-plan.md.
- Tor bootstraps with arti's default config, so the directory cache persists
  in the platform state dir and later runs are fast. One `bootstrapping
  tor...` line goes to stderr before connecting. Stdout stays pure JSON.
- `--timeout` keeps meaning connection establishment (the SOCKS5 handshake or
  the circuit dial included). Bootstrap runs unbounded, Ctrl-C kills it.
- The scheme-to-TLS rule is unchanged for every transport: tonic runs the
  handshake end-to-end through the tunnel. `.onion` endpoints are out of
  scope (see onion-plan.md).
- `--nym-socks5 <addr>` defaults to `127.0.0.1:1080`, the `nym-socks5-client`
  default. Passing it explicitly implies `--transport nym`, the same rule
  crosslink-only commands use for the variant. An explicit contradiction
  (`--transport tor --nym-socks5 ...`) is an error.
- The transports are default-on cargo features (`tor`, `nym`) on
  `lightwallet-cli`, because arti roughly doubles the CLI's build. The
  `--transport` enum values are cfg-gated to match, so a build without a
  feature doesn't advertise a value it can't honor.
- Testing: the in-process SOCKS5 server from `transport-nym/tests/tunnel.rs`
  moved to `lightwallet-test-support`, and the CLI e2e suite drives
  `--transport nym` through it offline. Tor has no offline story (arti needs
  the real network), so it gets a manual live smoke, matching the transport
  crate's own live suite one layer down.
