use crate::header::HashLen;
use std::fmt;

/// Everything a `lightwallet-core` call can fail with. `tonic` is a private
/// dependency: consumers match on this type, not on gRPC internals. The
/// underlying `tonic::Status` is still reachable through [`Error::code`] for
/// retry classification, or `std::error::Error::source` for the full detail.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The indexer answered with a gRPC error status.
    Rpc(tonic::Status),
    /// A block-hash field on the wire was not 32 bytes.
    HashLen(HashLen),
}

/// Shorthand for `std::result::Result<T, lightwallet_core::Error>`, used by
/// every fallible call and stream item in this crate.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The gRPC status code, when this is an RPC failure. `None` for errors
    /// that never reached the transport (a malformed wire field).
    pub fn code(&self) -> Option<tonic::Code> {
        match self {
            Error::Rpc(status) => Some(status.code()),
            Error::HashLen(_) => None,
        }
    }

    /// Whether retrying the same call could plausibly succeed. A transport
    /// hiccup is worth another attempt; a rejected argument is not.
    pub fn retryable(&self) -> bool {
        matches!(
            self.code(),
            Some(tonic::Code::Unavailable | tonic::Code::DeadlineExceeded)
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Rpc(status) => write!(f, "indexer rpc failed: {status}"),
            Error::HashLen(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Rpc(status) => Some(status),
            Error::HashLen(e) => Some(e),
        }
    }
}

impl From<tonic::Status> for Error {
    fn from(status: tonic::Status) -> Self {
        Error::Rpc(status)
    }
}

impl From<HashLen> for Error {
    fn from(e: HashLen) -> Self {
        Error::HashLen(e)
    }
}

/// Adapt a tonic response stream into the crate's public stream shape: box it so
/// tonic's concrete `Streaming` type stays out of the signature, and map each
/// item's `tonic::Status` into [`Error`].
#[cfg(any(feature = "canonical", feature = "crosslink"))]
pub(crate) fn wrap_stream<S, T>(stream: S) -> futures_util::stream::BoxStream<'static, Result<T>>
where
    S: futures_util::Stream<Item = std::result::Result<T, tonic::Status>> + Send + 'static,
    T: Send + 'static,
{
    use futures_util::stream::StreamExt;
    stream.map(|item| item.map_err(Error::Rpc)).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_status_maps_to_a_retry_decision() {
        let transient: Error = tonic::Status::unavailable("indexer down").into();
        assert_eq!(transient.code(), Some(tonic::Code::Unavailable));
        assert!(transient.retryable());

        let rejected: Error = tonic::Status::invalid_argument("bad height").into();
        assert!(!rejected.retryable());
    }

    #[test]
    fn wire_shape_error_has_no_grpc_code() {
        let e: Error = HashLen { len: 20 }.into();
        assert_eq!(e.code(), None);
        assert!(!e.retryable());
    }

    #[test]
    fn deadline_exceeded_is_retryable() {
        let e: Error = tonic::Status::deadline_exceeded("indexer too slow").into();
        assert!(e.retryable());
    }

    #[test]
    fn display_and_source_expose_the_underlying_failure() {
        use std::error::Error as _;

        let rpc: Error = tonic::Status::not_found("missing").into();
        assert!(rpc.to_string().starts_with("indexer rpc failed: "));
        assert!(rpc.to_string().contains("missing"));
        assert!(
            rpc.source()
                .unwrap()
                .downcast_ref::<tonic::Status>()
                .is_some()
        );

        let wire: Error = HashLen { len: 20 }.into();
        assert_eq!(
            wire.to_string(),
            "expected a 32-byte block hash, got 20 bytes"
        );
        assert!(wire.source().unwrap().downcast_ref::<HashLen>().is_some());
    }
}
