//! The wire RPCs that both variants share but that don't belong on the generic
//! `IndexerClient` trait (see `indexer.rs`). They return each variant's own
//! generated message types, so they can't be generic without an associated type
//! per return message. Instead one macro emits identical inherent methods on
//! both `CanonicalIndexerClient` and `CrosslinkIndexerClient`, keeping the two surfaces from
//! drifting. The overlay is purely additive to canonical, so the bodies really
//! are byte-for-byte the same modulo the proto crate path.
//!
//! Only chain-wide RPCs live here. Anything whose request content names a
//! wallet-specific identifier is in `identity.rs`, on the identity clients,
//! so it cannot ride the sync channel (docs/adr/0001).
//!
//! Errors surface as [`crate::Error`] and streams as `BoxStream`, so tonic stays
//! out of the public signatures here just as it does on the trait.

/// Emit the shared `CompactTxStreamer` surface as inherent methods on `$indexer`,
/// with all message types resolved through the `$proto` crate. `$indexer` must
/// have a `client: CompactTxStreamerClient<T>` field.
macro_rules! impl_streamer_methods {
    ($indexer:ident, $proto:ident) => {
        impl<T: $crate::transport::GrpcTransport> $indexer<T> {
            /// Deprecated upstream: use `get_block_range` with `poolTypes` instead.
            #[deprecated = "use get_block_range with poolTypes"]
            #[allow(deprecated)]
            pub async fn get_block_nullifiers(
                &self,
                height: u64,
            ) -> $crate::error::Result<$proto::CompactBlock> {
                let mut client = self.client.clone();
                Ok(client
                    .get_block_nullifiers($proto::BlockId {
                        height,
                        hash: Vec::new(),
                    })
                    .await?
                    .into_inner())
            }

            /// Deprecated upstream: use `get_block_range` with `poolTypes` instead.
            #[deprecated = "use get_block_range with poolTypes"]
            #[allow(deprecated)]
            pub async fn get_block_range_nullifiers(
                &self,
                start: u64,
                end: u64,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<
                    'static,
                    $crate::error::Result<$proto::CompactBlock>,
                >,
            > {
                let mut client = self.client.clone();
                let stream = client
                    .get_block_range_nullifiers($crate::streamer::block_range!($proto, start, end))
                    .await?
                    .into_inner();
                Ok($crate::error::wrap_stream(stream))
            }

            /// Full mempool transactions, streamed until the next block is mined.
            pub async fn get_mempool_stream(
                &self,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<
                    'static,
                    $crate::error::Result<$proto::RawTransaction>,
                >,
            > {
                let mut client = self.client.clone();
                let stream = client
                    .get_mempool_stream($proto::Empty {})
                    .await?
                    .into_inner();
                Ok($crate::error::wrap_stream(stream))
            }

            /// Note-commitment subtree roots for one shielded protocol. Takes the
            /// generated arg directly because it carries a per-variant enum.
            pub async fn get_subtree_roots(
                &self,
                arg: $proto::GetSubtreeRootsArg,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<
                    'static,
                    $crate::error::Result<$proto::SubtreeRoot>,
                >,
            > {
                let mut client = self.client.clone();
                let stream = client.get_subtree_roots(arg).await?.into_inner();
                Ok($crate::error::wrap_stream(stream))
            }

            /// Version and chain-state metadata for this indexer instance.
            pub async fn get_lightd_info(&self) -> $crate::error::Result<$proto::LightdInfo> {
                let mut client = self.client.clone();
                Ok(client.get_lightd_info($proto::Empty {}).await?.into_inner())
            }

            /// Testing-only latency probe. Requires the server's insecure ping flag.
            pub async fn ping(
                &self,
                interval_us: i64,
            ) -> $crate::error::Result<$proto::PingResponse> {
                let mut client = self.client.clone();
                Ok(client
                    .ping($proto::Duration { interval_us })
                    .await?
                    .into_inner())
            }
        }
    };
}

macro_rules! block_range {
    ($proto:ident, $start:expr, $end:expr) => {
        $proto::BlockRange {
            start: Some($proto::BlockId {
                height: $start,
                hash: Vec::new(),
            }),
            end: Some($proto::BlockId {
                height: $end,
                hash: Vec::new(),
            }),
            pool_types: Vec::new(),
        }
    };
}

macro_rules! taddr_filter {
    ($proto:ident, $address:expr, $start:expr, $end:expr) => {
        $proto::TransparentAddressBlockFilter {
            address: $address,
            range: Some($crate::streamer::block_range!($proto, $start, $end)),
        }
    };
}

pub(crate) use {block_range, impl_streamer_methods, taddr_filter};
