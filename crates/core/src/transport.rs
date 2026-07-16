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
