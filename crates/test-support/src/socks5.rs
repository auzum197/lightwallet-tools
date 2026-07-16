//! An in-process SOCKS5 server standing in for `nym-socks5-client`. It speaks
//! the server side of RFC 1928 (no-auth greeting, CONNECT), records each
//! CONNECT target, then splices the connection to `upstream` regardless of
//! the requested target. That splice is what lets tests use an unresolvable
//! endpoint hostname: the request only succeeds if the connector handed the
//! name to the proxy verbatim instead of resolving it locally.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// RFC 1928 reply code: request granted.
pub const REPLY_SUCCEEDED: u8 = 0x00;
/// RFC 1928 reply code: connection refused.
pub const REPLY_REFUSED: u8 = 0x05;

/// One recorded CONNECT request: the raw address type byte plus the decoded
/// target, so tests can pin domain addressing (`atyp == 0x03`) explicitly.
pub struct Connect {
    /// The wire address type: `0x01` IPv4, `0x03` domain, `0x04` IPv6.
    pub atyp: u8,
    /// The requested host, decoded per `atyp`.
    pub host: String,
    /// The requested port.
    pub port: u16,
}

/// A handle to a running [`spawn_socks5`] server.
pub struct Socks5Mock {
    /// The loopback address the mock is listening on.
    pub addr: SocketAddr,
    /// Every CONNECT received so far, in arrival order.
    pub connects: Arc<Mutex<Vec<Connect>>>,
    /// Count of accepted TCP connections, including ones that never
    /// completed a SOCKS5 handshake.
    pub accepted: Arc<AtomicUsize>,
}

/// Listen on a loopback port and answer every CONNECT with `reply`. On
/// success the connection is spliced to `upstream`.
pub async fn spawn_socks5(upstream: Option<SocketAddr>, reply: u8) -> Socks5Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connects = Arc::new(Mutex::new(Vec::new()));
    let accepted = Arc::new(AtomicUsize::new(0));
    let recorded = Arc::clone(&connects);
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(serve_socks5(stream, upstream, reply, Arc::clone(&recorded)));
        }
    });
    Socks5Mock {
        addr,
        connects,
        accepted,
    }
}

async fn serve_socks5(
    mut client: TcpStream,
    upstream: Option<SocketAddr>,
    reply: u8,
    recorded: Arc<Mutex<Vec<Connect>>>,
) {
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting[0], 0x05);
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods).await.unwrap();
    assert!(methods.contains(&0x00), "client must offer no-auth");
    client.write_all(&[0x05, 0x00]).await.unwrap();

    let mut head = [0u8; 4];
    client.read_exact(&mut head).await.unwrap();
    assert_eq!(head[0], 0x05);
    assert_eq!(head[1], 0x01, "expected CONNECT");
    let atyp = head[3];
    let host = match atyp {
        0x01 => {
            let mut octets = [0u8; 4];
            client.read_exact(&mut octets).await.unwrap();
            std::net::Ipv4Addr::from(octets).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await.unwrap();
            let mut name = vec![0u8; len[0] as usize];
            client.read_exact(&mut name).await.unwrap();
            String::from_utf8(name).unwrap()
        }
        0x04 => {
            let mut octets = [0u8; 16];
            client.read_exact(&mut octets).await.unwrap();
            std::net::Ipv6Addr::from(octets).to_string()
        }
        other => panic!("unknown address type {other:#x}"),
    };
    let mut port = [0u8; 2];
    client.read_exact(&mut port).await.unwrap();
    recorded.lock().unwrap().push(Connect {
        atyp,
        host,
        port: u16::from_be_bytes(port),
    });

    client
        .write_all(&[0x05, reply, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    if reply != REPLY_SUCCEEDED {
        return;
    }
    let mut server = TcpStream::connect(upstream.unwrap()).await.unwrap();
    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
}
