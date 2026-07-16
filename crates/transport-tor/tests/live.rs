//! Live validation for the Tor transport: bootstrap arti, connect the
//! canonical indexer through it, and sync a block. Ignored by default (it
//! reaches the Tor network and a live endpoint); runs under `just live-check`.
//!
//! Arti state and cache live under the OS temp dir so repeat runs reuse the
//! downloaded directory info without touching the user's arti install.

use lightwallet_core::{CanonicalIndexer, CompactBlockHeader, NetworkParams, TestnetIndexer};
use std::collections::BTreeMap;
use tonic::transport::{ClientTlsConfig, Endpoint};

#[tokio::test]
#[ignore = "live network (tor bootstrap + canonical endpoint)"]
async fn canonical_indexer_over_tor() {
    let url = std::env::var("LIGHTWALLET_CANONICAL_URL")
        .unwrap_or_else(|_| "https://zec.rocks:443".into());

    let dirs = std::env::temp_dir().join("lightwallet-transport-tor-test");
    let config = arti_client::config::TorClientConfigBuilder::from_directories(
        dirs.join("state"),
        dirs.join("cache"),
    )
    .build()
    .expect("arti config");
    let tor = arti_client::TorClient::create_bootstrapped(config)
        .await
        .expect("bootstrap tor");

    let endpoint = Endpoint::from_shared(url)
        .expect("endpoint url")
        .tls_config(ClientTlsConfig::new().with_webpki_roots())
        .expect("tls config");
    let channel = lightwallet_transport_tor::channel(&endpoint, &tor)
        .await
        .expect("tor channel");

    let indexer = CanonicalIndexer::new(
        channel,
        NetworkParams {
            chain_name: "live".into(),
            activation_heights: BTreeMap::new(),
            consensus_branch_id: 0,
        },
    );

    let tip = indexer
        .get_latest_height()
        .await
        .expect("latest height over tor");
    assert!(tip > 0);
    let block = indexer.get_block(tip).await.expect("tip block over tor");
    block.block_hash().expect("32-byte hash");
}
