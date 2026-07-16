//! Generated bindings for the CROSSLINK variant: the canonical lightwalletd
//! surface plus the Crosslink additions (finalizer roster, bond info,
//! faucet) from `proto/overlay/crosslink.proto`.
//!
//! The overlay is a full mirror of canonical `service.proto` with added
//! RPCs only (proto3 cannot extend services), enforced by `just
//! mirror-check`. Codegen only, as in `lightwallet-proto-canonical`:
//! `protox` compiles the protos in-process (no `protoc`), and
//! `tonic-prost-build` emits the message types,
//! [`compact_tx_streamer_client`], and the [`compact_tx_streamer_server`]
//! trait. Typed wrappers live in `lightwallet-core`.

include!(concat!(env!("OUT_DIR"), "/cash.z.wallet.sdk.rpc.rs"));

/// Descriptor set for this variant's protos, for gRPC server reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptor.bin"));
