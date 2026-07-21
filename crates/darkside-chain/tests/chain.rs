//! Behavior tests for the chain state machine: the spec claims.

use darkside_chain::{
    Chain, ChainParams, Corruption, FundSpec, PendingState, Pool, Recipient, Seed, SendSpec,
};

const ZEC: u64 = 100_000_000;

fn chain() -> Chain {
    Chain::new(ChainParams::regtest(), Seed::from(0xC0FFEE))
}

fn declared(names: &[&str]) -> Chain {
    let mut c = chain();
    for name in names {
        c.declare_account(name, &format!("{name}-test-seed"), 0)
            .expect("derivation succeeds");
    }
    c
}

fn fund(recipient: Recipient, pool: Option<Pool>, zats: u64, at: u32) -> FundSpec {
    FundSpec {
        recipient,
        pool,
        zats,
        outputs: 1,
        at,
        via_coinbase: false,
        corruption: None,
    }
}

fn send(from: &str, zats: u64, recipient: Recipient, at: u32) -> SendSpec {
    SendSpec {
        from: from.into(),
        pool: None,
        zats,
        recipient,
        at: Some(at),
        pending_from: None,
        expiring_at: None,
        corruption: None,
    }
}

#[test]
fn same_seed_byte_identical_chain() {
    let mut a = declared(&["alice"]);
    let mut b = declared(&["alice"]);
    for c in [&mut a, &mut b] {
        c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
            .expect("fund registers");
        c.mine(10).expect("mining succeeds");
    }
    assert_eq!(a.tip_height(), 10);
    for h in 0..=10 {
        let (ba, bb) = (a.block(h).unwrap(), b.block(h).unwrap());
        assert_eq!(ba.hash.0, bb.hash.0, "hash differs at {h}");
        assert_eq!(ba.txs.len(), bb.txs.len());
        for (ta, tb) in ba.txs.iter().zip(&bb.txs) {
            assert_eq!(ta.txid, tb.txid);
            assert_eq!(ta.raw, tb.raw);
        }
    }
}

#[test]
fn hash_chain_is_continuous() {
    let mut c = chain();
    c.mine(5).expect("mining succeeds");
    for h in 1..=5 {
        assert_eq!(
            c.block(h).unwrap().prev_hash.0,
            c.block(h - 1).unwrap().hash.0
        );
    }
}

#[test]
fn every_block_has_a_coinbase_at_index_zero() {
    let mut c = declared(&["alice"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, ZEC, 2))
        .expect("fund registers");
    c.mine(3).expect("mining succeeds");
    for block in c.blocks() {
        let coinbase = &block.txs[0];
        let bundle = coinbase
            .tx
            .transparent_bundle()
            .expect("coinbase is transparent");
        assert!(
            bundle.is_coinbase(),
            "index 0 must be coinbase at {}",
            block.height
        );
    }
    // The funded transaction landed at index >= 1 with no inputs.
    let funded = &c.block(2).unwrap().txs[1];
    assert!(funded.tx.orchard_bundle().is_some());
}

#[test]
fn fund_credits_the_default_pool() {
    let mut c = declared(&["alice"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    c.mine(5).expect("mining succeeds");
    assert_eq!(c.expected_balance("alice").unwrap(), 10 * ZEC);
    assert_eq!(
        c.expected_balance_in("alice", Pool::Orchard).unwrap(),
        10 * ZEC
    );
    assert_eq!(c.expected_balance_in("alice", Pool::Sapling).unwrap(), 0);
}

#[test]
fn multi_output_fund_mints_many_notes_in_one_tx() {
    let mut c = declared(&["alice"]);
    c.fund(FundSpec {
        recipient: Recipient::Declared("alice".into()),
        pool: None,
        zats: 3 * ZEC,
        outputs: 8,
        at: 2,
        via_coinbase: false,
        corruption: None,
    })
    .expect("fund registers");
    c.mine(2).expect("mining succeeds");

    let notes = c.notes_for("alice").unwrap();
    assert_eq!(notes.len(), 8, "one note per output");
    assert!(
        notes
            .iter()
            .all(|n| n.value == 3 * ZEC && n.pool == Pool::Orchard)
    );
    assert_eq!(c.expected_balance("alice").unwrap(), 24 * ZEC);

    // All eight outputs ride in a single transaction: the block holds the
    // coinbase plus that one fund, and every note carries its txid.
    let block = c.block(2).unwrap();
    assert_eq!(block.txs.len(), 2);
    let fund_txid = block.txs[1].txid;
    assert!(notes.iter().all(|n| n.txid == fund_txid));
}

#[test]
fn fund_count_beyond_the_per_tx_cap_splits_into_multiple_txs() {
    // Two funds at the same height, as the drive schedules when the note
    // count exceeds its per-transaction cap.
    let mut c = declared(&["alice"]);
    for outputs in [8u32, 4] {
        c.fund(FundSpec {
            recipient: Recipient::Declared("alice".into()),
            pool: None,
            zats: 3 * ZEC,
            outputs,
            at: 2,
            via_coinbase: false,
            corruption: None,
        })
        .expect("fund registers");
    }
    c.mine(2).expect("mining succeeds");

    let notes = c.notes_for("alice").unwrap();
    assert_eq!(notes.len(), 12);
    assert_eq!(c.expected_balance("alice").unwrap(), 36 * ZEC);

    // Coinbase plus two fund transactions, and the notes split across the two.
    let block = c.block(2).unwrap();
    assert_eq!(block.txs.len(), 3);
    let distinct: std::collections::HashSet<_> = notes.iter().map(|n| n.txid).collect();
    assert_eq!(distinct.len(), 2);
}

#[test]
fn zero_value_and_dust_notes_are_recorded() {
    let mut c = declared(&["alice"]);
    // Three dust notes below a conventional fee, and three worth nothing.
    c.fund(FundSpec {
        recipient: Recipient::Declared("alice".into()),
        pool: None,
        zats: 1_000,
        outputs: 3,
        at: 2,
        via_coinbase: false,
        corruption: None,
    })
    .expect("dust fund registers");
    c.fund(FundSpec {
        recipient: Recipient::Declared("alice".into()),
        pool: None,
        zats: 0,
        outputs: 3,
        at: 2,
        via_coinbase: false,
        corruption: None,
    })
    .expect("zero-value fund registers");
    c.mine(2).expect("mining succeeds");

    let notes = c.notes_for("alice").unwrap();
    assert_eq!(notes.len(), 6, "zero-value notes are kept, not dropped");
    assert_eq!(notes.iter().filter(|n| n.value == 0).count(), 3);
    assert_eq!(notes.iter().filter(|n| n.value == 1_000).count(), 3);
    assert_eq!(c.expected_balance("alice").unwrap(), 3_000);
}

#[test]
fn zero_output_fund_is_rejected() {
    let mut c = declared(&["alice"]);
    let err = c.fund(FundSpec {
        recipient: Recipient::Declared("alice".into()),
        pool: None,
        zats: 3 * ZEC,
        outputs: 0,
        at: 2,
        via_coinbase: false,
        corruption: None,
    });
    assert!(err.is_err());
}

#[test]
fn fund_sapling_and_ironwood_pools() {
    let mut c = declared(&["alice"]);
    c.fund(fund(
        Recipient::Declared("alice".into()),
        Some(Pool::Sapling),
        3 * ZEC,
        4,
    ))
    .expect("sapling fund registers");
    c.fund(fund(
        Recipient::Declared("alice".into()),
        Some(Pool::Ironwood),
        2 * ZEC,
        5,
    ))
    .expect("ironwood fund registers");
    c.mine(5).expect("mining succeeds");
    assert_eq!(
        c.expected_balance_in("alice", Pool::Sapling).unwrap(),
        3 * ZEC
    );
    assert_eq!(
        c.expected_balance_in("alice", Pool::Ironwood).unwrap(),
        2 * ZEC
    );
    assert_eq!(c.expected_balance("alice").unwrap(), 5 * ZEC);
    // Ironwood rides in the V6 slot of its own.
    let iron_tx = &c.block(5).unwrap().txs[1];
    assert!(iron_tx.tx.ironwood_bundle().is_some());
    assert!(iron_tx.tx.orchard_bundle().is_none());
}

#[test]
fn send_moves_value_between_declared_accounts() {
    let mut c = declared(&["alice", "bob"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    c.send(send("alice", 2 * ZEC, Recipient::Declared("bob".into()), 7))
        .expect("send registers");
    c.mine(7).expect("mining succeeds");
    assert_eq!(c.expected_balance("alice").unwrap(), 8 * ZEC);
    assert_eq!(c.expected_balance("bob").unwrap(), 2 * ZEC);
    // The spent note is marked, change came back.
    let notes = c.notes_for("alice").unwrap();
    assert!(notes.iter().any(|n| n.spent == Some(7)));
    assert!(
        notes
            .iter()
            .any(|n| n.spent.is_none() && n.value == 8 * ZEC)
    );
}

#[test]
fn send_to_external_recipient_recovered_via_ovk() {
    let mut c = declared(&["alice"]);
    // An external wallet: derived independently, never declared.
    let external =
        darkside_chain::Account::derive(c.params(), "ext", "external-wallet-seed", 0).unwrap();
    let external_ua = external.ua().encode(&c.params().network);

    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    c.send(send("alice", 3 * ZEC, Recipient::Literal(external_ua), 7))
        .expect("send registers");
    c.mine(7).expect("mining succeeds");

    assert_eq!(c.expected_balance("alice").unwrap(), 7 * ZEC);
    let outgoing = c.outgoing_payments("alice").unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].value, 3 * ZEC);
    assert_eq!(outgoing[0].height, 7);
}

#[test]
fn mempool_dwell_is_height_anchored() {
    let mut c = declared(&["alice", "bob"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 3))
        .expect("fund registers");
    let mut spec = send("alice", ZEC, Recipient::Declared("bob".into()), 7);
    spec.pending_from = Some(4);
    c.send(spec).expect("send registers");

    c.mine(4).expect("mining to 4 succeeds");
    assert_eq!(c.mempool().len(), 1, "pending from 4: visible at tip 4");
    c.mine(2).expect("mining to 6 succeeds");
    assert_eq!(c.mempool().len(), 1, "still unmined at 6");
    c.mine(1).expect("mining to 7 succeeds");
    assert!(c.mempool().is_empty(), "mined at 7");
    assert_eq!(c.expected_balance("bob").unwrap(), ZEC);
}

#[test]
fn expiring_send_moves_no_value_ever() {
    let mut c = declared(&["alice", "bob"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 3))
        .expect("fund registers");
    let spec = SendSpec {
        from: "alice".into(),
        pool: None,
        zats: ZEC,
        recipient: Recipient::Declared("bob".into()),
        at: None,
        pending_from: Some(5),
        expiring_at: Some(8),
        corruption: None,
    };
    c.send(spec).expect("send registers");
    c.mine(6).expect("mining to 6 succeeds");
    assert_eq!(c.mempool().len(), 1, "visible while pending");
    c.mine(4).expect("mining to 10 succeeds");
    assert!(c.mempool().is_empty(), "evicted after expiry");
    assert_eq!(c.expected_balance("alice").unwrap(), 10 * ZEC);
    assert_eq!(c.expected_balance("bob").unwrap(), 0);
    assert!(c.pending_txs().iter().any(|p| matches!(
        p.state,
        PendingState::Evicted(_, darkside_chain::Eviction::Expired)
    )));
}

#[test]
fn transparent_fund_and_coinbase_maturity_marking() {
    let mut c = declared(&["alice"]);
    c.fund(fund(
        Recipient::DeclaredTransparent("alice".into()),
        None,
        2 * ZEC,
        4,
    ))
    .expect("taddr fund registers");
    c.fund(FundSpec {
        recipient: Recipient::DeclaredTransparent("alice".into()),
        pool: None,
        zats: 5 * ZEC,
        outputs: 1,
        at: 5,
        via_coinbase: true,
        corruption: None,
    })
    .expect("coinbase fund registers");
    c.mine(5).expect("mining succeeds");

    assert_eq!(c.expected_transparent_balance("alice").unwrap(), 7 * ZEC);
    let utxos = c.utxos_for("alice").unwrap();
    assert_eq!(utxos.len(), 2);
    let coinbase_utxos: Vec<_> = utxos
        .iter()
        .filter(|u| c.utxo_set().is_coinbase(&u.outpoint))
        .collect();
    assert_eq!(coinbase_utxos.len(), 1);
    assert_eq!(coinbase_utxos[0].value.into_u64(), 5 * ZEC);
    // The coinbase fund sits at index 0 of block 5.
    let block5 = c.block(5).unwrap();
    let cb = block5.txs[0].tx.transparent_bundle().unwrap();
    assert!(cb.is_coinbase());
    assert_eq!(cb.vout[0].value().into_u64(), 5 * ZEC);
    // Address history covers both receives.
    assert_eq!(
        c.utxo_set()
            .txids_for(c.account("alice").unwrap().taddr())
            .len(),
        2
    );
}

#[test]
fn fork_shares_prefix_and_diverges_after() {
    let mut main = declared(&["alice"]);
    main.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    main.mine(10).expect("mining succeeds");

    let mut fork = main.fork_at(8).expect("fork succeeds");
    assert_eq!(fork.tip_height(), 8);
    // Shared prefix: identical hashes, identical ground truth.
    for h in 0..=8 {
        assert_eq!(fork.block(h).unwrap().hash.0, main.block(h).unwrap().hash.0);
    }
    assert_eq!(fork.expected_balance("alice").unwrap(), 10 * ZEC);

    fork.mine(4).expect("fork mining succeeds");
    assert_eq!(fork.tip_height(), 12);
    // Divergence: same height, different block.
    assert_ne!(fork.block(9).unwrap().hash.0, main.block(9).unwrap().hash.0);
    assert_eq!(
        fork.block(9).unwrap().prev_hash.0,
        fork.block(8).unwrap().hash.0
    );
}

#[test]
fn fork_above_tip_is_rejected() {
    let mut c = chain();
    c.mine(3).expect("mining succeeds");
    assert!(c.fork_at(7).is_err());
}

#[test]
fn overdraw_is_caught_while_building() {
    let mut c = declared(&["alice", "bob"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, ZEC, 3))
        .expect("fund registers");
    c.send(send("alice", 5 * ZEC, Recipient::Declared("bob".into()), 6))
        .expect("registration accepts; fabrication rejects");
    let err = c.mine(6).expect_err("overdraw surfaces while building");
    assert!(matches!(
        err,
        darkside_chain::Error::InsufficientFunds { .. }
    ));
}

#[test]
fn corrupt_commitment_debits_sender_credits_nobody() {
    let mut c = declared(&["alice", "bob"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    let mut spec = send("alice", 2 * ZEC, Recipient::Declared("bob".into()), 7);
    spec.corruption = Some(Corruption::Commitment);
    c.send(spec).expect("send registers");
    c.mine(7).expect("mining succeeds");
    // The spend half is real: alice is debited and keeps only change.
    assert_eq!(c.expected_balance("alice").unwrap(), 8 * ZEC);
    // The rigorous scanner rejects the mismatched commitment: bob gets nothing.
    assert_eq!(c.expected_balance("bob").unwrap(), 0);
}

#[test]
fn corrupt_spentness_leaves_sender_untouched() {
    let mut c = declared(&["alice", "bob"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    let mut spec = send("alice", 2 * ZEC, Recipient::Declared("bob".into()), 7);
    spec.corruption = Some(Corruption::Spentness);
    c.send(spec).expect("send registers");
    c.mine(7).expect("mining succeeds");
    // The nullifiers were lies: no note of alice's was really spent.
    assert_eq!(
        c.notes_for("alice")
            .unwrap()
            .iter()
            .filter(|n| n.spent.is_some())
            .count(),
        0
    );
    // The outputs are real: bob is credited.
    assert_eq!(c.expected_balance("bob").unwrap(), 2 * ZEC);
}

#[test]
fn tree_states_and_subtree_roots_are_served() {
    let mut c = declared(&["alice"]);
    c.fund(fund(Recipient::Declared("alice".into()), None, ZEC, 2))
        .expect("fund registers");
    c.mine(3).expect("mining succeeds");
    let states = c.tree_states(3).expect("tree state at tip");
    assert!(
        states.orchard_size >= 2,
        "fund appended orchard commitments"
    );
    assert_eq!(states.ironwood_size, 0);
    assert!(!states.sapling.is_empty());
    // No pool is anywhere near 65,536 notes: an empty stream is correct.
    assert!(c.subtree_roots(Pool::Orchard).is_empty());
    // Tree state at an earlier height reflects that height, and heights
    // beyond the tip do not exist.
    let genesis = c.tree_states(0).expect("genesis tree state");
    assert_eq!(genesis.orchard_size, 0);
    assert!(c.tree_states(9).is_none());
}

#[test]
fn submitted_raw_bytes_round_trip_through_the_accept_path() {
    // Chain A plays the wallet: it fabricates a send whose raw bytes we
    // lift. Chain B shares the same declared world through height 6, so
    // the spent note exists there with the same nullifier.
    let mut a = declared(&["alice", "bob"]);
    a.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    a.send(send("alice", 2 * ZEC, Recipient::Declared("bob".into()), 7))
        .expect("send registers");
    a.mine(7).expect("mining succeeds");
    let raw = a.block(7).unwrap().txs[1].raw.clone();

    let mut b = declared(&["alice", "bob"]);
    b.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    b.mine(6).expect("mining succeeds");
    let txid = b
        .submit(&raw)
        .expect("structurally valid bytes are accepted");
    assert_eq!(b.mempool().len(), 1);
    b.mine(1).expect("mining succeeds");
    assert!(b.mempool().is_empty());
    assert_eq!(b.expected_balance("alice").unwrap(), 8 * ZEC);
    assert_eq!(b.expected_balance("bob").unwrap(), 2 * ZEC);
    assert_eq!(b.transaction(&txid).unwrap().height, Some(7));
}

#[test]
fn withheld_submissions_never_mine() {
    let mut a = declared(&["alice", "bob"]);
    a.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    a.send(send("alice", 2 * ZEC, Recipient::Declared("bob".into()), 7))
        .expect("send registers");
    a.mine(7).expect("mining succeeds");
    let raw = a.block(7).unwrap().txs[1].raw.clone();

    let mut b = declared(&["alice", "bob"]);
    b.fund(fund(Recipient::Declared("alice".into()), None, 10 * ZEC, 5))
        .expect("fund registers");
    b.mine(6).expect("mining succeeds");
    b.set_withhold(true);
    b.submit(&raw).expect("accepted into the mempool");
    b.mine(3).expect("mining succeeds");
    assert_eq!(b.mempool().len(), 1, "withheld: visible, never mined");
    assert_eq!(b.expected_balance("alice").unwrap(), 10 * ZEC);
}

#[test]
fn garbage_bytes_are_rejected_on_parse_only() {
    let mut c = chain();
    assert!(c.submit(&[0xde, 0xad, 0xbe, 0xef]).is_err());
}

#[test]
fn corrupt_prev_hash_breaks_continuity_on_request() {
    let mut c = chain();
    c.mine(5).expect("mining succeeds");
    c.corrupt_prev_hash(3).expect("block exists");
    assert_ne!(c.block(3).unwrap().prev_hash.0, c.block(2).unwrap().hash.0);
}

#[test]
fn mainnet_boots_past_nu5_with_orchard_live() {
    let params = ChainParams::main();
    let boot = params.start_height;
    // The boot height defaults to mainnet's real NU5 activation.
    assert_eq!(boot, 1_687_104);
    let mut c = Chain::new(params, Seed::from(1));
    assert_eq!(c.tip_height(), boot);
    c.declare_account("alice", "alice-test-seed", 0)
        .expect("derives");
    // An Orchard fund at a real mainnet height succeeds, so the pool is
    // live there.
    c.fund(fund(
        Recipient::Declared("alice".into()),
        Some(Pool::Orchard),
        5 * ZEC,
        boot + 1,
    ))
    .expect("orchard fund registers");
    c.mine(1).expect("mines the fund");
    assert_eq!(
        c.expected_balance_in("alice", Pool::Orchard).unwrap(),
        5 * ZEC
    );
    // Genesis carried only its coinbase; the shielded trees start empty.
    assert_eq!(c.tree_states(boot).unwrap().orchard_size, 0);
}

#[test]
fn prehistory_serves_empty_continuous_blocks() {
    let params = ChainParams::main();
    let boot = params.start_height;
    let c = Chain::new(params, Seed::from(2));

    // A band of blocks straddling the boot height reads back continuous:
    // each block's prev_hash equals the hash of the block below it, whether
    // that block is on-demand prehistory or the materialized boot block.
    let mut prev = c.block_at(boot - 4).expect("prehistory block");
    assert!(prev.txs.is_empty(), "prehistory blocks are empty");
    for height in (boot - 3)..=boot {
        let block = c.block_at(height).expect("block exists");
        assert_eq!(block.prev_hash.0, prev.hash.0, "continuity at {height}");
        prev = block;
    }
    // Prehistory tree state is empty, so any post-Sapling birthday anchors
    // on an empty tree.
    let trees = c.tree_states(boot - 2).expect("prehistory tree state");
    assert_eq!((trees.sapling_size, trees.orchard_size), (0, 0));
    // Nothing exists above the tip.
    assert!(c.block_at(boot + 1).is_none());
}

#[test]
fn forks_share_prehistory_and_diverge_after_the_fork_point() {
    // A mainnet chain mined a few blocks past its boot height, then forked.
    let params = ChainParams::main();
    let boot = params.start_height;
    let mut main = Chain::new(params, Seed::from(3));
    main.mine(4).expect("mining succeeds");
    let fork = main.fork_at(boot + 2).expect("fork within range");

    // Shared prehistory: an empty block below the boot height is identical.
    assert_eq!(
        main.block_at(boot - 5).unwrap().hash.0,
        fork.block_at(boot - 5).unwrap().hash.0,
    );
    // Shared prefix up to the fork point.
    assert_eq!(
        main.block(boot + 2).unwrap().hash.0,
        fork.block(boot + 2).unwrap().hash.0,
    );
    // Divergence begins one block above the fork point once the fork mines.
    let mut forked = fork;
    forked.mine(3).expect("fork mines on");
    assert_ne!(
        main.block(boot + 3).unwrap().hash.0,
        forked.block(boot + 3).unwrap().hash.0,
    );
}
