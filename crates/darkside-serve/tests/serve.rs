//! End-to-end: a declared world served over real gRPC, consumed with the
//! workspace's own client layer, indistinguishable from a real indexer.

use darkside_chain::{Chain, ChainParams, Seed};
use darkside_decl::Declaration;
use darkside_serve::{Darkside, DarksideIndexerClient, canonical, crosslink, run_scenario};
use futures_util::StreamExt as _;
use lightwallet_core::{
    CanonicalIdentityClient, CanonicalIndexerClient, IdentityTransport, IndexerClient,
    NetworkParams, is_continuous,
};

const ZEC: u64 = 100_000_000;

const WORLD: &str = r#"
network regtest
seed 0xC0FFEE

account alice seed "alice-test-seed"
account bob seed "bob-test-seed"

chain main {
  blocks 0..5
  fund alice 10 ZEC at 5
  fund alice.taddr 2 ZEC at 5
  send alice 2 ZEC to bob at 7
  blocks 9..10
}

chain fork from main@8 {
  blocks 8..12
}

scenario reorg_after_sync {
  serve main
  wait block_requested >= 10
  serve fork
  expect tip == 12
}
"#;

fn client_params() -> NetworkParams {
    NetworkParams {
        chain_name: "darkside-regtest".into(),
        activation_heights: Default::default(),
        consensus_branch_id: 0,
    }
}

fn built_main() -> Chain {
    let decl = Declaration::parse(WORLD).expect("world parses");
    decl.build_chain("main", None).expect("main builds")
}

async fn sync_with_continuity<I: IndexerClient>(indexer: &I) -> u64 {
    let tip = indexer.get_latest_height().await.expect("latest height");
    let mut stream = indexer.get_block_range(0, tip).await.expect("range opens");
    let mut prev: Option<I::Block> = None;
    let mut count = 0u64;
    while let Some(block) = stream.next().await {
        let block = block.expect("stream item");
        if let Some(prev) = &prev {
            assert!(
                is_continuous::<I>(prev, &block),
                "continuity broke at {count}"
            );
        }
        prev = Some(block);
        count += 1;
    }
    count
}

async fn count_continuous_range<I: IndexerClient>(indexer: &I, start: u64, end: u64) -> u64 {
    let mut stream = indexer
        .get_block_range(start, end)
        .await
        .expect("range opens");
    let mut prev: Option<I::Block> = None;
    let mut count = 0u64;
    while let Some(block) = stream.next().await {
        let block = block.expect("stream item");
        if let Some(prev) = &prev {
            assert!(
                is_continuous::<I>(prev, &block),
                "continuity broke near {count}"
            );
        }
        prev = Some(block);
        count += 1;
    }
    count
}

#[tokio::test]
async fn a_wallet_view_over_grpc_syncs_the_declared_world() {
    let darkside = Darkside::new(built_main());
    let channel = canonical::serve_in_process(darkside.clone()).await;
    let indexer = CanonicalIndexerClient::new(channel, client_params());

    assert_eq!(indexer.get_latest_height().await.unwrap(), 10);
    assert_eq!(sync_with_continuity(&indexer).await, 11);

    // Block 5 carries the two funds at index >= 1; the coinbase sits at 0.
    let block5 = indexer.get_block(5).await.unwrap();
    assert_eq!(block5.vtx.len(), 3);
    assert_eq!(block5.vtx[0].index, 0);
    assert!(!block5.vtx[1].actions.is_empty(), "shielded fund");
    assert!(!block5.vtx[2].vout.is_empty(), "transparent fund");
    let metadata = block5.chain_metadata.expect("chain metadata present");
    assert!(metadata.orchard_commitment_tree_size >= 2);

    // The send at 7 spends and pays: nullifiers and new actions.
    let block7 = indexer.get_block(7).await.unwrap();
    assert!(
        block7.vtx[1]
            .actions
            .iter()
            .any(|a| !a.nullifier.is_empty())
    );

    // Tree states serve all three pools.
    let state = indexer.get_tree_state(10).await.unwrap();
    assert!(!state.sapling_tree.is_empty());
    assert!(!state.orchard_tree.is_empty());
    assert!(!state.ironwood_tree.is_empty());

    // GetLightdInfo identifies darkside and reports consistent params.
    let info = indexer.get_lightd_info().await.unwrap();
    assert!(info.vendor.contains("darkside"));
    assert_eq!(info.block_height, 10);
    assert_eq!(info.sapling_activation_height, 1);
}

#[tokio::test]
async fn send_transaction_round_trips_over_grpc() {
    // The "wallet": a fully built world whose send at 7 supplies raw bytes
    // a real wallet would have constructed itself.
    let source = built_main();
    let sent = &source.block(7).unwrap().txs[1];
    let raw = sent.raw.clone();
    let expected_txid = sent.txid;

    // The served world: same declaration, stopped at 6, without the send.
    let decl = Declaration::parse(
        r#"
network regtest
seed 0xC0FFEE

account alice seed "alice-test-seed"
account bob seed "bob-test-seed"

chain main {
  blocks 0..6
  fund alice 10 ZEC at 5
  fund alice.taddr 2 ZEC at 5
}
"#,
    )
    .expect("parses");
    let darkside = Darkside::new(decl.build_chain("main", None).expect("builds"));
    let channel = canonical::serve_in_process(darkside.clone()).await;
    let identity = CanonicalIdentityClient::new(IdentityTransport::dedicated(channel));

    let response = identity.send_transaction(raw).await.expect("rpc succeeds");
    assert_eq!(response.error_code, 0, "{}", response.error_message);
    // lightwalletd echoes the txid in error_message on success, and wallets
    // parse it straight back into a TxId. An empty field fails their conversion
    // ("invalid txid length, should be 32 bytes") before a block is even mined.
    assert_eq!(response.error_message, expected_txid.to_string());

    // Visible in the mempool, then mined by the driver's tick.
    let mempool: Vec<_> = identity
        .get_mempool_tx(Vec::new(), Vec::new())
        .await
        .expect("mempool opens")
        .collect()
        .await;
    assert_eq!(mempool.len(), 1);

    darkside.mine_with_time(1_800_000_000).expect("mines");
    assert_eq!(
        darkside.with_chain(|c| c.expected_balance("bob").unwrap()),
        2 * ZEC
    );
    assert_eq!(
        darkside.with_chain(|c| c.expected_balance("alice").unwrap()),
        8 * ZEC + 2 * ZEC
    );

    // GetTransaction serves the full bytes back with the mined height.
    let fetched = identity
        .get_transaction(expected_txid.as_ref().to_vec())
        .await
        .expect("found");
    assert_eq!(fetched.height, 7);
}

#[tokio::test]
async fn the_reorg_scenario_runs_against_a_syncing_wallet() {
    let decl = Declaration::parse(WORLD).expect("parses");
    let world = decl.build().expect("builds");
    let scenario = world.scenarios[0].clone();

    let darkside = Darkside::new(Chain::new(decl.params.clone(), decl.seed));
    let runner = {
        let darkside = darkside.clone();
        let chains = world.chains;
        tokio::spawn(async move { run_scenario(&darkside, chains, &scenario).await })
    };

    let channel = canonical::serve_in_process(darkside.clone()).await;
    let indexer = CanonicalIndexerClient::new(channel, client_params());

    // Wait for `serve main`, then sync to 10, firing the barrier.
    while indexer.get_latest_height().await.unwrap() != 10 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let main_block9 = indexer.get_block(9).await.unwrap();
    assert_eq!(sync_with_continuity(&indexer).await, 11);

    // The runner swaps to the fork and asserts tip == 12.
    runner
        .await
        .expect("runner task")
        .expect("scenario completes");
    assert_eq!(indexer.get_latest_height().await.unwrap(), 12);

    // The wallet re-fetching height 9 sees the divergence and can walk
    // back to the shared prefix at 8.
    let fork_block9 = indexer.get_block(9).await.unwrap();
    assert_ne!(fork_block9.hash, main_block9.hash);
    let fork_block8 = indexer.get_block(8).await.unwrap();
    assert_eq!(fork_block9.prev_hash, fork_block8.hash);
    assert_eq!(sync_with_continuity(&indexer).await, 13);
}

#[tokio::test]
async fn the_direct_indexer_client_needs_no_socket() {
    let darkside = Darkside::new(built_main());
    let indexer = DarksideIndexerClient::new(darkside);

    assert_eq!(indexer.get_latest_height().await.unwrap(), 10);
    assert_eq!(sync_with_continuity(&indexer).await, 11);
    assert_eq!(indexer.network_params().chain_name, "darkside-regtest");
    assert_eq!(
        indexer
            .network_params()
            .activation_heights
            .get("NU6.3")
            .copied(),
        Some(1)
    );
}

fn main_client_params() -> NetworkParams {
    NetworkParams {
        chain_name: "main".into(),
        activation_heights: Default::default(),
        consensus_branch_id: 0,
    }
}

#[tokio::test]
async fn a_mainnet_wallet_syncs_a_bounded_window_across_the_boot_height() {
    // A mainnet-flavored darkside: real prefixes and schedule, booted at
    // NU5 and mined a few blocks on.
    let params = ChainParams::main();
    let boot = params.start_height;
    let mut chain = Chain::new(params, Seed::from(7));
    chain.mine(3).expect("mines past boot");
    let darkside = Darkside::new(chain);
    let channel = canonical::serve_in_process(darkside.clone()).await;
    let indexer = CanonicalIndexerClient::new(channel, main_client_params());

    assert_eq!(
        indexer.get_latest_height().await.unwrap(),
        (boot + 3) as u64
    );

    // A fresh wallet anchors just below the boot height and syncs a bounded
    // window forward. Continuity holds across the prehistory/materialized
    // boundary.
    let synced = count_continuous_range(&indexer, (boot - 2) as u64, (boot + 3) as u64).await;
    assert_eq!(synced, 6);

    // GetTreeState at a prehistory anchor serves the same empty tree the
    // boot block does: a wallet witnesses nothing before its birthday.
    let anchor = indexer.get_tree_state((boot - 1) as u64).await.unwrap();
    let at_boot = indexer.get_tree_state(boot as u64).await.unwrap();
    assert_eq!(anchor.orchard_tree, at_boot.orchard_tree);
    assert_eq!(anchor.sapling_tree, at_boot.sapling_tree);

    // The wallet believes it is on mainnet.
    let info = indexer.get_lightd_info().await.unwrap();
    assert_eq!(info.chain_name, "main");
    assert_eq!(info.sapling_activation_height, 419_200);
}

#[tokio::test]
async fn the_crosslink_faucet_funds_the_requesting_address() {
    let decl = Declaration::parse(WORLD).expect("parses");
    let darkside = Darkside::new(Chain::new(decl.params.clone(), decl.seed));
    let channel = crosslink::serve_in_process(darkside.clone()).await;
    let identity =
        lightwallet_core::CrosslinkIdentityClient::new(IdentityTransport::dedicated(channel));

    // An external wallet's UA, never declared.
    let external = darkside_chain::Account::derive(&decl.params, "ext", "external-wallet-seed", 0)
        .expect("derives");
    let ua = external.ua().encode(&decl.params.network);

    let donation = identity
        .request_faucet_donation(ua)
        .await
        .expect("faucet answers");
    assert_eq!(donation.amount, ZEC);

    // The next tick mines the donation as an orchard fund at index 1.
    darkside.mine_with_time(1_800_000_000).expect("mines");
    let funded = darkside.with_chain(|c| {
        let block = c.block(c.tip_height()).unwrap();
        !block.txs[1]
            .tx
            .orchard_bundle()
            .unwrap()
            .actions()
            .is_empty()
    });
    assert!(funded);
}

/// A long range is served a chunk at a time so the chain lock is handed back
/// while a wallet reads. The chunk seams are where continuity would break, so
/// this crosses several of them, and crosses a skipped span while it is at it.
#[tokio::test]
async fn a_long_range_is_continuous_across_chunk_seams() {
    let darkside = Darkside::new(built_main());
    darkside
        .jump_to(1_000, 1_800_000_000)
        .expect("jumps above the tip");
    let indexer = DarksideIndexerClient::new(darkside);

    assert_eq!(count_continuous_range(&indexer, 0, 1_000).await, 1_001);
}

/// A descending range has to reverse the chunk order and each chunk's own
/// blocks. Reversing one without the other gives a sawtooth that arrives with
/// the right length, so the heights themselves are checked.
#[tokio::test]
async fn a_descending_range_descends_across_chunk_seams() {
    let darkside = Darkside::new(built_main());
    darkside
        .jump_to(1_000, 1_800_000_000)
        .expect("jumps above the tip");
    let indexer = DarksideIndexerClient::new(darkside);

    let mut stream = indexer
        .get_block_range(1_000, 0)
        .await
        .expect("range opens");
    let mut heights = Vec::new();
    while let Some(block) = stream.next().await {
        heights.push(block.expect("stream item").height);
    }

    assert_eq!(heights.len(), 1_001);
    assert!(
        heights.windows(2).all(|pair| pair[0] == pair[1] + 1),
        "order broke: {:?}",
        &heights[..heights.len().min(12)]
    );
}
