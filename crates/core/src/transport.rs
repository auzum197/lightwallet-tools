use tonic::client::GrpcService;
use tonic::codegen::{Body, Bytes, StdError};

/// The transport bound tonic's generated client places on its `T`, named once
/// so the indexer impls don't repeat it. Any tonic `Channel` (plain, Tor)
/// satisfies it. This is the seam that keeps a concrete transport out of the
/// public API: the indexers are generic over it, and the `Send` bounds let
/// their futures be spawned.
pub trait GrpcTransport:
    GrpcService<
        tonic::body::Body,
        Error: Into<StdError>,
        ResponseBody: Body<Data = Bytes, Error: Into<StdError> + Send> + Send + 'static,
        Future: Send,
    > + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T> GrpcTransport for T where
    T: GrpcService<
            tonic::body::Body,
            Error: Into<StdError>,
            ResponseBody: Body<Data = Bytes, Error: Into<StdError> + Send> + Send + 'static,
            Future: Send,
        > + Clone
        + Send
        + Sync
        + 'static
{
}

/// A transport dedicated to one unlinkability domain, the only thing an
/// identity client can be built from.
///
/// Deliberately not `Clone`: a token backs exactly one identity client, so a
/// channel cannot fan out across domains by handing the same one to several
/// clients (docs/adr/0001). A bare `Channel` does not construct an identity
/// client at all, so the sync channel cannot leak onto an identity client by
/// accident. The residual case, wrapping a channel that is secretly shared,
/// has to be written out through [`IdentityTransport::dedicated`], since a
/// tonic `Channel` is opaque and cloneable and core cannot detect it.
///
/// Handing an identity client a bare channel does not compile, which is the
/// point:
///
/// ```compile_fail
/// use lightwallet_core::CanonicalIdentityClient;
/// use lightwallet_core::tonic::transport::Endpoint;
///
/// let channel = Endpoint::from_static("http://localhost:1234").connect_lazy();
/// // `new` wants an `IdentityTransport`, not a `Channel`.
/// let _ = CanonicalIdentityClient::new(channel);
/// ```
#[cfg(any(feature = "canonical", feature = "crosslink"))]
pub struct IdentityTransport<T>(T);

#[cfg(any(feature = "canonical", feature = "crosslink"))]
impl IdentityTransport<tonic::transport::Channel> {
    /// A fresh, lazily-connected direct channel for one identity. Two calls
    /// from the same `Endpoint` yield independent connections, so the domain
    /// split is structural over the direct transport. Synchronous and
    /// infallible: the connection lands on first use.
    pub fn connect_lazy(endpoint: tonic::transport::Endpoint) -> Self {
        Self(endpoint.connect_lazy())
    }
}

#[cfg(any(feature = "canonical", feature = "crosslink"))]
impl<T: GrpcTransport> IdentityTransport<T> {
    /// Wrap a caller-minted transport, a fresh `channel_lazy` from a privacy
    /// transport or an in-memory test channel. The caller owns freshness: this
    /// transport must be built for this one domain, not shared with the sync
    /// client or another identity, or the domains collapse into one linkable
    /// peer.
    pub fn dedicated(transport: T) -> Self {
        Self(transport)
    }

    pub(crate) fn into_inner(self) -> T {
        self.0
    }
}
