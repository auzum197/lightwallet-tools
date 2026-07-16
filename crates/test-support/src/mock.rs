//! One macro emits the whole mock endpoint for a variant: the `MockStreamer`
//! state and configuration surface, the generated-server-trait impl, and the
//! duplex `serve` function. Same rationale as core's `impl_streamer_methods!`:
//! the two variants' surfaces are identical modulo the proto crate path plus
//! the CROSSLINK additions, and a single definition can't drift.
//!
//! The extra_* blocks are token escape hatches for the CROSSLINK delta:
//! `extra_state` adds struct fields, `extra_config` adds `with_*` builders,
//! `extra_rpcs` adds the overlay's trait methods.

macro_rules! mock_streamer {
    (
        $proto:ident,
        extra_state { $($extra_state:tt)* },
        extra_config { $($extra_config:tt)* },
        extra_rpcs { $($extra_rpcs:tt)* } $(,)?
    ) => {
        use $proto::compact_tx_streamer_server::{CompactTxStreamer, CompactTxStreamerServer};

        /// In-memory implementation of this variant's `CompactTxStreamer`.
        ///
        /// Configure with the `with_*` methods, then hand it to [`serve`].
        /// Clone it first to keep a handle for post-hoc assertions (the
        /// clones share the `SendTransaction` inbox).
        #[derive(Clone, Default)]
        pub struct MockStreamer {
            blocks: std::sync::Arc<std::sync::Mutex<Vec<$proto::CompactBlock>>>,
            tree_states: std::collections::BTreeMap<u64, $proto::TreeState>,
            transactions: std::collections::HashMap<Vec<u8>, $proto::RawTransaction>,
            taddress_txs: std::collections::HashMap<String, Vec<$proto::RawTransaction>>,
            balance: i64,
            mempool_txs: Vec<$proto::CompactTx>,
            mempool_stream: Vec<$proto::RawTransaction>,
            utxos: Vec<$proto::GetAddressUtxosReply>,
            subtree_roots: Vec<$proto::SubtreeRoot>,
            lightd_info: $proto::LightdInfo,
            faults: std::collections::HashMap<$crate::Rpc, tonic::Status>,
            stream_faults: std::collections::HashMap<$crate::Rpc, (usize, tonic::Status)>,
            sent: std::sync::Arc<std::sync::Mutex<Vec<$proto::RawTransaction>>>,
            balance_queries: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
            $($extra_state)*
        }

        impl MockStreamer {
            /// An empty mock: no blocks, no state, every RPC answers as if
            /// the chain were vacant.
            pub fn new() -> Self {
                Self::default()
            }

            /// The chain to serve. [`linked_blocks`] builds a hash-linked run.
            pub fn with_blocks(
                self,
                blocks: impl IntoIterator<Item = $proto::CompactBlock>,
            ) -> Self {
                self.replace_chain(blocks);
                self
            }

            /// Swap the served chain mid-test, through any clone: the reorg
            /// lever. Blocks already handed out keep their old hashes, so a
            /// consumer re-fetching across the swap sees the fork.
            pub fn replace_chain(
                &self,
                blocks: impl IntoIterator<Item = $proto::CompactBlock>,
            ) {
                *self.blocks.lock().unwrap() = blocks.into_iter().collect();
            }

            /// Tree state served for `state.height` (and for `GetLatestTreeState`
            /// when it is the highest one registered).
            pub fn with_tree_state(mut self, state: $proto::TreeState) -> Self {
                self.tree_states.insert(state.height, state);
                self
            }

            /// A full transaction served by `GetTransaction` for `txid`.
            pub fn with_transaction(
                mut self,
                txid: Vec<u8>,
                tx: $proto::RawTransaction,
            ) -> Self {
                self.transactions.insert(txid, tx);
                self
            }

            /// Metadata served by `GetLightdInfo`.
            pub fn with_lightd_info(mut self, info: $proto::LightdInfo) -> Self {
                self.lightd_info = info;
                self
            }

            /// Transactions served for `address` by both taddress-transaction
            /// RPCs, filtered to the requested range by `RawTransaction::height`.
            pub fn with_taddress_txs(
                mut self,
                address: impl Into<String>,
                txs: impl IntoIterator<Item = $proto::RawTransaction>,
            ) -> Self {
                self.taddress_txs
                    .insert(address.into(), txs.into_iter().collect());
                self
            }

            /// Total zatoshis answered by both balance RPCs.
            pub fn with_balance(mut self, value_zat: i64) -> Self {
                self.balance = value_zat;
                self
            }

            /// Compact transactions served by `GetMempoolTx`, before the
            /// exclude-suffix filter.
            pub fn with_mempool_txs(
                mut self,
                txs: impl IntoIterator<Item = $proto::CompactTx>,
            ) -> Self {
                self.mempool_txs = txs.into_iter().collect();
                self
            }

            /// Full transactions streamed by `GetMempoolStream`; the stream
            /// then closes, standing in for the next mined block.
            pub fn with_mempool_stream(
                mut self,
                txs: impl IntoIterator<Item = $proto::RawTransaction>,
            ) -> Self {
                self.mempool_stream = txs.into_iter().collect();
                self
            }

            /// UTXOs served by both `GetAddressUtxos` forms, filtered by
            /// `start_height` and truncated to `max_entries` (0 serves all).
            pub fn with_utxos(
                mut self,
                utxos: impl IntoIterator<Item = $proto::GetAddressUtxosReply>,
            ) -> Self {
                self.utxos = utxos.into_iter().collect();
                self
            }

            /// Subtree roots served by `GetSubtreeRoots`, paged by
            /// `start_index` and `max_entries` (0 serves all).
            pub fn with_subtree_roots(
                mut self,
                roots: impl IntoIterator<Item = $proto::SubtreeRoot>,
            ) -> Self {
                self.subtree_roots = roots.into_iter().collect();
                self
            }

            /// Addresses received through `GetTaddressBalanceStream`, in
            /// arrival order.
            pub fn balance_queries(&self) -> Vec<String> {
                self.balance_queries.lock().unwrap().clone()
            }

            /// Answer `rpc` with `status` instead of a response. This is the
            /// endpoint-level fault injection §5 promises: no tower layer, no
            /// separate harness.
            pub fn with_fault(mut self, rpc: $crate::Rpc, status: tonic::Status) -> Self {
                self.faults.insert(rpc, status);
                self
            }

            /// For a streaming `rpc`: yield `after` items normally, then fail
            /// the stream with `status`. Models a connection dropped mid-range.
            pub fn with_stream_fault(
                mut self,
                rpc: $crate::Rpc,
                after: usize,
                status: tonic::Status,
            ) -> Self {
                self.stream_faults.insert(rpc, (after, status));
                self
            }

            /// Transactions received via `SendTransaction`, in arrival order.
            pub fn sent(&self) -> Vec<$proto::RawTransaction> {
                self.sent.lock().unwrap().clone()
            }

            $($extra_config)*

            fn check(&self, rpc: $crate::Rpc) -> std::result::Result<(), tonic::Status> {
                match self.faults.get(&rpc) {
                    Some(status) => Err(status.clone()),
                    None => Ok(()),
                }
            }

            fn block_at(&self, height: u64) -> std::result::Result<$proto::CompactBlock, tonic::Status> {
                self.blocks
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|b| b.height == height)
                    .cloned()
                    .ok_or_else(|| tonic::Status::not_found(format!("no block at height {height}")))
            }

            fn taddress_matches(
                &self,
                filter: $proto::TransparentAddressBlockFilter,
            ) -> std::result::Result<Vec<$proto::RawTransaction>, tonic::Status> {
                let Some((start, end)) = filter.range.and_then(|r| r.start.zip(r.end)) else {
                    return Err(tonic::Status::invalid_argument(
                        "a range with start and end is required",
                    ));
                };
                Ok(self
                    .taddress_txs
                    .get(&filter.address)
                    .map(|txs| {
                        txs.iter()
                            .filter(|tx| tx.height >= start.height && tx.height <= end.height)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default())
            }

            fn utxos_matching(
                &self,
                arg: &$proto::GetAddressUtxosArg,
            ) -> Vec<$proto::GetAddressUtxosReply> {
                let matched = self
                    .utxos
                    .iter()
                    .filter(|utxo| utxo.height >= arg.start_height)
                    .cloned();
                if arg.max_entries > 0 {
                    matched.take(arg.max_entries as usize).collect()
                } else {
                    matched.collect()
                }
            }

            fn stream_of<T: Send + 'static>(
                &self,
                rpc: $crate::Rpc,
                items: Vec<T>,
            ) -> futures_util::stream::BoxStream<'static, std::result::Result<T, tonic::Status>> {
                use futures_util::StreamExt;
                let mut items: Vec<std::result::Result<T, tonic::Status>> =
                    items.into_iter().map(Ok).collect();
                if let Some((after, status)) = self.stream_faults.get(&rpc) {
                    items.truncate(*after);
                    items.push(Err(status.clone()));
                }
                futures_util::stream::iter(items).boxed()
            }
        }

        #[allow(deprecated)]
        #[tonic::async_trait]
        impl CompactTxStreamer for MockStreamer {
            async fn get_latest_block(
                &self,
                _request: tonic::Request<$proto::ChainSpec>,
            ) -> std::result::Result<tonic::Response<$proto::BlockId>, tonic::Status> {
                self.check($crate::Rpc::GetLatestBlock)?;
                let blocks = self.blocks.lock().unwrap();
                let tip = blocks
                    .last()
                    .ok_or_else(|| tonic::Status::unavailable("mock chain is empty"))?;
                Ok(tonic::Response::new($proto::BlockId {
                    height: tip.height,
                    hash: tip.hash.clone(),
                }))
            }

            async fn get_block(
                &self,
                request: tonic::Request<$proto::BlockId>,
            ) -> std::result::Result<tonic::Response<$proto::CompactBlock>, tonic::Status> {
                self.check($crate::Rpc::GetBlock)?;
                Ok(tonic::Response::new(self.block_at(request.into_inner().height)?))
            }

            async fn get_block_nullifiers(
                &self,
                _request: tonic::Request<$proto::BlockId>,
            ) -> std::result::Result<tonic::Response<$proto::CompactBlock>, tonic::Status> {
                Err(tonic::Status::unimplemented("not modeled by the mock"))
            }

            type GetBlockRangeStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::CompactBlock, tonic::Status>,
            >;

            async fn get_block_range(
                &self,
                request: tonic::Request<$proto::BlockRange>,
            ) -> std::result::Result<tonic::Response<Self::GetBlockRangeStream>, tonic::Status> {
                self.check($crate::Rpc::GetBlockRange)?;
                let range = request.into_inner();
                let (start, end) = match (range.start, range.end) {
                    (Some(s), Some(e)) => (s.height, e.height),
                    _ => return Err(tonic::Status::invalid_argument("start and end are required")),
                };
                let mut blocks: Vec<_> = self
                    .blocks
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|b| b.height >= start.min(end) && b.height <= start.max(end))
                    .cloned()
                    .collect();
                blocks.sort_by_key(|b| b.height);
                if start > end {
                    blocks.reverse();
                }
                Ok(tonic::Response::new(
                    self.stream_of($crate::Rpc::GetBlockRange, blocks),
                ))
            }

            type GetBlockRangeNullifiersStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::CompactBlock, tonic::Status>,
            >;

            async fn get_block_range_nullifiers(
                &self,
                _request: tonic::Request<$proto::BlockRange>,
            ) -> std::result::Result<
                tonic::Response<Self::GetBlockRangeNullifiersStream>,
                tonic::Status,
            > {
                Err(tonic::Status::unimplemented("not modeled by the mock"))
            }

            async fn get_transaction(
                &self,
                request: tonic::Request<$proto::TxFilter>,
            ) -> std::result::Result<tonic::Response<$proto::RawTransaction>, tonic::Status> {
                self.check($crate::Rpc::GetTransaction)?;
                let txid = request.into_inner().hash;
                self.transactions
                    .get(&txid)
                    .cloned()
                    .map(tonic::Response::new)
                    .ok_or_else(|| tonic::Status::not_found("no such transaction"))
            }

            async fn send_transaction(
                &self,
                request: tonic::Request<$proto::RawTransaction>,
            ) -> std::result::Result<tonic::Response<$proto::SendResponse>, tonic::Status> {
                self.check($crate::Rpc::SendTransaction)?;
                self.sent.lock().unwrap().push(request.into_inner());
                Ok(tonic::Response::new($proto::SendResponse {
                    error_code: 0,
                    error_message: String::new(),
                }))
            }

            type GetTaddressTxidsStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::RawTransaction, tonic::Status>,
            >;

            async fn get_taddress_txids(
                &self,
                request: tonic::Request<$proto::TransparentAddressBlockFilter>,
            ) -> std::result::Result<tonic::Response<Self::GetTaddressTxidsStream>, tonic::Status>
            {
                self.check($crate::Rpc::GetTaddressTxids)?;
                let txs = self.taddress_matches(request.into_inner())?;
                Ok(tonic::Response::new(
                    self.stream_of($crate::Rpc::GetTaddressTxids, txs),
                ))
            }

            type GetTaddressTransactionsStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::RawTransaction, tonic::Status>,
            >;

            async fn get_taddress_transactions(
                &self,
                request: tonic::Request<$proto::TransparentAddressBlockFilter>,
            ) -> std::result::Result<
                tonic::Response<Self::GetTaddressTransactionsStream>,
                tonic::Status,
            > {
                self.check($crate::Rpc::GetTaddressTransactions)?;
                let txs = self.taddress_matches(request.into_inner())?;
                Ok(tonic::Response::new(
                    self.stream_of($crate::Rpc::GetTaddressTransactions, txs),
                ))
            }

            async fn get_taddress_balance(
                &self,
                _request: tonic::Request<$proto::AddressList>,
            ) -> std::result::Result<tonic::Response<$proto::Balance>, tonic::Status> {
                self.check($crate::Rpc::GetTaddressBalance)?;
                Ok(tonic::Response::new($proto::Balance {
                    value_zat: self.balance,
                }))
            }

            async fn get_taddress_balance_stream(
                &self,
                request: tonic::Request<tonic::Streaming<$proto::Address>>,
            ) -> std::result::Result<tonic::Response<$proto::Balance>, tonic::Status> {
                self.check($crate::Rpc::GetTaddressBalanceStream)?;
                let mut addresses = request.into_inner();
                while let Some(address) = addresses.message().await? {
                    self.balance_queries.lock().unwrap().push(address.address);
                }
                Ok(tonic::Response::new($proto::Balance {
                    value_zat: self.balance,
                }))
            }

            type GetMempoolTxStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::CompactTx, tonic::Status>,
            >;

            async fn get_mempool_tx(
                &self,
                request: tonic::Request<$proto::GetMempoolTxRequest>,
            ) -> std::result::Result<tonic::Response<Self::GetMempoolTxStream>, tonic::Status>
            {
                self.check($crate::Rpc::GetMempoolTx)?;
                let exclude = request.into_inner().exclude_txid_suffixes;
                let txs: Vec<_> = self
                    .mempool_txs
                    .iter()
                    .filter(|tx| !exclude.iter().any(|suffix| tx.txid.ends_with(suffix)))
                    .cloned()
                    .collect();
                Ok(tonic::Response::new(
                    self.stream_of($crate::Rpc::GetMempoolTx, txs),
                ))
            }

            type GetMempoolStreamStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::RawTransaction, tonic::Status>,
            >;

            async fn get_mempool_stream(
                &self,
                _request: tonic::Request<$proto::Empty>,
            ) -> std::result::Result<tonic::Response<Self::GetMempoolStreamStream>, tonic::Status>
            {
                self.check($crate::Rpc::GetMempoolStream)?;
                Ok(tonic::Response::new(self.stream_of(
                    $crate::Rpc::GetMempoolStream,
                    self.mempool_stream.clone(),
                )))
            }

            async fn get_tree_state(
                &self,
                request: tonic::Request<$proto::BlockId>,
            ) -> std::result::Result<tonic::Response<$proto::TreeState>, tonic::Status> {
                self.check($crate::Rpc::GetTreeState)?;
                let height = request.into_inner().height;
                self.tree_states
                    .get(&height)
                    .cloned()
                    .map(tonic::Response::new)
                    .ok_or_else(|| {
                        tonic::Status::not_found(format!("no tree state at height {height}"))
                    })
            }

            async fn get_latest_tree_state(
                &self,
                _request: tonic::Request<$proto::Empty>,
            ) -> std::result::Result<tonic::Response<$proto::TreeState>, tonic::Status> {
                self.check($crate::Rpc::GetLatestTreeState)?;
                self.tree_states
                    .values()
                    .next_back()
                    .cloned()
                    .map(tonic::Response::new)
                    .ok_or_else(|| tonic::Status::unavailable("mock has no tree states"))
            }

            type GetSubtreeRootsStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::SubtreeRoot, tonic::Status>,
            >;

            async fn get_subtree_roots(
                &self,
                request: tonic::Request<$proto::GetSubtreeRootsArg>,
            ) -> std::result::Result<tonic::Response<Self::GetSubtreeRootsStream>, tonic::Status>
            {
                self.check($crate::Rpc::GetSubtreeRoots)?;
                let arg = request.into_inner();
                let paged = self
                    .subtree_roots
                    .iter()
                    .skip(arg.start_index as usize)
                    .cloned();
                let roots: Vec<_> = if arg.max_entries > 0 {
                    paged.take(arg.max_entries as usize).collect()
                } else {
                    paged.collect()
                };
                Ok(tonic::Response::new(
                    self.stream_of($crate::Rpc::GetSubtreeRoots, roots),
                ))
            }

            async fn get_address_utxos(
                &self,
                request: tonic::Request<$proto::GetAddressUtxosArg>,
            ) -> std::result::Result<tonic::Response<$proto::GetAddressUtxosReplyList>, tonic::Status>
            {
                self.check($crate::Rpc::GetAddressUtxos)?;
                let arg = request.into_inner();
                Ok(tonic::Response::new($proto::GetAddressUtxosReplyList {
                    address_utxos: self.utxos_matching(&arg),
                }))
            }

            type GetAddressUtxosStreamStream = futures_util::stream::BoxStream<
                'static,
                std::result::Result<$proto::GetAddressUtxosReply, tonic::Status>,
            >;

            async fn get_address_utxos_stream(
                &self,
                request: tonic::Request<$proto::GetAddressUtxosArg>,
            ) -> std::result::Result<
                tonic::Response<Self::GetAddressUtxosStreamStream>,
                tonic::Status,
            > {
                self.check($crate::Rpc::GetAddressUtxosStream)?;
                let arg = request.into_inner();
                Ok(tonic::Response::new(self.stream_of(
                    $crate::Rpc::GetAddressUtxosStream,
                    self.utxos_matching(&arg),
                )))
            }

            async fn get_lightd_info(
                &self,
                _request: tonic::Request<$proto::Empty>,
            ) -> std::result::Result<tonic::Response<$proto::LightdInfo>, tonic::Status> {
                self.check($crate::Rpc::GetLightdInfo)?;
                Ok(tonic::Response::new(self.lightd_info.clone()))
            }

            async fn ping(
                &self,
                _request: tonic::Request<$proto::Duration>,
            ) -> std::result::Result<tonic::Response<$proto::PingResponse>, tonic::Status> {
                self.check($crate::Rpc::Ping)?;
                Ok(tonic::Response::new($proto::PingResponse { entry: 0, exit: 0 }))
            }

            $($extra_rpcs)*
        }

        /// A hash-linked run of `len` empty compact blocks starting at `start`,
        /// with hashes from [`crate::mock_hash`].
        pub fn linked_blocks(start: u64, len: u64) -> Vec<$proto::CompactBlock> {
            (start..start + len)
                .map(|height| $proto::CompactBlock {
                    height,
                    hash: $crate::mock_hash(height).to_vec(),
                    prev_hash: height
                        .checked_sub(1)
                        .map(|prev| $crate::mock_hash(prev).to_vec())
                        .unwrap_or_else(|| vec![0; 32]),
                    ..Default::default()
                })
                .collect()
        }

        /// Serve `mock` over an in-memory duplex pipe and return a connected
        /// `Channel` for it. The server task runs on the ambient tokio runtime
        /// and exits when the channel drops. The pipe carries exactly one
        /// connection, so a channel reconnect fails rather than silently
        /// serving a fresh, empty mock.
        pub async fn serve(mock: MockStreamer) -> tonic::transport::Channel {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            tokio::spawn(
                tonic::transport::Server::builder()
                    .add_service(CompactTxStreamerServer::new(mock))
                    .serve_with_incoming(futures_util::stream::iter([
                        Ok::<_, std::io::Error>(server_io),
                    ])),
            );
            let mut client_io = Some(client_io);
            tonic::transport::Endpoint::from_static("http://in-memory.mock")
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let io = client_io.take();
                    async move {
                        io.map(hyper_util::rt::TokioIo::new).ok_or_else(|| {
                            std::io::Error::other("mock duplex carries a single connection")
                        })
                    }
                }))
                .await
                .expect("connect to the in-memory mock server")
        }
    };
}

pub(crate) use mock_streamer;
