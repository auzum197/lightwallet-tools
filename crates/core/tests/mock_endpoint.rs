//! Milestone 3.5: each indexer against the in-memory mock harness. Real prost
//! encode/decode and real HTTP/2 over a duplex pipe, so these exercise the
//! whole client path (request encoding, status mapping, stream adaptation)
//! without a port or a process. Runs on `cargo nextest run --all-features`.
#![cfg(all(feature = "canonical", feature = "crosslink"))]

use futures_util::StreamExt;
use lightwallet_core::{
    CanonicalIdentityClient, CanonicalIndexer, CompactBlockHeader, CrosslinkIdentityClient,
    CrosslinkIndexer, NetworkParams, TestnetIndexer, assert_continuity,
};
use lightwallet_proto_crosslink::BondInfoResponse;
use lightwallet_test_support::{Rpc, canonical, crosslink, mock_hash};
use std::collections::BTreeMap;
use tonic::{Code, Status};

fn params(chain: &str) -> NetworkParams {
    NetworkParams {
        chain_name: chain.into(),
        activation_heights: BTreeMap::new(),
        consensus_branch_id: 0,
    }
}

/// The variant-agnostic sync loop the trait exists for: walk `[start, tip]`
/// and check header continuity. The same body drives both indexers.
async fn sync_to_tip<I: TestnetIndexer>(indexer: &I, start: u64) -> u64 {
    let tip = indexer.get_latest_height().await.unwrap();
    let mut blocks = indexer.get_block_range(start, tip).await.unwrap();
    let mut prev: Option<I::Block> = None;
    let mut seen = 0;
    while let Some(block) = blocks.next().await {
        let block = block.unwrap();
        block.block_hash().unwrap();
        if let Some(prev) = &prev {
            assert!(assert_continuity::<I>(prev, &block));
        }
        prev = Some(block);
        seen += 1;
    }
    seen
}

/// An empty chain answers Unavailable at the tip, and a consumer may retry.
async fn assert_tip_unavailable<I: TestnetIndexer>(indexer: &I) {
    let err = indexer.get_latest_height().await.unwrap_err();
    assert_eq!(err.code(), Some(Code::Unavailable));
    assert!(err.retryable());
}

/// A stream fault after `ok_before_fault` items: the good prefix arrives in
/// order from `start`, then one error item, then the stream ends.
async fn assert_mid_stream_fault<I: TestnetIndexer>(
    indexer: &I,
    start: u64,
    end: u64,
    ok_before_fault: usize,
) where
    I::Block: std::fmt::Debug,
{
    let mut blocks = indexer.get_block_range(start, end).await.unwrap();
    for offset in 0..ok_before_fault {
        let block = blocks.next().await.unwrap().unwrap();
        assert_eq!(block.height(), start + offset as u64);
    }
    let err = blocks.next().await.unwrap().unwrap_err();
    assert_eq!(err.code(), Some(Code::Unavailable));
    assert!(blocks.next().await.is_none());
}

#[tokio::test]
async fn canonical_indexer_syncs_the_mock_chain() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(100, 5));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    assert_eq!(indexer.get_latest_height().await.unwrap(), 104);
    assert_eq!(sync_to_tip(&indexer, 100).await, 5);

    let block = indexer.get_block(102).await.unwrap();
    assert_eq!(block.block_hash().unwrap(), mock_hash(102));
}

#[tokio::test]
async fn crosslink_indexer_runs_the_same_generic_sync() {
    let mock = crosslink::MockStreamer::new().with_blocks(crosslink::linked_blocks(0, 8));
    let indexer = CrosslinkIndexer::new(crosslink::serve(mock).await, params("mock"));

    assert_eq!(sync_to_tip(&indexer, 0).await, 8);
}

#[tokio::test]
async fn injected_fault_surfaces_as_a_retryable_core_error() {
    let mock = canonical::MockStreamer::new()
        .with_blocks(canonical::linked_blocks(0, 3))
        .with_fault(Rpc::GetTreeState, Status::unavailable("injected"));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let err = indexer.get_tree_state(1).await.unwrap_err();
    assert_eq!(err.code(), Some(Code::Unavailable));
    assert!(err.retryable());
    assert!(err.to_string().contains("injected"));

    // The fault is per-RPC: the block path still answers.
    assert_eq!(indexer.get_latest_height().await.unwrap(), 2);
}

#[tokio::test]
async fn mid_stream_fault_arrives_as_an_error_item() {
    let mock = canonical::MockStreamer::new()
        .with_blocks(canonical::linked_blocks(0, 5))
        .with_stream_fault(
            Rpc::GetBlockRange,
            2,
            Status::unavailable("dropped mid-range"),
        );
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    assert_mid_stream_fault(&indexer, 0, 4, 2).await;
}

#[tokio::test]
async fn crosslink_mid_stream_fault_arrives_as_an_error_item() {
    let mock = crosslink::MockStreamer::new()
        .with_blocks(crosslink::linked_blocks(0, 5))
        .with_stream_fault(
            Rpc::GetBlockRange,
            2,
            Status::unavailable("dropped mid-range"),
        );
    let indexer = CrosslinkIndexer::new(crosslink::serve(mock).await, params("mock"));

    assert_mid_stream_fault(&indexer, 0, 4, 2).await;
}

#[tokio::test]
async fn stream_fault_at_position_zero_fails_before_any_block() {
    let mock = canonical::MockStreamer::new()
        .with_blocks(canonical::linked_blocks(0, 5))
        .with_stream_fault(
            Rpc::GetBlockRange,
            0,
            Status::unavailable("dropped at once"),
        );
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    assert_mid_stream_fault(&indexer, 0, 4, 0).await;
}

#[tokio::test]
async fn empty_mock_reports_unavailable_for_tip_and_latest_tree_state() {
    let indexer = CanonicalIndexer::new(
        canonical::serve(canonical::MockStreamer::new()).await,
        params("mock"),
    );

    assert_tip_unavailable(&indexer).await;
    let err = indexer.get_latest_tree_state().await.unwrap_err();
    assert_eq!(err.code(), Some(Code::Unavailable));
    assert!(err.retryable());
}

#[tokio::test]
async fn crosslink_empty_mock_reports_unavailable_at_the_tip() {
    let indexer = CrosslinkIndexer::new(
        crosslink::serve(crosslink::MockStreamer::new()).await,
        params("mock"),
    );

    assert_tip_unavailable(&indexer).await;
}

// The TestnetIndexer impls are hand-written per variant (only the streamer
// surface comes from the macro), so crosslink's block and tree-state paths
// need their own round trips.
#[tokio::test]
async fn crosslink_block_and_tree_state_lookups_answer() {
    let mock = crosslink::MockStreamer::new()
        .with_blocks(crosslink::linked_blocks(10, 2))
        .with_tree_state(lightwallet_proto_crosslink::TreeState {
            height: 11,
            ..Default::default()
        });
    let indexer = CrosslinkIndexer::new(crosslink::serve(mock).await, params("mock"));

    let block = indexer.get_block(11).await.unwrap();
    assert_eq!(block.block_hash().unwrap(), mock_hash(11));
    assert_eq!(indexer.get_tree_state(11).await.unwrap().height, 11);
    assert_eq!(indexer.get_latest_tree_state().await.unwrap().height, 11);
}

#[tokio::test]
async fn faults_on_crosslink_only_rpcs_surface_with_their_codes() {
    let mock = crosslink::MockStreamer::new()
        .with_fault(Rpc::GetRoster, Status::unavailable("roster down"))
        .with_fault(Rpc::GetBondInfo, Status::permission_denied("not yours"))
        .with_fault(
            Rpc::RequestFaucetDonation,
            Status::resource_exhausted("faucet dry"),
        );
    // The mock's duplex carries one connection, so both handles share the
    // channel; in-memory tests have no domains to keep apart.
    let channel = crosslink::serve(mock).await;
    let indexer = CrosslinkIndexer::new(channel.clone(), params("mock"));
    let identity = CrosslinkIdentityClient::new(channel);

    let roster = indexer.get_roster().await.unwrap_err();
    assert_eq!(roster.code(), Some(Code::Unavailable));
    assert!(roster.retryable());

    let bond = indexer.get_bond_info(vec![1; 32]).await.unwrap_err();
    assert_eq!(bond.code(), Some(Code::PermissionDenied));
    assert!(!bond.retryable());

    let faucet = identity
        .request_faucet_donation("u1mock".into())
        .await
        .unwrap_err();
    assert_eq!(faucet.code(), Some(Code::ResourceExhausted));
    assert!(!faucet.retryable());
}

#[tokio::test]
async fn get_transaction_round_trips_and_unknown_txid_is_not_found() {
    let tx = lightwallet_proto_canonical::RawTransaction {
        data: vec![0xaa; 10],
        height: 7,
    };
    let mock = canonical::MockStreamer::new().with_transaction(vec![1; 32], tx);
    let client = CanonicalIdentityClient::new(canonical::serve(mock).await);

    let fetched = client.get_transaction(vec![1; 32]).await.unwrap();
    assert_eq!(fetched.data, vec![0xaa; 10]);
    assert_eq!(fetched.height, 7);

    let err = client.get_transaction(vec![2; 32]).await.unwrap_err();
    assert_eq!(err.code(), Some(Code::NotFound));
    assert!(!err.retryable());
}

#[tokio::test]
async fn lightd_info_and_ping_answer_from_the_mock() {
    let info = lightwallet_proto_canonical::LightdInfo {
        version: "v0.0-mock".into(),
        ..Default::default()
    };
    let mock = canonical::MockStreamer::new().with_lightd_info(info);
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    assert_eq!(
        indexer.get_lightd_info().await.unwrap().version,
        "v0.0-mock"
    );
    let pong = indexer.ping(0).await.unwrap();
    assert_eq!((pong.entry, pong.exit), (0, 0));
}

#[tokio::test]
async fn tree_state_lookups_serve_registered_heights_and_latest_picks_the_highest() {
    let mock = canonical::MockStreamer::new()
        .with_tree_state(lightwallet_proto_canonical::TreeState {
            height: 100,
            ..Default::default()
        })
        .with_tree_state(lightwallet_proto_canonical::TreeState {
            height: 200,
            ..Default::default()
        });
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    assert_eq!(indexer.get_tree_state(100).await.unwrap().height, 100);
    assert_eq!(indexer.get_latest_tree_state().await.unwrap().height, 200);

    let err = indexer.get_tree_state(150).await.unwrap_err();
    assert_eq!(err.code(), Some(Code::NotFound));
}

#[tokio::test]
async fn deprecated_nullifier_methods_surface_unimplemented() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 2));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    #[allow(deprecated)]
    let err = indexer.get_block_nullifiers(0).await.unwrap_err();
    assert_eq!(err.code(), Some(Code::Unimplemented));

    #[allow(deprecated)]
    let ranged = indexer.get_block_range_nullifiers(0, 1).await;
    let Err(err) = ranged else {
        panic!("expected the streaming call to fail at call time")
    };
    assert_eq!(err.code(), Some(Code::Unimplemented));
}

#[tokio::test]
async fn single_block_range_yields_exactly_one_block() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(5, 3));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let heights: Vec<u64> = indexer
        .get_block_range(6, 6)
        .await
        .unwrap()
        .map(|block| block.unwrap().height())
        .collect()
        .await;
    assert_eq!(heights, [6]);
}

#[tokio::test]
async fn descending_range_streams_blocks_in_reverse() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 5));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let heights: Vec<u64> = indexer
        .get_block_range(4, 0)
        .await
        .unwrap()
        .map(|block| block.unwrap().height())
        .collect()
        .await;
    assert_eq!(heights, [4, 3, 2, 1, 0]);
}

#[tokio::test]
async fn range_past_the_tip_returns_the_existing_prefix() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 3));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let blocks: Vec<_> = indexer
        .get_block_range(0, 10)
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(blocks.len(), 3);
    assert!(blocks.iter().all(|block| block.is_ok()));
}

#[tokio::test]
async fn genesis_block_carries_an_all_zero_prev_hash() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 2));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let genesis = indexer.get_block(0).await.unwrap();
    assert_eq!(genesis.prev_block_hash().unwrap(), [0u8; 32]);

    let next = indexer.get_block(1).await.unwrap();
    assert!(assert_continuity::<
        CanonicalIndexer<tonic::transport::Channel>,
    >(&genesis, &next));
}

#[tokio::test]
async fn consumer_detects_a_reorg_through_continuity() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 5));
    let chain = mock.clone();
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    assert_eq!(sync_to_tip(&indexer, 0).await, 5);
    let held_3 = indexer.get_block(3).await.unwrap();

    let mut fork = canonical::linked_blocks(0, 3);
    fork.push(lightwallet_proto_canonical::CompactBlock {
        height: 3,
        hash: mock_hash(1003).to_vec(),
        prev_hash: mock_hash(2).to_vec(),
        ..Default::default()
    });
    fork.push(lightwallet_proto_canonical::CompactBlock {
        height: 4,
        hash: mock_hash(1004).to_vec(),
        prev_hash: mock_hash(1003).to_vec(),
        ..Default::default()
    });
    chain.replace_chain(fork);

    type Ix = CanonicalIndexer<tonic::transport::Channel>;
    let new_4 = indexer.get_block(4).await.unwrap();
    assert!(!assert_continuity::<Ix>(&held_3, &new_4));

    let new_3 = indexer.get_block(3).await.unwrap();
    let unchanged_2 = indexer.get_block(2).await.unwrap();
    assert!(assert_continuity::<Ix>(&unchanged_2, &new_3));
    assert!(assert_continuity::<Ix>(&new_3, &new_4));
}

#[tokio::test]
async fn network_params_return_the_carried_values() {
    let mut heights = BTreeMap::new();
    heights.insert("nu5".to_string(), 1_687_104);
    let carried = NetworkParams {
        chain_name: "custom".into(),
        activation_heights: heights,
        consensus_branch_id: 0xc2d6_d0b4,
    };
    let indexer = CanonicalIndexer::new(
        canonical::serve(canonical::MockStreamer::new()).await,
        carried.clone(),
    );

    assert_eq!(indexer.network_params(), &carried);
}

#[tokio::test]
async fn missing_block_maps_to_not_found_not_a_panic() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(10, 2));
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let err = indexer.get_block(999).await.unwrap_err();
    assert_eq!(err.code(), Some(Code::NotFound));
    assert!(!err.retryable());
}

#[tokio::test]
async fn sent_transactions_are_observable_on_the_mock() {
    let mock = canonical::MockStreamer::new();
    let inbox = mock.clone();
    let client = CanonicalIdentityClient::new(canonical::serve(mock).await);

    let resp = client.send_transaction(vec![0xab; 40]).await.unwrap();
    assert_eq!(resp.error_code, 0);

    let sent = inbox.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].data, vec![0xab; 40]);
}

#[tokio::test]
async fn crosslink_only_surface_answers_concretely() {
    let mock = crosslink::MockStreamer::new()
        .with_roster(b"roster-bytes".to_vec())
        .with_bond(
            vec![1; 32],
            BondInfoResponse {
                amount: 5_000,
                status: 1,
            },
        )
        .with_faucet_amount(250_000);
    let channel = crosslink::serve(mock).await;
    let indexer = CrosslinkIndexer::new(channel.clone(), params("mock"));
    let identity = CrosslinkIdentityClient::new(channel);

    let bond = indexer.get_bond_info(vec![1; 32]).await.unwrap();
    assert_eq!((bond.amount, bond.status), (5_000, 1));

    let unknown = indexer.get_bond_info(vec![2; 32]).await.unwrap_err();
    assert_eq!(unknown.code(), Some(Code::NotFound));

    assert_eq!(indexer.get_roster().await.unwrap().data, b"roster-bytes");
    assert_eq!(
        identity
            .request_faucet_donation("u1mockaddress".into())
            .await
            .unwrap()
            .amount,
        250_000
    );
}

#[tokio::test]
async fn mempool_txs_honor_the_exclude_suffixes() {
    let tx = |last: u8| lightwallet_proto_canonical::CompactTx {
        txid: {
            let mut txid = vec![0u8; 32];
            txid[31] = last;
            txid
        },
        ..Default::default()
    };
    let mock = canonical::MockStreamer::new().with_mempool_txs([tx(1), tx(2)]);
    let client = CanonicalIdentityClient::new(canonical::serve(mock).await);

    let all: Vec<_> = client
        .get_mempool_tx(Vec::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(all.len(), 2);

    let remaining: Vec<_> = client
        .get_mempool_tx(vec![vec![1]])
        .await
        .unwrap()
        .map(|tx| tx.unwrap().txid[31])
        .collect()
        .await;
    assert_eq!(remaining, [2]);
}

#[tokio::test]
async fn mempool_stream_drains_to_completion() {
    let raw = |height: u64| lightwallet_proto_canonical::RawTransaction {
        data: Vec::new(),
        height,
    };
    let mock = canonical::MockStreamer::new().with_mempool_stream([raw(1), raw(2), raw(3)]);
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let heights: Vec<u64> = indexer
        .get_mempool_stream()
        .await
        .unwrap()
        .map(|tx| tx.unwrap().height)
        .collect()
        .await;
    assert_eq!(heights, [1, 2, 3]);
}

#[tokio::test]
async fn taddress_transactions_filter_by_height_range() {
    let raw = |height: u64| lightwallet_proto_canonical::RawTransaction {
        data: Vec::new(),
        height,
    };
    let mock =
        canonical::MockStreamer::new().with_taddress_txs("t1known", [raw(5), raw(10), raw(15)]);
    let client = CanonicalIdentityClient::new(canonical::serve(mock).await);

    let heights: Vec<u64> = client
        .get_taddress_transactions("t1known".into(), 8, 20)
        .await
        .unwrap()
        .map(|tx| tx.unwrap().height)
        .collect()
        .await;
    assert_eq!(heights, [10, 15]);

    #[allow(deprecated)]
    let deprecated: Vec<_> = client
        .get_taddress_txids("t1known".into(), 8, 20)
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(deprecated.len(), 2);

    let unknown: Vec<_> = client
        .get_taddress_transactions("t1unknown".into(), 0, 100)
        .await
        .unwrap()
        .collect()
        .await;
    assert!(unknown.is_empty());
}

#[tokio::test]
async fn balance_answers_both_unary_and_client_streaming() {
    let mock = canonical::MockStreamer::new().with_balance(1_234);
    let inbox = mock.clone();
    let client = CanonicalIdentityClient::new(canonical::serve(mock).await);

    let unary = client
        .get_taddress_balance(vec!["t1a".into()])
        .await
        .unwrap();
    assert_eq!(unary.value_zat, 1_234);

    let addresses = futures_util::stream::iter([
        lightwallet_proto_canonical::Address {
            address: "t1a".into(),
        },
        lightwallet_proto_canonical::Address {
            address: "t1b".into(),
        },
    ]);
    let streamed = client.get_taddress_balance_stream(addresses).await.unwrap();
    assert_eq!(streamed.value_zat, 1_234);
    assert_eq!(inbox.balance_queries(), ["t1a", "t1b"]);
}

#[tokio::test]
async fn utxos_respect_max_entries_on_both_forms() {
    let utxo = |height: u64| lightwallet_proto_canonical::GetAddressUtxosReply {
        address: "t1known".into(),
        height,
        ..Default::default()
    };
    let mock = canonical::MockStreamer::new().with_utxos([utxo(1), utxo(2), utxo(3)]);
    let client = CanonicalIdentityClient::new(canonical::serve(mock).await);

    let from_2 = client
        .get_address_utxos(vec!["t1known".into()], 2, 0)
        .await
        .unwrap();
    let heights: Vec<u64> = from_2.address_utxos.iter().map(|u| u.height).collect();
    assert_eq!(heights, [2, 3]);

    let capped = client
        .get_address_utxos(vec!["t1known".into()], 0, 1)
        .await
        .unwrap();
    assert_eq!(capped.address_utxos.len(), 1);

    let streamed: Vec<u64> = client
        .get_address_utxos_stream(vec!["t1known".into()], 2, 0)
        .await
        .unwrap()
        .map(|utxo| utxo.unwrap().height)
        .collect()
        .await;
    assert_eq!(streamed, [2, 3]);
}

#[tokio::test]
async fn subtree_roots_page_from_start_index() {
    let root = |height: u64| lightwallet_proto_canonical::SubtreeRoot {
        completing_block_height: height,
        ..Default::default()
    };
    let mock = canonical::MockStreamer::new().with_subtree_roots([root(10), root(20), root(30)]);
    let indexer = CanonicalIndexer::new(canonical::serve(mock).await, params("mock"));

    let arg = |start_index, max_entries| lightwallet_proto_canonical::GetSubtreeRootsArg {
        start_index,
        shielded_protocol: 1,
        max_entries,
    };

    let paged: Vec<u64> = indexer
        .get_subtree_roots(arg(1, 0))
        .await
        .unwrap()
        .map(|root| root.unwrap().completing_block_height)
        .collect()
        .await;
    assert_eq!(paged, [20, 30]);

    let capped: Vec<_> = indexer
        .get_subtree_roots(arg(0, 1))
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(capped.len(), 1);
}

#[tokio::test]
async fn crosslink_mempool_txs_flow_through_the_macro_surface() {
    let tx = lightwallet_proto_crosslink::CompactTx {
        txid: vec![9u8; 32],
        ..Default::default()
    };
    let mock = crosslink::MockStreamer::new().with_mempool_txs([tx]);
    let client = CrosslinkIdentityClient::new(crosslink::serve(mock).await);

    let txs: Vec<_> = client
        .get_mempool_tx(Vec::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(txs.len(), 1);
    assert_eq!(txs[0].as_ref().unwrap().txid, vec![9u8; 32]);
}
