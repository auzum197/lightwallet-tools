//! Funding an address nobody declared, through the command dispatcher.
//!
//! The address is derived from a throwaway chain and then funded on a
//! different chain that never declared it, which is the whole point: a dev
//! brings a wallet, hands over the address it prints, and darkside pays it.

use darkside_chain::{Chain, ChainParams, FundSpec, Receiver, Recipient, Seed};
use darkside_serve::command::{BootChain, Command, Detail, Dispatcher, parse_receivers, parse_zec};
use darkside_serve::{Darkside, MinerControl, Tick};
use tokio::sync::watch;

const ZEC: u64 = 100_000_000;

/// Everything active from height 1, so ironwood is reachable.
fn params() -> ChainParams {
    let mut params = ChainParams::regtest();
    params
        .network
        .apply_overrides("all=1")
        .expect("regtest schedule accepts all=1");
    params
}

/// A unified address darkside has no key for. Derived on a chain that is
/// then dropped, so the address arrives the way a real wallet's would.
fn undeclared_ua() -> String {
    let mut elsewhere = Chain::new(params(), Seed::from(7));
    elsewhere
        .declare_account("external", "some-other-wallets-seed", 0)
        .expect("declaring an account on a throwaway chain");
    let account = elsewhere.account("external").expect("just declared");
    account.ua().encode(&elsewhere.params().network)
}

fn dispatcher() -> Dispatcher {
    let boot = BootChain::new(params(), Seed::from(0));
    let darkside = Darkside::new(boot.build());
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    Dispatcher::new(darkside, miner, boot, 10_000)
}

fn fund(dispatcher: &Dispatcher, address: &str, zec: &str, receivers: Option<&str>) -> Detail {
    let outcome = dispatcher
        .run(Command::Fund {
            address: address.to_owned(),
            zats: parse_zec(zec).expect("test amounts parse"),
            receivers: receivers.map(|r| parse_receivers(r).expect("test receiver sets parse")),
        })
        .expect("fund succeeds");
    outcome.detail
}

fn outputs(detail: &Detail) -> Vec<(char, u64)> {
    match detail {
        Detail::Funded { outputs, .. } => outputs.iter().map(|o| (o.receiver, o.zats)).collect(),
        other => panic!("expected a funded detail, got {other:?}"),
    }
}

#[test]
fn funds_an_address_nobody_declared() {
    let dispatcher = dispatcher();
    let detail = fund(&dispatcher, &undeclared_ua(), "12", None);
    // Ironwood is active here, and it is the newest receiver the address
    // carries, so a bare fund picks it.
    assert_eq!(outputs(&detail), vec![('i', 12 * ZEC)]);
}

#[test]
fn splits_equally_across_the_receivers_asked_for() {
    let dispatcher = dispatcher();
    let detail = fund(&dispatcher, &undeclared_ua(), "12", Some("os"));
    assert_eq!(outputs(&detail), vec![('o', 6 * ZEC), ('s', 6 * ZEC)]);
}

#[test]
fn pays_every_receiver_of_a_unified_address() {
    let dispatcher = dispatcher();
    let detail = fund(&dispatcher, &undeclared_ua(), "12", Some("tsoi"));
    assert_eq!(
        outputs(&detail),
        vec![
            ('t', 3 * ZEC),
            ('s', 3 * ZEC),
            ('o', 3 * ZEC),
            ('i', 3 * ZEC)
        ]
    );
}

#[test]
fn the_remainder_lands_on_the_first_receiver_typed() {
    let ua = undeclared_ua();

    let first = dispatcher();
    assert_eq!(
        outputs(&fund(&first, &ua, "0.00000001", Some("os"))),
        vec![('o', 1), ('s', 0)]
    );

    let second = dispatcher();
    assert_eq!(
        outputs(&fund(&second, &ua, "0.00000001", Some("so"))),
        vec![('s', 1), ('o', 0)]
    );
}

#[test]
fn an_inactive_pool_is_skipped_and_its_share_re_split() {
    let mut pre_ironwood = ChainParams::regtest();
    pre_ironwood
        .network
        .apply_overrides("all=1, nu6.3=off")
        .expect("regtest schedule accepts turning ironwood off");
    let boot = BootChain::new(pre_ironwood, Seed::from(0));
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    let dispatcher = Dispatcher::new(Darkside::new(boot.build()), miner, boot, 10_000);

    let outcome = dispatcher
        .run(Command::Fund {
            address: undeclared_ua(),
            zats: 12 * ZEC,
            receivers: Some(parse_receivers("osi").expect("valid set")),
        })
        .expect("the surviving receivers still fund");

    // The amount asked for is the amount that lands, split over what is
    // left rather than losing ironwood's third.
    assert_eq!(
        outputs(&outcome.detail),
        vec![('o', 6 * ZEC), ('s', 6 * ZEC)]
    );
    assert_eq!(outcome.warnings.len(), 1);
    assert!(outcome.warnings[0].contains('i'), "{:?}", outcome.warnings);
}

#[test]
fn funding_fails_when_no_requested_receiver_survives() {
    let mut pre_ironwood = ChainParams::regtest();
    pre_ironwood
        .network
        .apply_overrides("all=1, nu6.3=off")
        .expect("regtest schedule accepts turning ironwood off");
    let boot = BootChain::new(pre_ironwood, Seed::from(0));
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    let dispatcher = Dispatcher::new(Darkside::new(boot.build()), miner, boot, 10_000);

    let failure = dispatcher.run(Command::Fund {
        address: undeclared_ua(),
        zats: ZEC,
        receivers: Some(vec![Receiver::Ironwood]),
    });
    assert!(
        failure.is_err(),
        "a fund that pays nobody must not report ok"
    );
}

#[test]
fn advance_mines_to_a_named_activation() {
    let mut schedule = ChainParams::regtest();
    schedule
        .network
        .apply_overrides("all=1, nu6.3=90")
        .expect("regtest schedule accepts explicit heights");
    let boot = BootChain::new(schedule, Seed::from(0));
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    let dispatcher = Dispatcher::new(Darkside::new(boot.build()), miner, boot, 10_000);

    let outcome = dispatcher
        .run(Command::Advance {
            upgrade: "ironwood".to_owned(),
        })
        .expect("ironwood is scheduled above the tip");
    match outcome.detail {
        Detail::Mined { tip, .. } => assert_eq!(tip, 90),
        other => panic!("expected a mined detail, got {other:?}"),
    }

    // Mining backwards is not a thing, so asking again has to fail rather
    // than quietly do nothing.
    assert!(
        dispatcher
            .run(Command::Advance {
                upgrade: "ironwood".to_owned(),
            })
            .is_err()
    );

    // An upgrade this chain never schedules is a different failure from one
    // already passed, and both are refusals.
    assert!(
        dispatcher
            .run(Command::Advance {
                upgrade: "banana".to_owned(),
            })
            .is_err()
    );
}

#[test]
fn advance_refuses_an_upgrade_the_chain_never_schedules() {
    let mut schedule = ChainParams::regtest();
    schedule
        .network
        .apply_overrides("all=1, nu6.3=off")
        .expect("regtest schedule accepts turning ironwood off");
    let boot = BootChain::new(schedule, Seed::from(0));
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    let dispatcher = Dispatcher::new(Darkside::new(boot.build()), miner, boot, 10_000);

    assert!(
        dispatcher
            .run(Command::Advance {
                upgrade: "ironwood".to_owned(),
            })
            .is_err()
    );
}

/// A dispatcher over a regtest chain whose ironwood sits `distance` blocks
/// above the boot height, capped at `max_blocks` per command.
fn far_ironwood(distance: u32, max_blocks: u32) -> Dispatcher {
    let mut schedule = ChainParams::regtest();
    schedule
        .network
        .apply_overrides(&format!("all=1, nu6.3={}", distance + 1))
        .expect("regtest schedule accepts explicit heights");
    let boot = BootChain::new(schedule, Seed::from(0));
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    Dispatcher::new(Darkside::new(boot.build()), miner, boot, max_blocks)
}

#[test]
fn a_span_too_long_to_mine_is_computed_on_demand() {
    // Ironwood far above the cap. Mining would store a block per height.
    let dispatcher = far_ironwood(500_000, 10);
    let outcome = dispatcher
        .run(Command::Advance {
            upgrade: "ironwood".to_owned(),
        })
        .expect("a distant upgrade is jumped rather than mined");

    match outcome.detail {
        Detail::Jumped { from, tip } => {
            assert_eq!(from, 0);
            assert_eq!(tip, 500_001);
        }
        other => panic!("expected a jump, got {other:?}"),
    }

    dispatcher.darkside().with_chain(|chain| {
        // Two stored blocks, boot and target, not half a million.
        assert_eq!(chain.blocks().len(), 2);
        assert!(chain.params().ironwood_active(chain.tip_height()));

        // The span serves empty blocks that chain to each other, across
        // both seams: out of the real boot block, and back into the real
        // target block.
        for height in [1, 2, 250_000, 500_000, 500_001] {
            let block = chain
                .block_at(height)
                .unwrap_or_else(|| panic!("no block served at {height}"));
            let below = chain
                .block_at(height - 1)
                .unwrap_or_else(|| panic!("no block served at {}", height - 1));
            assert_eq!(
                block.prev_hash,
                below.hash,
                "continuity broke between {} and {height}",
                height - 1
            );
        }
        assert!(chain.block_at(500_002).is_none(), "served above the tip");
    });
}

#[test]
fn a_jump_keeps_everything_already_mined() {
    let dispatcher = far_ironwood(500_000, 10);
    let funded = match fund(&dispatcher, &undeclared_ua(), "10", Some("o")) {
        Detail::Funded { height, .. } => height,
        other => panic!("expected a funded detail, got {other:?}"),
    };
    let (tree_before, size_before) = dispatcher.darkside().with_chain(|c| {
        let states = c.tree_states(funded).expect("tree state at the fund");
        (states.orchard.clone(), states.orchard_size)
    });

    dispatcher
        .run(Command::Advance {
            upgrade: "ironwood".to_owned(),
        })
        .expect("a fund below the span does not block a jump");

    dispatcher.darkside().with_chain(|chain| {
        // The fund is untouched: same height, still carrying its
        // transaction beyond the coinbase.
        let block = chain.block(funded).expect("the funded block is still here");
        assert!(block.txs.len() > 1, "the fund left its block");

        // The trees carry through the span unchanged, since empty blocks
        // append no commitments. A wallet anchoring anywhere in the span
        // gets the state as of the last real block.
        let after = chain
            .tree_states(chain.tip_height())
            .expect("tree state at the new tip");
        assert_eq!(after.orchard, tree_before);
        assert_eq!(after.orchard_size, size_before);
        let mid = chain
            .tree_states(250_000)
            .expect("tree state inside the span");
        assert_eq!(mid.orchard, tree_before);
    });
}

#[test]
fn a_jump_over_scheduled_work_is_refused() {
    let dispatcher = far_ironwood(500_000, 10);
    // Schedule a fund far above the tip but below the jump target, so
    // jumping past it would mean it never fires.
    dispatcher
        .darkside()
        .with_chain_mut(|chain| {
            chain.fund(FundSpec {
                recipient: Recipient::Literal(undeclared_ua()),
                pool: Some(darkside_chain::Pool::Orchard),
                zats: ZEC,
                outputs: 1,
                at: 400_000,
                via_coinbase: false,
                corruption: None,
            })
        })
        .expect("scheduling a fund above the tip");

    let refused = dispatcher.run(Command::Advance {
        upgrade: "ironwood".to_owned(),
    });
    let Err(e) = refused else {
        panic!("a jump must not skip past work that would then never fire");
    };
    assert!(e.to_string().contains("400000"), "{e}");
}

#[test]
fn a_span_within_the_cap_is_mined_rather_than_jumped() {
    let dispatcher = far_ironwood(50, 10_000);
    let outcome = dispatcher
        .run(Command::Advance {
            upgrade: "ironwood".to_owned(),
        })
        .expect("50 blocks is well inside the cap");
    // Mining keeps continuity, so a short hop must not rewrite history.
    assert!(
        matches!(outcome.detail, Detail::Mined { tip: 51, .. }),
        "{:?}",
        outcome.detail
    );
    assert!(outcome.warnings.is_empty());
}

#[test]
fn mining_beyond_the_cap_is_refused() {
    let boot = BootChain::new(params(), Seed::from(0));
    let (miner, _rx) = watch::channel(MinerControl {
        paused: true,
        tick: Tick::fixed(3600),
    });
    let dispatcher = Dispatcher::new(Darkside::new(boot.build()), miner, boot, 10);

    assert!(dispatcher.run(Command::Mine { blocks: 10 }).is_ok());
    assert!(dispatcher.run(Command::Mine { blocks: 11 }).is_err());
}
