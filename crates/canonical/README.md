# lightwallet-proto-canonical

Generated bindings for the CANONICAL variant: the lightwalletd gRPC surface
defined by [`zcash/lightwallet-protocol`](https://github.com/zcash/lightwallet-protocol),
vendored as a git subtree at a pinned tag under `proto/canonical/`.

Codegen only, no hand-written API. The build script compiles
`service.proto` and `compact_formats.proto` with `protox` (in-process, no
`protoc` install) and emits through `tonic-prost-build`:

- the prost message types (`CompactBlock`, `RawTransaction`, ...)
- `compact_tx_streamer_client::CompactTxStreamerClient` for callers
- `compact_tx_streamer_server::CompactTxStreamer` for mock or real servers
- `FILE_DESCRIPTOR_SET` for gRPC server reflection

Typed wrappers over the client live in `lightwallet-core`. To move to a new
upstream tag, run `just canonical-pull <tag>` at the workspace root, then
re-sync the Crosslink overlay and run `just check`.
