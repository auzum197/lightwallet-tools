//! Client layer for Zcash lightwalletd-style indexers, generic over protocol
//! variant and transport.
//!
//! Two variants exist, each behind a cargo feature. `canonical` is the stock
//! lightwalletd surface defined by `zcash/lightwallet-protocol`. `crosslink`
//! mirrors it and adds the Crosslink fork's RPCs (finalizer roster, bond
//! info, faucet). There is no shared normalized block type: each variant
//! keeps its generated types, and genericity comes from the narrow
//! [`CompactBlockHeader`] capability plus associated types on
//! [`IndexerClient`].
//!
//! # API layout
//!
//! The wire surface splits three ways:
//!
//! - [`IndexerClient`] carries the block-sync path (latest height, block
//!   fetch, block ranges, tree state), the one place consumers are genuinely
//!   variant-agnostic.
//! - The rest of the chain-wide surface (mempool stream, subtree roots,
//!   server info) exists as identical inherent methods on
//!   [`CanonicalIndexerClient`] and [`CrosslinkIndexerClient`], returning each variant's
//!   own generated types. Variant-only RPCs are inherent methods on
//!   [`CrosslinkIndexerClient`] alone, never an `Option` on something shared.
//! - RPCs whose request content names a wallet-specific identifier (a txid,
//!   a transparent address, held transactions) live on
//!   [`CanonicalIdentityClient`] and [`CrosslinkIdentityClient`], each built
//!   over a transport of its own so they cannot ride the sync channel.
//!   Construct one per identity the wallet wants a server to see as a
//!   stranger (docs/adr/0001 in the repository).
//!
//! # Transports, errors, streams
//!
//! Indexers and identity clients are generic over [`GrpcTransport`], which
//! any tonic `Channel` satisfies: a direct connection, or one built by
//! `lightwallet-transport-tor` / `lightwallet-transport-nym`. Failures
//! surface as [`Error`], with [`Error::code`] and [`Error::retryable`] for
//! classification. Server streams are `BoxStream<'static, Result<T>>` whose
//! items are individually fallible. Retries, timeouts, and backoff are
//! deliberately absent: layer them on the consumer side, per method.
//!
//! # Example
//!
//! ```no_run
//! use lightwallet_core::{CanonicalIndexerClient, NetworkParams, IndexerClient};
//! use tonic::transport::Endpoint;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let channel = Endpoint::from_static("https://zec.rocks:443")
//!     .connect()
//!     .await?;
//! let params = NetworkParams {
//!     chain_name: "main".into(),
//!     activation_heights: Default::default(),
//!     consensus_branch_id: 0,
//! };
//! let client = CanonicalIndexerClient::new(channel, params);
//! let tip = client.get_latest_height().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Features
//!
//! - `canonical` (default): [`CanonicalIndexerClient`] and
//!   [`CanonicalIdentityClient`], over `lightwallet-proto-canonical`.
//! - `crosslink`: [`CrosslinkIndexerClient`] and [`CrosslinkIdentityClient`], over
//!   `lightwallet-proto-crosslink`.
//!
//! The features are additive. A build with neither still exposes the traits,
//! [`NetworkParams`], and [`Error`].

mod error;
mod header;
#[cfg(any(feature = "canonical", feature = "crosslink"))]
mod identity;
mod indexer;
mod params;
#[cfg(any(feature = "canonical", feature = "crosslink"))]
mod streamer;
mod transport;

pub use error::{Error, Result};
pub use header::{CompactBlockHeader, HashLen};
pub use indexer::{IndexerClient, assert_continuity};
pub use params::NetworkParams;
pub use transport::GrpcTransport;

#[cfg(feature = "canonical")]
mod canonical;
#[cfg(feature = "canonical")]
pub use canonical::{CanonicalIdentityClient, CanonicalIndexerClient};

#[cfg(feature = "crosslink")]
mod crosslink;
#[cfg(feature = "crosslink")]
pub use crosslink::{CrosslinkIdentityClient, CrosslinkIndexerClient};

// The same generic function runs against both variants' block types with no
// conversion and no shared struct, exercised here at the type level.
#[cfg(all(test, feature = "canonical", feature = "crosslink"))]
mod tests {
    use super::*;
    use tonic::transport::Channel;

    #[test]
    fn continuity_holds_across_both_variants() {
        let a = lightwallet_proto_canonical::CompactBlock {
            height: 100,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let b = lightwallet_proto_canonical::CompactBlock {
            height: 101,
            prev_hash: vec![7u8; 32],
            ..Default::default()
        };
        assert!(assert_continuity::<CanonicalIndexerClient<Channel>>(&a, &b));

        let c = lightwallet_proto_crosslink::CompactBlock {
            height: 100,
            hash: vec![9u8; 32],
            ..Default::default()
        };
        let d = lightwallet_proto_crosslink::CompactBlock {
            height: 101,
            prev_hash: vec![9u8; 32],
            ..Default::default()
        };
        assert!(assert_continuity::<CrosslinkIndexerClient<Channel>>(&c, &d));
    }

    // The macros are supposed to give both variants the same inherent surface
    // on each type. Naming a method as a function item on each variant fails
    // to compile if a signature drifts or a method goes missing from one side.
    #[test]
    fn both_variants_expose_the_full_shared_surface() {
        let _ = (
            CanonicalIndexerClient::<Channel>::get_latest_height,
            CanonicalIndexerClient::<Channel>::get_mempool_stream,
            CanonicalIndexerClient::<Channel>::get_lightd_info,
            CanonicalIndexerClient::<Channel>::ping,
        );
        let _ = (
            CrosslinkIndexerClient::<Channel>::get_latest_height,
            CrosslinkIndexerClient::<Channel>::get_mempool_stream,
            CrosslinkIndexerClient::<Channel>::get_lightd_info,
            CrosslinkIndexerClient::<Channel>::ping,
            // The Crosslink-only chain-wide surface, concrete and off the
            // shared trait.
            CrosslinkIndexerClient::<Channel>::get_bond_info,
            CrosslinkIndexerClient::<Channel>::get_roster,
        );
        let _ = (
            CanonicalIdentityClient::<Channel>::get_transaction,
            CanonicalIdentityClient::<Channel>::send_transaction,
            CanonicalIdentityClient::<Channel>::get_taddress_balance,
            CanonicalIdentityClient::<Channel>::get_mempool_tx,
            CanonicalIdentityClient::<Channel>::get_address_utxos,
        );
        let _ = (
            CrosslinkIdentityClient::<Channel>::get_transaction,
            CrosslinkIdentityClient::<Channel>::send_transaction,
            CrosslinkIdentityClient::<Channel>::get_taddress_balance,
            CrosslinkIdentityClient::<Channel>::get_mempool_tx,
            CrosslinkIdentityClient::<Channel>::get_address_utxos,
            // Faucet requests name the wallet's address, so the method lives
            // on the identity client.
            CrosslinkIdentityClient::<Channel>::request_faucet_donation,
        );
    }

    #[test]
    fn continuity_rejects_a_skipped_height() {
        let a = lightwallet_proto_canonical::CompactBlock {
            height: 100,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let b = lightwallet_proto_canonical::CompactBlock {
            height: 102,
            prev_hash: vec![7u8; 32],
            ..Default::default()
        };
        assert!(!assert_continuity::<CanonicalIndexerClient<Channel>>(&a, &b));
    }

    #[test]
    fn continuity_rejects_a_repeated_height() {
        let a = lightwallet_proto_canonical::CompactBlock {
            height: 100,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let b = lightwallet_proto_canonical::CompactBlock {
            height: 100,
            prev_hash: vec![7u8; 32],
            ..Default::default()
        };
        assert!(!assert_continuity::<CanonicalIndexerClient<Channel>>(&a, &b));
    }

    #[test]
    fn continuity_rejects_a_prev_hash_mismatch_at_the_next_height() {
        let a = lightwallet_proto_canonical::CompactBlock {
            height: 100,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let b = lightwallet_proto_canonical::CompactBlock {
            height: 101,
            prev_hash: vec![8u8; 32],
            ..Default::default()
        };
        assert!(!assert_continuity::<CanonicalIndexerClient<Channel>>(&a, &b));

        let c = lightwallet_proto_crosslink::CompactBlock {
            height: 100,
            hash: vec![9u8; 32],
            ..Default::default()
        };
        let d = lightwallet_proto_crosslink::CompactBlock {
            height: 101,
            prev_hash: vec![10u8; 32],
            ..Default::default()
        };
        assert!(!assert_continuity::<CrosslinkIndexerClient<Channel>>(&c, &d));
    }

    #[test]
    fn typed_hash_accessor_catches_wrong_length_instead_of_panicking() {
        let ok = lightwallet_proto_canonical::CompactBlock {
            hash: vec![7u8; 32],
            ..Default::default()
        };
        assert_eq!(ok.block_hash().unwrap(), [7u8; 32]);

        let short = lightwallet_proto_canonical::CompactBlock {
            hash: vec![1u8; 20],
            ..Default::default()
        };
        assert_eq!(short.block_hash(), Err(HashLen { len: 20 }));
    }
}
