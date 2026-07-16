# lightwallet-proto-crosslink

Generated bindings for the CROSSLINK variant: the canonical lightwalletd
surface plus the Crosslink additions (finalizer roster, bond info, faucet).

The source is `proto/overlay/crosslink.proto`, a full mirror of canonical
`service.proto` with added RPCs only, because proto3 cannot extend services
and Crosslink ships no separable proto source (only a fork with in-place
edits). Three workspace checks keep the mirror sound: `just mirror-check`
(purely additive against canonical), `just rpc-coverage-check` (every RPC in
the pinned upstream snapshot exists in the overlay), and `just
upstream-check` (diff the snapshot against `crosslink_monolith` on demand).

Codegen matches `lightwallet-proto-canonical`: `protox` compiles the protos
in-process (no `protoc`), `tonic-prost-build` emits the message types,
`CompactTxStreamerClient`, the `CompactTxStreamer` server trait, and
`FILE_DESCRIPTOR_SET`. Typed wrappers live in `lightwallet-core`.
