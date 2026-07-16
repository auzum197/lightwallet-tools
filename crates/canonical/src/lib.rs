//! Generated bindings for the CANONICAL variant: the lightwalletd gRPC
//! surface defined by `zcash/lightwallet-protocol`, vendored as a git
//! subtree at a pinned tag under `proto/canonical`.
//!
//! Codegen only. Everything here is emitted by `tonic-prost-build` from
//! `service.proto` and `compact_formats.proto`, compiled in-process by
//! `protox` (no `protoc` needed): the prost message types, a
//! [`compact_tx_streamer_client`] for callers, and a
//! [`compact_tx_streamer_server`] trait for mock or real servers. Typed
//! wrappers over the client live in `lightwallet-core`.

include!(concat!(env!("OUT_DIR"), "/cash.z.wallet.sdk.rpc.rs"));

/// Descriptor set for this variant's protos, for gRPC server reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/descriptor.bin"));
