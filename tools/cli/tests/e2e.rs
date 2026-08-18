//! The binary against real gRPC servers on localhost: the test-support mocks
//! served over TCP instead of their usual duplex pipe, with `lwcli` driven as
//! a subprocess. Covers JSON rendering, display-order hashes, NDJSON streams,
//! stdin input, exit codes, and the explicit-variant rule.

use lightwallet_proto_canonical::compact_tx_streamer_server::CompactTxStreamerServer as CanonicalServer;
use lightwallet_proto_crosslink::compact_tx_streamer_server::CompactTxStreamerServer as CrosslinkServer;
use lightwallet_test_support::{Rpc, canonical, crosslink, mock_hash};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::process::{Command, Output, Stdio};
use tonic::Status;

async fn serve_canonical_addr(mock: canonical::MockStreamer) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = futures_util::stream::unfold(listener, |listener| async {
        Some((listener.accept().await.map(|(socket, _)| socket), listener))
    });
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(CanonicalServer::new(mock))
            .serve_with_incoming(incoming),
    );
    addr
}

async fn serve_canonical(mock: canonical::MockStreamer) -> String {
    format!("http://{}", serve_canonical_addr(mock).await)
}

async fn serve_crosslink(mock: crosslink::MockStreamer) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = futures_util::stream::unfold(listener, |listener| async {
        Some((listener.accept().await.map(|(socket, _)| socket), listener))
    });
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(CrosslinkServer::new(mock))
            .serve_with_incoming(incoming),
    );
    format!("http://{addr}")
}

fn lwcli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lwcli"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout_json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "lwcli failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn display_hex(wire: [u8; 32]) -> String {
    hex::encode(wire.iter().rev().copied().collect::<Vec<_>>())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lightd_info_renders_as_json() {
    let info = lightwallet_proto_canonical::LightdInfo {
        chain_name: "test".into(),
        block_height: 42,
        ..Default::default()
    };
    let url = serve_canonical(canonical::MockStreamer::new().with_lightd_info(info)).await;

    let rendered = stdout_json(&lwcli(&["--url", &url, "get-lightd-info"]));
    assert_eq!(rendered["chainName"], "test");
    assert_eq!(rendered["blockHeight"], 42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_hashes_print_in_display_order() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(100, 3));
    let url = serve_canonical(mock).await;

    let block = stdout_json(&lwcli(&["--url", &url, "get-block", "101"]));
    assert_eq!(block["height"], 101);
    assert_eq!(block["hash"], display_hex(mock_hash(101)).as_str());
    assert_eq!(block["prevHash"], display_hex(mock_hash(100)).as_str());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_range_streams_ndjson() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(100, 3));
    let url = serve_canonical(mock).await;

    let output = lwcli(&["--url", &url, "get-block-range", "100", "102"]);
    assert!(output.status.success());
    let heights: Vec<u64> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["height"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(heights, [100, 101, 102]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_transaction_reads_hex_from_stdin() {
    let mock = canonical::MockStreamer::new();
    let inbox = mock.clone();
    let url = serve_canonical(mock).await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_lwcli"))
        .args(["--url", &url, "send-transaction", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"deadbeef\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    assert_eq!(inbox.sent()[0].data, vec![0xde, 0xad, 0xbe, 0xef]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_failure_exits_nonzero_with_the_status() {
    let url =
        serve_canonical(canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(1, 1)))
            .await;

    let output = lwcli(&["--url", &url, "get-block", "999"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no block at height 999"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crosslink_commands_require_the_explicit_variant() {
    let mock = crosslink::MockStreamer::new().with_roster(vec![1, 2, 3]);
    let url = serve_crosslink(mock).await;

    let output = lwcli(&["--url", &url, "get-roster"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("only on the crosslink variant"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let roster = stdout_json(&lwcli(&[
        "--url",
        &url,
        "--variant",
        "crosslink",
        "get-roster",
    ]));
    assert_eq!(roster["data"], "010203");
}

#[test]
fn explicit_canonical_rejects_crosslink_commands() {
    let output = lwcli(&["--variant", "canonical", "get-roster"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("only on the crosslink variant"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The endpoint hostname resolves nowhere on purpose: reaching the mock at
/// all proves the binary handed the name to the proxy instead of resolving
/// it locally, and the recorded CONNECT pins the address type byte.
#[cfg(feature = "nym")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nym_transport_tunnels_through_the_proxy() {
    use lightwallet_test_support::socks5::{REPLY_SUCCEEDED, spawn_socks5};

    let info = lightwallet_proto_canonical::LightdInfo {
        chain_name: "tunneled".into(),
        ..Default::default()
    };
    let upstream =
        serve_canonical_addr(canonical::MockStreamer::new().with_lightd_info(info)).await;
    let proxy = spawn_socks5(Some(upstream), REPLY_SUCCEEDED).await;

    let rendered = stdout_json(&lwcli(&[
        "--url",
        "http://lightwalletd.test:9067",
        "--transport",
        "nym",
        "--nym-socks5",
        &proxy.addr.to_string(),
        "get-lightd-info",
    ]));
    assert_eq!(rendered["chainName"], "tunneled");

    let connects = proxy.connects.lock().unwrap();
    assert_eq!(connects.len(), 1);
    assert_eq!(
        connects[0].atyp, 0x03,
        "hostname must reach the proxy as a domain address, not resolve locally"
    );
    assert_eq!(connects[0].host, "lightwalletd.test");
    assert_eq!(connects[0].port, 9067);
}

#[cfg(feature = "nym")]
#[test]
fn nym_socks5_alone_does_not_select_the_transport() {
    // The address without an explicit --transport nym is an error, not a
    // silent switch. The explicit nym path is covered by the tunnel test.
    let output = lwcli(&[
        "--url",
        "http://lightwalletd.test:9067",
        "--nym-socks5",
        "127.0.0.1:9060",
        "get-latest-height",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not select"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(feature = "nym")]
#[test]
fn nym_socks5_contradicts_an_explicit_transport() {
    let output = lwcli(&[
        "--transport",
        "direct",
        "--nym-socks5",
        "127.0.0.1:1080",
        "get-latest-height",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("applies only to the nym transport"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_transaction_round_trips_display_order() {
    let mut wire_txid = [0u8; 32];
    wire_txid[0] = 0xff;
    let tx = lightwallet_proto_canonical::RawTransaction {
        data: vec![0xab, 0xcd],
        height: 7,
    };
    let mock = canonical::MockStreamer::new().with_transaction(wire_txid.to_vec(), tx);
    let url = serve_canonical(mock).await;

    let rendered = stdout_json(&lwcli(&[
        "--url",
        &url,
        "get-transaction",
        &display_hex(wire_txid),
    ]));
    assert_eq!(rendered["data"], "abcd");
    assert_eq!(rendered["height"], 7);

    let unknown = lwcli(&["--url", &url, "get-transaction", &"00".repeat(32)]);
    assert!(!unknown.status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_states_render_at_height_and_at_the_tip() {
    let mock = canonical::MockStreamer::new()
        .with_tree_state(lightwallet_proto_canonical::TreeState {
            height: 100,
            ..Default::default()
        })
        .with_tree_state(lightwallet_proto_canonical::TreeState {
            height: 200,
            ..Default::default()
        });
    let url = serve_canonical(mock).await;

    assert_eq!(
        stdout_json(&lwcli(&["--url", &url, "get-tree-state", "100"]))["height"],
        100
    );
    assert_eq!(
        stdout_json(&lwcli(&["--url", &url, "get-latest-tree-state"]))["height"],
        200
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retryable_failures_say_so_on_stderr() {
    let mock =
        canonical::MockStreamer::new().with_fault(Rpc::GetLightdInfo, Status::unavailable("down"));
    let url = serve_canonical(mock).await;

    let output = lwcli(&["--url", &url, "get-lightd-info"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("(retryable)"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_crosslink_variant_drives_shared_commands() {
    let mock = crosslink::MockStreamer::new().with_blocks(crosslink::linked_blocks(50, 1));
    let url = serve_crosslink(mock).await;

    let block = stdout_json(&lwcli(&[
        "--url",
        &url,
        "--variant",
        "crosslink",
        "get-block",
        "50",
    ]));
    assert_eq!(block["height"], 50);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bond_info_takes_wire_order_hex() {
    let mock = crosslink::MockStreamer::new().with_bond(
        vec![0x11; 32],
        lightwallet_proto_crosslink::BondInfoResponse {
            amount: 7_000,
            status: 2,
        },
    );
    let url = serve_crosslink(mock).await;

    let info = stdout_json(&lwcli(&[
        "--url",
        &url,
        "--variant",
        "crosslink",
        "get-bond-info",
        &"11".repeat(32),
    ]));
    assert_eq!(info["amount"], 7000);
    assert_eq!(info["status"], 2);

    let bad = lwcli(&[
        "--url",
        &url,
        "--variant",
        "crosslink",
        "get-bond-info",
        "zz",
    ]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not valid hex"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn faucet_donation_renders_the_amount() {
    let mock = crosslink::MockStreamer::new().with_faucet_amount(250_000);
    let url = serve_crosslink(mock).await;

    let donation = stdout_json(&lwcli(&[
        "--url",
        &url,
        "--variant",
        "crosslink",
        "request-faucet-donation",
        "u1mock",
    ]));
    assert_eq!(donation["amount"], 250_000);
}

#[test]
fn completions_print_without_a_url() {
    let output = lwcli(&["completions", "bash"]);
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
}

#[test]
fn a_missing_url_is_a_clear_error() {
    let output = lwcli(&["get-latest-height"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--url is required"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_mode_streams_typed_lines() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 3));
    let url = serve_canonical(mock).await;

    let output = lwcli(&[
        "--url",
        &url,
        "--output",
        "debug",
        "get-block-range",
        "0",
        "2",
    ]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 3);
    assert!(
        stdout
            .lines()
            .all(|line| line.starts_with("CompactBlock {"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_pipe_ends_the_stream_quietly() {
    let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(0, 2000));
    let url = serve_canonical(mock).await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_lwcli"))
        .args(["--url", &url, "get-block-range", "0", "1999"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 1];
    stdout.read_exact(&mut first).unwrap();
    drop(stdout);

    let status = child.wait().unwrap();
    assert!(status.success(), "a broken pipe must exit 0, not panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mempool_txs_stream_with_excludes() {
    let tx = |last: u8| lightwallet_proto_canonical::CompactTx {
        txid: {
            let mut txid = vec![0u8; 32];
            txid[31] = last;
            txid
        },
        ..Default::default()
    };
    let mock = canonical::MockStreamer::new().with_mempool_txs([tx(1), tx(2)]);
    let url = serve_canonical(mock).await;

    let output = lwcli(&["--url", &url, "get-mempool-tx", "--exclude", "01"]);
    assert!(output.status.success());
    let lines: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(lines.len(), 1);
    let remaining: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    // txid prints in display order, so the wire-order suffix byte leads.
    assert!(remaining["txid"].as_str().unwrap().starts_with("02"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn address_utxos_render_as_a_list() {
    let utxo = |height: u64| lightwallet_proto_canonical::GetAddressUtxosReply {
        address: "t1known".into(),
        height,
        ..Default::default()
    };
    let mock = canonical::MockStreamer::new().with_utxos([utxo(1), utxo(2)]);
    let url = serve_canonical(mock).await;

    let rendered = stdout_json(&lwcli(&["--url", &url, "get-address-utxos", "t1known"]));
    assert_eq!(rendered["addressUtxos"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subtree_roots_take_the_protocol_flag() {
    let mock = canonical::MockStreamer::new().with_subtree_roots([
        lightwallet_proto_canonical::SubtreeRoot {
            completing_block_height: 42,
            ..Default::default()
        },
    ]);
    let url = serve_canonical(mock).await;

    let output = lwcli(&["--url", &url, "get-subtree-roots", "--protocol", "orchard"]);
    assert!(output.status.success());
    let root: serde_json::Value =
        serde_json::from_str(String::from_utf8(output.stdout).unwrap().trim()).unwrap();
    assert_eq!(root["completingBlockHeight"], 42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclude_arguments_must_be_hex() {
    let url = serve_canonical(canonical::MockStreamer::new()).await;

    let output = lwcli(&["--url", &url, "get-mempool-tx", "--exclude", "zz"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--exclude takes hex"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_output_prints_the_typed_struct() {
    let url =
        serve_canonical(canonical::MockStreamer::new().with_lightd_info(Default::default())).await;

    let output = lwcli(&["--url", &url, "--output", "debug", "get-lightd-info"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("LightdInfo {"));
}
