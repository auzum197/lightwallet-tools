//! Parser obligations, plus a build of the spec's own example.

use darkside_decl::{Barrier, Declaration, Expectation, Step};

const HEADER: &str = "network regtest\nseed 0xC0FFEE\n";

fn with_accounts(body: &str) -> String {
    format!(
        "{HEADER}account alice seed \"alice-test-seed\"\naccount bob seed \"bob-test-seed\"\n{body}"
    )
}

#[test]
fn the_spec_example_parses_and_builds() {
    let src = with_accounts(
        r#"
chain main {
  blocks 0..5
  fund alice 10 ZEC at 5              # shielded, default pool
  fund alice.taddr 2 ZEC at 5         # transparent
  fund bob orchard 3 ZEC at 6         # explicit pool
  send alice 2 ZEC to bob at 7        # darkside-authored spend
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
"#,
    );
    let decl = Declaration::parse(&src).expect("the spec example parses");
    assert_eq!(decl.chains.len(), 2);
    assert_eq!(decl.chains[0].tip, 10);
    assert_eq!(decl.chains[1].tip, 12);
    assert_eq!(decl.scenarios.len(), 1);
    assert!(matches!(
        decl.scenarios[0].steps[1],
        Step::Wait(Barrier::BlockRequested(10))
    ));
    assert!(matches!(
        decl.scenarios[0].steps[3],
        Step::Expect(Expectation::Tip(12))
    ));

    let world = decl.build().expect("the example builds");
    let main = world.chain("main").expect("main built");
    assert_eq!(main.tip_height(), 10);
    assert_eq!(main.expected_balance("alice").unwrap(), 10 * 100_000_000);
    assert_eq!(
        main.expected_transparent_balance("alice").unwrap(),
        2 * 100_000_000
    );
    assert_eq!(main.expected_balance("bob").unwrap(), 5 * 100_000_000);

    let fork = world.chain("fork").expect("fork built");
    assert_eq!(fork.tip_height(), 12);
    assert_eq!(fork.block(8).unwrap().hash.0, main.block(8).unwrap().hash.0);
    assert_ne!(fork.block(9).unwrap().hash.0, main.block(9).unwrap().hash.0);
    // The fork point predates the send at 7... shares it; balances agree.
    assert_eq!(fork.expected_balance("bob").unwrap(), 5 * 100_000_000);
}

#[test]
fn non_observable_barriers_are_rejected() {
    let src = with_accounts(
        "chain main {
  blocks 0..3
}\nscenario s {\n  wait alice.balance == 5\n}\n",
    );
    let e = Declaration::parse(&src).expect_err("wallet-internal barrier");
    assert!(e.to_string().contains("unrepresentable"), "{e}");
}

#[test]
fn fork_above_parent_tip_is_rejected() {
    let src = with_accounts(
        "chain main {
  blocks 0..5
}\nchain f from main@8 {
  blocks 8..9
}\n",
    );
    let e = Declaration::parse(&src).expect_err("fork above tip");
    assert!(e.to_string().contains("above"), "{e}");
}

#[test]
fn undeclared_accounts_are_rejected() {
    let src = format!("{HEADER}chain main {{\n  fund carol 1 ZEC at 2\n}}\n");
    let e = Declaration::parse(&src).expect_err("undeclared recipient");
    assert!(e.to_string().contains("not declared"), "{e}");

    let src = with_accounts("chain main {\n  send carol 1 ZEC to alice at 2\n}\n");
    let e = Declaration::parse(&src).expect_err("undeclared sender");
    assert!(e.to_string().contains("not declared"), "{e}");
}

#[test]
fn literal_recipients_need_no_declaration() {
    let src = with_accounts(
        "chain main {\n  send alice 1 ZEC to \"u1unknownexternal\" at 3\n  fund alice 2 ZEC at 2\n}\n",
    );
    Declaration::parse(&src).expect("literal recipient parses");
}

#[test]
fn overdraw_fails_while_building() {
    let src =
        with_accounts("chain main {\n  fund alice 1 ZEC at 2\n  send alice 5 ZEC to bob at 4\n}\n");
    let decl = Declaration::parse(&src).expect("parses fine");
    let e = match decl.build() {
        Err(e) => e,
        Ok(_) => panic!("overdraw must surface at build"),
    };
    assert!(e.to_string().contains("insufficient funds"), "{e}");
}

#[test]
fn pending_and_expiry_ordering_is_enforced() {
    let src = with_accounts("chain main {\n  send alice 1 ZEC to bob at 5 pending from 6\n}\n");
    assert!(Declaration::parse(&src).is_err());

    let src =
        with_accounts("chain main {\n  send alice 1 ZEC to bob pending from 5 expiring at 4\n}\n");
    assert!(Declaration::parse(&src).is_err());

    let src = with_accounts("chain main {\n  send alice 1 ZEC to bob\n}\n");
    assert!(Declaration::parse(&src).is_err(), "needs at or expiring");
}

#[test]
fn corruption_vocabulary_is_closed() {
    let src = with_accounts("chain main {\n  send alice 1 ZEC to bob at 3 corrupt entropy\n}\n");
    let e = Declaration::parse(&src).expect_err("unknown corruption word");
    assert!(e.to_string().contains("vocabulary"), "{e}");

    let src = with_accounts(
        "chain main {\n  fund alice 2 ZEC at 2\n  send alice 1 ZEC to bob at 4 corrupt commitment\n}\n",
    );
    Declaration::parse(&src).expect("the named set parses");
}

#[test]
fn pool_before_activation_is_rejected() {
    let src = "network regtest\nseed 1\nactivation sapling@1 nu5@10\naccount alice seed \"a\"\nchain main {\n  fund alice 1 ZEC at 5\n}\n".to_string();
    let e = Declaration::parse(&src).expect_err("orchard fund before nu5");
    assert!(e.to_string().contains("activation"), "{e}");

    let src = "network regtest\nseed 1\nactivation sapling@1 nu5@10\naccount alice seed \"a\"\nchain main {\n  fund alice sapling 1 ZEC at 5\n}\n".to_string();
    Declaration::parse(&src).expect("sapling fund after sapling activation");
}

#[test]
fn activation_ordering_is_enforced() {
    let src = format!("{HEADER}activation sapling@5 nu5@2\nchain main {{ blocks 0..3 }}\n");
    let e = Declaration::parse(&src).expect_err("decreasing activations");
    assert!(e.to_string().contains("non-decreasing"), "{e}");
}

#[test]
fn scenarios_referencing_undeclared_chains_are_rejected() {
    let src = with_accounts(
        "chain main {
  blocks 0..3
}\nscenario s {\n  serve ghost\n}\n",
    );
    let e = Declaration::parse(&src).expect_err("undeclared chain");
    assert!(e.to_string().contains("undeclared chain"), "{e}");
}

#[test]
fn network_and_seed_lines_are_required() {
    assert!(
        Declaration::parse(
            "seed 1\nchain main {
  blocks 0..2
}\n"
        )
        .is_err()
    );
    assert!(
        Declaration::parse(
            "network regtest\nchain main {
  blocks 0..2
}\n"
        )
        .is_err()
    );
}

#[test]
fn network_line_selects_encoding_and_activation_rides_it() {
    use darkside_chain::SyntheticNetwork;
    use zcash_protocol::consensus::NetworkUpgrade;

    // A bare `network test` inherits the real testnet schedule under testnet
    // encoding (regtest would put Sapling at height 1).
    let decl = Declaration::parse("network test\nseed 1\n").expect("test encoding parses");
    assert!(matches!(decl.params.network, SyntheticNetwork::Test(_)));
    assert_eq!(
        decl.params.activation(NetworkUpgrade::Sapling),
        Some(280_000)
    );

    // An activation line collapses the schedule while keeping the encoding.
    let decl = Declaration::parse("network test\nseed 1\nactivation sapling@1 nu5@1 nu6@1\n")
        .expect("collapsed testnet parses");
    assert!(matches!(decl.params.network, SyntheticNetwork::Test(_)));
    assert_eq!(decl.params.activation(NetworkUpgrade::Sapling), Some(1));
    assert_eq!(decl.params.activation(NetworkUpgrade::Nu6_1), None);

    assert!(Declaration::parse("network mainnet\nseed 1\n").is_err());
}

#[test]
fn ironwood_requires_nu6_3() {
    let src = "network regtest\nseed 1\nactivation sapling@1 nu6@1\naccount alice seed \"a\"\nchain main {\n  fund alice ironwood 1 ZEC at 3\n}\n".to_string();
    assert!(Declaration::parse(&src).is_err());

    let src = "network regtest\nseed 1\nactivation sapling@1 ironwood@1\naccount alice seed \"a\"\nchain main {\n  fund alice ironwood 1 ZEC at 3\n}\n".to_string();
    let decl = Declaration::parse(&src).expect("ironwood alias activates nu6.3");
    let world = decl.build().expect("builds");
    assert_eq!(
        world
            .chain("main")
            .unwrap()
            .expected_balance_in("alice", darkside_chain::Pool::Ironwood)
            .unwrap(),
        100_000_000
    );
}

#[test]
fn replay_boundary_keeps_future_content_scheduled() {
    let src = with_accounts("chain main {\n  blocks 0..10\n  fund alice 5 ZEC at 8\n}\n");
    let decl = Declaration::parse(&src).expect("parses");
    // Built only to height 4: the fund at 8 has not happened.
    let mut chain = decl.build_chain("main", Some(4)).expect("partial build");
    assert_eq!(chain.tip_height(), 4);
    assert_eq!(chain.expected_balance("alice").unwrap(), 0);
    // A driver mining onward reaches the scheduled content.
    chain.mine(6).expect("mining onward");
    assert_eq!(chain.expected_balance("alice").unwrap(), 5 * 100_000_000);
}

#[test]
fn decimal_amounts_parse_to_zatoshis() {
    let src =
        with_accounts("chain main {\n  fund alice 0.5 ZEC at 2\n  fund bob 25000 zats at 3\n}\n");
    let decl = Declaration::parse(&src).expect("parses");
    let world = decl.build().expect("builds");
    let main = world.chain("main").unwrap();
    assert_eq!(main.expected_balance("alice").unwrap(), 50_000_000);
    assert_eq!(main.expected_balance("bob").unwrap(), 25_000);
}
