//! Tor transport for the lightwallet indexers: an arti-backed connector that
//! still yields a plain tonic `Channel`, so construction is the only thing
//! that changes (§4 of high-level.md):
//!
//! ```no_run
//! use arti_client::{TorClient, TorClientConfig};
//! use lightwallet_core::{CanonicalIdentityClient, CanonicalIndexerClient, NetworkParams};
//! use std::sync::Arc;
//! use tonic::transport::{ClientTlsConfig, Endpoint};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let params = NetworkParams {
//! #     chain_name: "main".into(),
//! #     activation_heights: Default::default(),
//! #     consensus_branch_id: 0,
//! # };
//! let tor = Arc::new(TorClient::create_bootstrapped(TorClientConfig::default()).await?);
//! let endpoint = Endpoint::from_static("https://zec.rocks:443")
//!     .tls_config(ClientTlsConfig::new().with_webpki_roots())?;
//! let client = CanonicalIndexerClient::new(
//!     lightwallet_transport_tor::channel(&endpoint, &tor).await?,
//!     params,
//! );
//! // Identity-bearing RPCs ride an identity client with a channel of its
//! // own, so its own circuits. The lazy form builds no circuit until the
//! // first RPC fires.
//! let broadcast = CanonicalIdentityClient::new(
//!     lightwallet_transport_tor::channel_lazy(&endpoint, &tor),
//! );
//! # Ok(())
//! # }
//! ```
//!
//! TLS composes on top: tonic runs the handshake over the Tor stream when the
//! `Endpoint` carries a `tls_config`.
//!
//! Every [`channel`]/[`channel_lazy`] call mints a fresh isolation token, so
//! two channels never share a circuit: each channel is its own unlinkability
//! domain (docs/adr/0001). To place several channels in one domain on
//! purpose, mint an [`IsolationToken`] and build each of them with
//! [`channel_with_isolation`] or [`channel_lazy_with_isolation`].

use arti_client::{DataStream, StreamPrefs, TorClient};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tonic::transport::{Channel, Endpoint, Uri};
use tor_rtcompat::Runtime;

pub use arti_client::isolation::IsolationToken;

/// Connect `endpoint` through `tor`, yielding a `Channel` type-identical to a
/// direct one. Name the endpoint by hostname: arti resolves it at the Tor
/// exit, so no DNS leaves the client. The channel gets a fresh isolation
/// token, so it shares circuits with nothing else.
pub async fn channel<R: Runtime>(
    endpoint: &Endpoint,
    tor: &Arc<TorClient<R>>,
) -> Result<Channel, tonic::transport::Error> {
    channel_with_isolation(endpoint, tor, IsolationToken::new()).await
}

/// Like [`channel`], but the connection is only opened on first use. Lets a
/// wallet construct its indexer client while the Tor client is still bootstrapping,
/// and keeps an identity client's circuit unbuilt until its first RPC.
pub fn channel_lazy<R: Runtime>(endpoint: &Endpoint, tor: &Arc<TorClient<R>>) -> Channel {
    channel_lazy_with_isolation(endpoint, tor, IsolationToken::new())
}

/// Like [`channel`], with a caller-supplied isolation token: the channel may
/// share circuits with every other channel built from the same token, and
/// with nothing else. This is the only way to group channels into one
/// unlinkability domain.
pub async fn channel_with_isolation<R: Runtime>(
    endpoint: &Endpoint,
    tor: &Arc<TorClient<R>>,
    isolation: IsolationToken,
) -> Result<Channel, tonic::transport::Error> {
    endpoint
        .connect_with_connector(connector(Arc::clone(tor), isolation))
        .await
}

/// The lazy form of [`channel_with_isolation`].
pub fn channel_lazy_with_isolation<R: Runtime>(
    endpoint: &Endpoint,
    tor: &Arc<TorClient<R>>,
    isolation: IsolationToken,
) -> Channel {
    endpoint.connect_with_connector_lazy(connector(Arc::clone(tor), isolation))
}

fn connector<R: Runtime>(
    tor: Arc<TorClient<R>>,
    isolation: IsolationToken,
) -> impl tower::Service<Uri, Response = TokioIo<DataStream>, Error = std::io::Error, Future = impl Send>
+ Send
+ 'static {
    // The token binds to the connector, not to a single dial: tonic re-invokes
    // the connector after a dropped connection, and a reconnect must land in
    // the channel's own domain, not a fresh one and never a shared one.
    let mut prefs = StreamPrefs::new();
    prefs.set_isolation(isolation);
    tower::service_fn(move |uri: Uri| {
        let tor = Arc::clone(&tor);
        let prefs = prefs.clone();
        async move {
            let (host, port) = authority(&uri).map_err(std::io::Error::other)?;
            let stream: DataStream = tor
                .connect_with_prefs((host.as_str(), port), &prefs)
                .await
                .map_err(std::io::Error::other)?;
            Ok(TokioIo::new(stream))
        }
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
        // arti receives the bracketed form verbatim; pinned so a change in
        // the http crate's host parsing surfaces here instead of at an exit.
        assert_eq!(auth("https://[::1]:9067").unwrap(), ("[::1]".into(), 9067));
    }
}
