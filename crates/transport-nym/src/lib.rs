//! Nym mixnet transport for the lightwallet indexers: a SOCKS5 connector that
//! still yields a plain tonic `Channel`, so construction is the only thing
//! that changes:
//!
//! ```no_run
//! use lightwallet_core::{CanonicalIndexerClient, NetworkParams};
//! use std::net::SocketAddr;
//! use tonic::transport::{ClientTlsConfig, Endpoint};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let params = NetworkParams {
//! #     chain_name: "main".into(),
//! #     activation_heights: Default::default(),
//! #     consensus_branch_id: 0,
//! # };
//! let socks: SocketAddr = "127.0.0.1:1080".parse()?; // a running nym-socks5-client
//! let endpoint = Endpoint::from_static("https://zec.rocks:443")
//!     .tls_config(ClientTlsConfig::new().with_webpki_roots())?;
//! let client = CanonicalIndexerClient::new(
//!     lightwallet_transport_nym::channel(&endpoint, socks).await?,
//!     params,
//! );
//! # Ok(())
//! # }
//! ```
//!
//! The proxy carries the stream through the mixnet to a network requester,
//! which egresses to the endpoint. Hostnames go into the SOCKS5 request as
//! domain addresses and resolve at the requester, so no DNS leaves the
//! client. TLS composes on top: tonic runs the handshake end-to-end to the
//! real lightwalletd when the `Endpoint` carries a `tls_config`.
//!
//! This crate does not embed a Nym client or handle bandwidth credentials.
//! Running and funding the `nym-socks5-client` is the operator's setup.

use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;
use tonic::transport::{Channel, Endpoint, Uri};

/// Connect `endpoint` through the `nym-socks5-client` listening at `socks`,
/// yielding a `Channel` type-identical to a direct one.
pub async fn channel(
    endpoint: &Endpoint,
    socks: SocketAddr,
) -> Result<Channel, tonic::transport::Error> {
    endpoint.connect_with_connector(connector(socks)).await
}

/// Like [`channel`], but the connection is only opened on first use. Lets a
/// wallet construct its indexer client before the SOCKS5 client is reachable.
pub fn channel_lazy(endpoint: &Endpoint, socks: SocketAddr) -> Channel {
    endpoint.connect_with_connector_lazy(connector(socks))
}

fn connector(
    socks: SocketAddr,
) -> impl tower::Service<
    Uri,
    Response = TokioIo<Socks5Stream<TcpStream>>,
    Error = std::io::Error,
    Future = impl Send,
> + Send
+ 'static {
    tower::service_fn(move |uri: Uri| async move {
        let (host, port) = authority(&uri).map_err(std::io::Error::other)?;
        let stream = Socks5Stream::connect(socks, (host.as_str(), port))
            .await
            .map_err(std::io::Error::other)?;
        Ok(TokioIo::new(stream))
    })
}

fn authority(uri: &Uri) -> Result<(String, u16), String> {
    let host = uri
        .host()
        .ok_or_else(|| format!("no host in endpoint uri {uri}"))?;
    let port = uri
        .port_u16()
        .or(match uri.scheme_str() {
            Some("https") => Some(443),
            Some("http") => Some(80),
            _ => None,
        })
        .ok_or_else(|| format!("no port in endpoint uri {uri} and no default for its scheme"))?;
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(uri: &str) -> Result<(String, u16), String> {
        authority(&uri.parse().unwrap())
    }

    #[test]
    fn explicit_port_wins() {
        assert_eq!(
            auth("https://zec.rocks:9067").unwrap(),
            ("zec.rocks".into(), 9067)
        );
    }

    #[test]
    fn scheme_supplies_the_default_port() {
        assert_eq!(
            auth("https://zec.rocks").unwrap(),
            ("zec.rocks".into(), 443)
        );
        assert_eq!(auth("http://localhost").unwrap(), ("localhost".into(), 80));
    }

    #[test]
    fn portless_unknown_scheme_is_rejected() {
        assert!(auth("unix://socket").is_err());
    }

    #[test]
    fn uri_without_a_host_is_rejected() {
        assert!(auth("/no/host").unwrap_err().contains("no host"));
    }

    #[test]
    fn ipv6_literal_host_keeps_its_brackets() {
        // The bracketed form fails tokio-socks' IpAddr parse downstream, so
        // it ships to the proxy as a domain address; tunnel.rs pins that.
        assert_eq!(auth("https://[::1]:9067").unwrap(), ("[::1]".into(), 9067));
    }
}
