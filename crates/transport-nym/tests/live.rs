//! Live validation for the Nym transport: connect the canonical indexer
//! through a running `nym-socks5-client` and run the generic sync loop over a
//! tip window. Ignored by default; runs under `just live-check-nym`.
//!
//! The proxy address comes from `LIGHTWALLET_NYM_SOCKS5_ADDR` (no default:
//! a reachable, funded socks5 client is deployment setup, so the test skips
//! when it is unset). The endpoint comes from `LIGHTWALLET_CANONICAL_URL`,
//! defaulting to zec.rocks.
//!
//! The test prints the wall-clock time for the block-range sync. That number,
//! not the assertions, is the promotion gate: the mixnet is 5-hop with cover
//! traffic, and the crate only graduates from experiment to promised
//! transport if the sync rate holds up against the milestone 3.6 bar.

use futures_util::StreamExt;
use lightwallet_core::{
    CanonicalIndexerClient, CompactBlockHeader, IndexerClient, NetworkParams, is_continuous,
};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Instant;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

#[tokio::test]
#[ignore = "live network (running nym-socks5-client + canonical endpoint)"]
async fn canonical_indexer_over_nym() {
    let Ok(socks) = std::env::var("LIGHTWALLET_NYM_SOCKS5_ADDR") else {
        eprintln!("LIGHTWALLET_NYM_SOCKS5_ADDR unset, skipping (no nym-socks5-client to dial)");
        return;
    };
    let socks: SocketAddr = socks.parse().expect("socks5 address");
    let url = std::env::var("LIGHTWALLET_CANONICAL_URL")
        .unwrap_or_else(|_| "https://zec.rocks:443".into());

    let endpoint = Endpoint::from_shared(url)
        .expect("endpoint url")
        .tls_config(ClientTlsConfig::new().with_webpki_roots())
        .expect("tls config");
    let channel = lightwallet_transport_nym::channel(&endpoint, socks)
        .await
        .expect("nym channel");

    let indexer = CanonicalIndexerClient::new(
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
        .expect("latest height over nym");
    assert!(tip > 0);

    let window = 10;
    let start = tip.saturating_sub(window - 1);
    let synced_at = Instant::now();
    let mut blocks = indexer
        .get_block_range(start, tip)
        .await
        .expect("block range");
    let mut prev = None;
    let mut seen = 0u64;
    while let Some(block) = blocks.next().await {
        let block = block.expect("stream item");
        block.block_hash().expect("32-byte block hash");
        if let Some(prev) = &prev {
            assert!(
                is_continuous::<CanonicalIndexerClient<Channel>>(prev, &block),
                "hash chain broke at height {}",
                block.height()
            );
        }
        prev = Some(block);
        seen += 1;
    }
    assert_eq!(seen, window);
    eprintln!(
        "synced {window} blocks over the mixnet in {:.1}s",
        synced_at.elapsed().as_secs_f64()
    );
}
