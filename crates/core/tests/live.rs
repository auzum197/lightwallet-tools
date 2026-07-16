//! Milestone 3.6: each indexer against a real endpoint. Real servers are the
//! adversarial case for the abstraction (they were not co-designed with it),
//! and this is the only suite that can catch wire-format drift the mock
//! harness is structurally blind to (§5 caveat).
//!
//! Ignored by default; run with `just live-check`, nightly rather than
//! per-commit. Endpoints:
//!
//! - CANONICAL: `LIGHTWALLET_CANONICAL_URL`, default `https://zec.rocks:443`.
//! - CROSSLINK: `LIGHTWALLET_CROSSLINK_URL`, no default. Featurenets reset
//!   each season, so the current endpoint is deployment config; the test
//!   skips when the variable is unset.
#![cfg(all(feature = "canonical", feature = "crosslink"))]

use futures_util::StreamExt;
use lightwallet_core::{
    CanonicalIndexer, CompactBlockHeader, CrosslinkIndexer, NetworkParams, TestnetIndexer,
    assert_continuity,
};
use std::collections::BTreeMap;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

fn params(chain: &str) -> NetworkParams {
    NetworkParams {
        chain_name: chain.into(),
        activation_heights: BTreeMap::new(),
        consensus_branch_id: 0,
    }
}

async fn connect(url: &str) -> Channel {
    let mut endpoint = Endpoint::from_shared(url.to_string()).expect("endpoint url");
    if url.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .expect("tls config");
    }
    endpoint
        .connect()
        .await
        .unwrap_or_else(|e| panic!("connect to {url}: {e}"))
}

/// The §3 exit criterion against production infrastructure: the identical
/// generic sync loop that drives the mocks walks a live tip window, checking
/// hash-chain continuity and 32-byte hashes, then fetches tree state through
/// the trait's opaque associated type.
async fn scan_live_tip<I: TestnetIndexer>(indexer: &I, window: u64) {
    let tip = indexer.get_latest_height().await.expect("latest height");
    assert!(tip > 0, "live chain reports an empty best chain");
    let start = tip.saturating_sub(window - 1);

    let mut blocks = indexer
        .get_block_range(start, tip)
        .await
        .expect("block range");
    let mut prev: Option<I::Block> = None;
    let mut seen = 0;
    while let Some(block) = blocks.next().await {
        let block = block.expect("stream item");
        block.block_hash().expect("32-byte block hash");
        block.prev_block_hash().expect("32-byte prev hash");
        if let Some(prev) = &prev {
            assert!(
                assert_continuity::<I>(prev, &block),
                "hash chain broke at height {}",
                block.height()
            );
        }
        prev = Some(block);
        seen += 1;
    }
    assert_eq!(seen, tip - start + 1);

    let single = indexer.get_block(start).await.expect("single block");
    assert_eq!(single.height(), start);

    indexer.get_tree_state(start).await.expect("tree state");
    indexer
        .get_latest_tree_state()
        .await
        .expect("latest tree state");
}

#[tokio::test]
#[ignore = "live network (canonical endpoint)"]
async fn canonical_indexer_against_a_live_endpoint() {
    let url = std::env::var("LIGHTWALLET_CANONICAL_URL")
        .unwrap_or_else(|_| "https://zec.rocks:443".into());
    let indexer = CanonicalIndexer::new(connect(&url).await, params("live"));

    let info = indexer.get_lightd_info().await.expect("lightd info");
    assert!(
        info.chain_name == "main" || info.chain_name == "test",
        "unexpected chain name {:?}",
        info.chain_name
    );

    scan_live_tip(&indexer, 10).await;

    // The inherent surface decodes real (non-compact) responses too.
    let tip = indexer.get_latest_height().await.unwrap();
    let tree = indexer.get_tree_state(tip.saturating_sub(9)).await.unwrap();
    assert_eq!(tree.height, tip.saturating_sub(9));
}

#[tokio::test]
#[ignore = "live network (crosslink featurenet)"]
async fn crosslink_indexer_against_a_live_featurenet() {
    let Ok(url) = std::env::var("LIGHTWALLET_CROSSLINK_URL") else {
        eprintln!("LIGHTWALLET_CROSSLINK_URL unset, skipping (featurenets reset each season)");
        return;
    };
    let indexer = CrosslinkIndexer::new(connect(&url).await, params("featurenet"));

    let info = indexer.get_lightd_info().await.expect("lightd info");
    assert!(!info.chain_name.is_empty());

    scan_live_tip(&indexer, 10).await;

    // Read-only piece of the CROSSLINK-only surface. Bond and faucet calls
    // mutate or depend on featurenet state, so the suite leaves them out.
    indexer.get_roster().await.expect("bft roster");
}
