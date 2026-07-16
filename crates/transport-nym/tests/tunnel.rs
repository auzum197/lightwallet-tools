//! Offline validation of the connector: the test-support SOCKS5 server
//! stands in for `nym-socks5-client`, bridging to the test-support mock
//! lightwalletd over loopback TCP. This covers everything the crate itself
//! claims (the handshake, domain addressing, gRPC through the tunnel, lazy
//! connection, error surfacing) with no processes and no network. What it
//! cannot cover, mixnet latency, stays with `tests/live.rs` and the
//! promotion gate.
//!
//! The endpoint hostname resolves nowhere on purpose. The sync loop can only
//! succeed if the connector handed it to the proxy verbatim instead of
//! resolving it locally, and the recorded CONNECT request pins the address
//! type byte explicitly.

use futures_util::StreamExt;
use lightwallet_core::{
    CanonicalIndexer, CompactBlockHeader, NetworkParams, TestnetIndexer, assert_continuity,
};
use lightwallet_proto_canonical::compact_tx_streamer_server::CompactTxStreamerServer;
use lightwallet_test_support::canonical::{MockStreamer, linked_blocks};
use lightwallet_test_support::socks5::{REPLY_REFUSED, REPLY_SUCCEEDED, spawn_socks5};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use tokio::net::TcpListener;
use tonic::transport::{Channel, Endpoint};

const ENDPOINT: &str = "http://lightwalletd.test:9067";

/// The test-support mock served over real loopback TCP instead of its usual
/// duplex pipe, so the SOCKS5 mock has something to splice to.
async fn serve_mock_tcp(mock: MockStreamer) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let incoming = futures_util::stream::unfold(listener, |listener| async move {
        let conn = listener.accept().await.map(|(stream, _)| stream);
        Some((conn, listener))
    });
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(CompactTxStreamerServer::new(mock))
            .serve_with_incoming(incoming),
    );
    addr
}

fn params() -> NetworkParams {
    NetworkParams {
        chain_name: "mock".into(),
        activation_heights: BTreeMap::new(),
        consensus_branch_id: 0,
    }
}

#[tokio::test]
async fn grpc_flows_through_the_tunnel() {
    let upstream = serve_mock_tcp(MockStreamer::new().with_blocks(linked_blocks(100, 10))).await;
    let proxy = spawn_socks5(Some(upstream), REPLY_SUCCEEDED).await;

    let endpoint = Endpoint::from_static(ENDPOINT);
    let channel = lightwallet_transport_nym::channel(&endpoint, proxy.addr)
        .await
        .expect("channel through the mock proxy");
    let indexer = CanonicalIndexer::new(channel, params());

    let tip = indexer.get_latest_height().await.unwrap();
    assert_eq!(tip, 109);

    let mut blocks = indexer.get_block_range(100, tip).await.unwrap();
    let mut prev = None;
    let mut seen = 0;
    while let Some(block) = blocks.next().await {
        let block = block.unwrap();
        block.block_hash().unwrap();
        if let Some(prev) = &prev {
            assert!(assert_continuity::<CanonicalIndexer<Channel>>(prev, &block));
        }
        prev = Some(block);
        seen += 1;
    }
    assert_eq!(seen, 10);

    let connects = proxy.connects.lock().unwrap();
    assert_eq!(connects.len(), 1);
    assert_eq!(
        connects[0].atyp, 0x03,
        "hostname must reach the proxy as a domain address, not resolve locally"
    );
    assert_eq!(connects[0].host, "lightwalletd.test");
    assert_eq!(connects[0].port, 9067);
}

#[tokio::test]
async fn channel_lazy_connects_on_first_use() {
    let upstream = serve_mock_tcp(MockStreamer::new().with_blocks(linked_blocks(100, 10))).await;
    let proxy = spawn_socks5(Some(upstream), REPLY_SUCCEEDED).await;

    let endpoint = Endpoint::from_static(ENDPOINT);
    let channel = lightwallet_transport_nym::channel_lazy(&endpoint, proxy.addr);
    assert_eq!(proxy.accepted.load(Ordering::SeqCst), 0);

    let indexer = CanonicalIndexer::new(channel, params());
    assert_eq!(indexer.get_latest_height().await.unwrap(), 109);
    assert_eq!(proxy.accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ip_literal_endpoint_connects_by_address() {
    let upstream = serve_mock_tcp(MockStreamer::new().with_blocks(linked_blocks(100, 1))).await;
    let proxy = spawn_socks5(Some(upstream), REPLY_SUCCEEDED).await;

    let endpoint = Endpoint::from_static("http://127.0.0.1:9067");
    let channel = lightwallet_transport_nym::channel(&endpoint, proxy.addr)
        .await
        .unwrap();
    let indexer = CanonicalIndexer::new(channel, params());
    assert_eq!(indexer.get_latest_height().await.unwrap(), 100);

    let connects = proxy.connects.lock().unwrap();
    assert_eq!(
        connects[0].atyp, 0x01,
        "an IP-literal endpoint must skip domain addressing"
    );
    assert_eq!(connects[0].host, "127.0.0.1");
    assert_eq!(connects[0].port, 9067);
}

#[tokio::test]
async fn ipv6_literal_reaches_the_proxy_as_a_domain() {
    let upstream = serve_mock_tcp(MockStreamer::new().with_blocks(linked_blocks(100, 1))).await;
    let proxy = spawn_socks5(Some(upstream), REPLY_SUCCEEDED).await;

    let endpoint = Endpoint::from_static("http://[::1]:9067");
    let channel = lightwallet_transport_nym::channel(&endpoint, proxy.addr)
        .await
        .unwrap();
    let indexer = CanonicalIndexer::new(channel, params());
    assert_eq!(indexer.get_latest_height().await.unwrap(), 100);

    // The brackets survive `Uri::host()` and fail tokio-socks' IpAddr parse,
    // so the literal leaves as a domain address a real requester would try
    // to resolve. Pinned as documentation of the leak, not as endorsement.
    let connects = proxy.connects.lock().unwrap();
    assert_eq!(connects[0].atyp, 0x03);
    assert_eq!(connects[0].host, "[::1]");
}

#[tokio::test]
async fn proxy_refusal_surfaces_as_an_error() {
    let proxy = spawn_socks5(None, REPLY_REFUSED).await;

    let endpoint = Endpoint::from_static(ENDPOINT);
    lightwallet_transport_nym::channel(&endpoint, proxy.addr)
        .await
        .expect_err("a SOCKS5 refusal must fail the connect");
}
