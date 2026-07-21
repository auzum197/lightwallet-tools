//! One macro emits a variant's whole server: the streamer type, the
//! generated-trait impl over an [`crate::Darkside`], and the in-process
//! serve function. Same discipline as core's `impl_streamer_methods!` and
//! test-support's `mock_streamer!`: one definition, two variants, an
//! `extra_rpcs` escape hatch for the Crosslink delta.

macro_rules! darkside_streamer {
    (
        $proto:ident,
        extra_rpcs { $($extra_rpcs:tt)* } $(,)?
    ) => {
        use $proto::compact_tx_streamer_server::{CompactTxStreamer, CompactTxStreamerServer};

        use $crate::service::{Tier, tier};
        use $crate::wire;

        /// This variant's `CompactTxStreamer` over the shared darkside.
        #[derive(Clone)]
        pub struct DarksideStreamer {
            emu: $crate::Darkside,
        }

        impl DarksideStreamer {
            /// Wrap a darkside handle.
            pub fn new(emu: $crate::Darkside) -> Self {
                DarksideStreamer { emu }
            }
        }

        pub(crate) fn to_compact_tx(data: wire::CompactTxData) -> $proto::CompactTx {
            $proto::CompactTx {
                index: data.index,
                txid: data.txid,
                fee: 0,
                spends: data
                    .spends
                    .into_iter()
                    .map(|nf| $proto::CompactSaplingSpend { nf })
                    .collect(),
                outputs: data
                    .outputs
                    .into_iter()
                    .map(|o| $proto::CompactSaplingOutput {
                        cmu: o.cmu,
                        ephemeral_key: o.epk,
                        ciphertext: o.ciphertext,
                    })
                    .collect(),
                actions: data.actions.into_iter().map(to_compact_action).collect(),
                ironwood_actions: data
                    .ironwood_actions
                    .into_iter()
                    .map(to_compact_action)
                    .collect(),
                vin: data
                    .vin
                    .into_iter()
                    .map(|(txid, n)| $proto::CompactTxIn {
                        prevout_txid: txid,
                        prevout_index: n,
                    })
                    .collect(),
                vout: data
                    .vout
                    .into_iter()
                    .map(|(value, script)| $proto::TxOut {
                        value,
                        script_pub_key: script,
                    })
                    .collect(),
            }
        }

        fn to_compact_action(a: wire::CompactActionData) -> $proto::CompactOrchardAction {
            $proto::CompactOrchardAction {
                nullifier: a.nullifier,
                cmx: a.cmx,
                ephemeral_key: a.epk,
                ciphertext: a.ciphertext,
            }
        }

        pub(crate) fn to_compact_block(data: wire::CompactBlockData) -> $proto::CompactBlock {
            $proto::CompactBlock {
                height: data.height,
                hash: data.hash,
                prev_hash: data.prev_hash,
                time: data.time,
                header: Vec::new(),
                vtx: data.txs.into_iter().map(to_compact_tx).collect(),
                chain_metadata: data.tree_sizes.map(|(sapling, orchard, ironwood)| {
                    $proto::ChainMetadata {
                        sapling_commitment_tree_size: sapling,
                        orchard_commitment_tree_size: orchard,
                        ironwood_commitment_tree_size: ironwood,
                    }
                }),
            }
        }

        pub(crate) fn to_tree_state(data: wire::TreeStateData) -> $proto::TreeState {
            $proto::TreeState {
                network: data.network,
                height: data.height,
                hash: data.hash,
                time: data.time,
                sapling_tree: data.sapling_tree,
                orchard_tree: data.orchard_tree,
                ironwood_tree: data.ironwood_tree,
            }
        }

        /// Blocks for a height range, built as the client reads them. A range
        /// that crosses a skipped span covers millions of heights, so nothing
        /// here may materialize the whole thing: the chain lock is taken one
        /// chunk at a time and released between chunks, which keeps memory
        /// flat and lets the miner interleave.
        pub(crate) fn compact_block_stream(
            emu: $crate::Darkside,
            start: u64,
            end: u64,
            filter: wire::PoolFilter,
            nullifiers_only: bool,
        ) -> Boxed<$proto::CompactBlock> {
            use futures_util::StreamExt as _;

            emu.record_block_request(start.max(end));
            let (lo, hi, descending) = if start <= end {
                (start, end, false)
            } else {
                (end, start, true)
            };
            let step = $crate::service::RANGE_CHUNK;
            let mut chunks: Vec<_> = (lo..=hi)
                .step_by(step as usize)
                .map(|c| (c, c.saturating_add(step - 1).min(hi)))
                .collect();
            if descending {
                chunks.reverse();
            }
            Box::pin(futures_util::stream::iter(chunks).flat_map(
                move |(chunk_lo, chunk_hi)| {
                    let mut blocks: Vec<_> = emu.with_chain(|chain| {
                        (chunk_lo..=chunk_hi)
                            .map(|h| match chain.block_at(h as u32) {
                                Some(block) => Ok(to_compact_block(wire::compact_block(
                                    chain,
                                    &block,
                                    filter,
                                    nullifiers_only,
                                ))),
                                None => Err(tonic::Status::not_found(format!(
                                    "no block at height {h}"
                                ))),
                            })
                            .collect()
                    });
                    if descending {
                        blocks.reverse();
                    }
                    futures_util::stream::iter(blocks)
                },
            ))
        }

        pub(crate) fn get_block_at(
            emu: &$crate::Darkside,
            request: $proto::BlockId,
            filter: wire::PoolFilter,
            nullifiers_only: bool,
        ) -> Result<$proto::CompactBlock, tonic::Status> {
            emu.record_block_request(request.height);
            emu.with_chain(|chain| {
                chain
                    .block_at(request.height as u32)
                    .map(|block| {
                        to_compact_block(wire::compact_block(chain, &block, filter, nullifiers_only))
                    })
                    .ok_or_else(|| {
                        tonic::Status::not_found(format!("no block at height {}", request.height))
                    })
            })
        }

        fn parse_txid(hash: &[u8]) -> Result<zcash_protocol::TxId, tonic::Status> {
            let bytes: [u8; 32] = hash
                .try_into()
                .map_err(|_| tonic::Status::invalid_argument("txid must be 32 bytes"))?;
            Ok(zcash_protocol::TxId::from_bytes(bytes))
        }

        type Boxed<T> = futures_util::stream::BoxStream<'static, Result<T, tonic::Status>>;

        #[tonic::async_trait]
        impl CompactTxStreamer for DarksideStreamer {
            async fn get_latest_block(
                &self,
                _request: tonic::Request<$proto::ChainSpec>,
            ) -> Result<tonic::Response<$proto::BlockId>, tonic::Status> {
                tracing::info!("GetLatestBlock");
                let (height, hash) = self.emu.with_chain(|chain| {
                    let tip = chain.tip_height();
                    let hash = chain
                        .block(tip)
                        .map(|b| b.hash.0.to_vec())
                        .unwrap_or_default();
                    (tip as u64, hash)
                });
                tracing::info!(height, hash = %$crate::service::short_id(&hash), "GetLatestBlock ->");
                Ok(tonic::Response::new($proto::BlockId { height, hash }))
            }

            async fn get_block(
                &self,
                request: tonic::Request<$proto::BlockId>,
            ) -> Result<tonic::Response<$proto::CompactBlock>, tonic::Status> {
                // GetBlock includes all pools, transparent data included.
                let block = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?block, "GetBlock"),
                    _ => tracing::info!(height = block.height, "GetBlock"),
                }
                let result = get_block_at(&self.emu, block, wire::PoolFilter::all(), false);
                match tier() {
                    Tier::Trace => tracing::trace!(?result, "GetBlock ->"),
                    _ => match &result {
                        Ok(b) => tracing::info!(
                            hash = %$crate::service::short_id(&b.hash),
                            txs = b.vtx.len(),
                            "GetBlock ->"
                        ),
                        Err(status) => tracing::info!(error = %status.message(), "GetBlock ->"),
                    },
                }
                result.map(tonic::Response::new)
            }

            async fn get_block_nullifiers(
                &self,
                request: tonic::Request<$proto::BlockId>,
            ) -> Result<tonic::Response<$proto::CompactBlock>, tonic::Status> {
                let block = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?block, "GetBlockNullifiers"),
                    _ => tracing::info!(height = block.height, "GetBlockNullifiers"),
                }
                let result = get_block_at(&self.emu, block, wire::PoolFilter::all(), true);
                match tier() {
                    Tier::Trace => tracing::trace!(?result, "GetBlockNullifiers ->"),
                    _ => match &result {
                        Ok(b) => tracing::info!(
                            hash = %$crate::service::short_id(&b.hash),
                            txs = b.vtx.len(),
                            "GetBlockNullifiers ->"
                        ),
                        Err(status) => {
                            tracing::info!(error = %status.message(), "GetBlockNullifiers ->")
                        }
                    },
                }
                result.map(tonic::Response::new)
            }

            type GetBlockRangeStream = Boxed<$proto::CompactBlock>;

            async fn get_block_range(
                &self,
                request: tonic::Request<$proto::BlockRange>,
            ) -> Result<tonic::Response<Self::GetBlockRangeStream>, tonic::Status> {
                let range = request.into_inner();
                let start = range.start.as_ref().map(|b| b.height).unwrap_or_default();
                let end = range.end.as_ref().map(|b| b.height).unwrap_or_default();
                match tier() {
                    Tier::Trace => tracing::trace!(?range, "GetBlockRange"),
                    _ => tracing::info!(start, end, "GetBlockRange"),
                }
                let filter = wire::PoolFilter::from_pool_types(&range.pool_types);
                let span = format!("{}..{}", start.min(end), start.max(end));
                let blocks = start.max(end) - start.min(end) + 1;
                // The count alone, at every tier. Blocks are built as the
                // client reads them, so none exist to name here, and naming
                // them would rebuild the range the stream exists to avoid.
                tracing::info!(blocks, heights = %span, "GetBlockRange ->");
                Ok(tonic::Response::new(compact_block_stream(
                    self.emu.clone(),
                    start,
                    end,
                    filter,
                    false,
                )))
            }

            type GetBlockRangeNullifiersStream = Boxed<$proto::CompactBlock>;

            async fn get_block_range_nullifiers(
                &self,
                request: tonic::Request<$proto::BlockRange>,
            ) -> Result<tonic::Response<Self::GetBlockRangeNullifiersStream>, tonic::Status>
            {
                let range = request.into_inner();
                let start = range.start.as_ref().map(|b| b.height).unwrap_or_default();
                let end = range.end.as_ref().map(|b| b.height).unwrap_or_default();
                match tier() {
                    Tier::Trace => tracing::trace!(?range, "GetBlockRangeNullifiers"),
                    _ => tracing::info!(start, end, "GetBlockRangeNullifiers"),
                }
                let span = format!("{}..{}", start.min(end), start.max(end));
                let blocks = start.max(end) - start.min(end) + 1;
                tracing::info!(blocks, heights = %span, "GetBlockRangeNullifiers ->");
                Ok(tonic::Response::new(compact_block_stream(
                    self.emu.clone(),
                    start,
                    end,
                    wire::PoolFilter::all(),
                    true,
                )))
            }

            async fn get_transaction(
                &self,
                request: tonic::Request<$proto::TxFilter>,
            ) -> Result<tonic::Response<$proto::RawTransaction>, tonic::Status> {
                let filter = request.into_inner();
                let txid = parse_txid(&filter.hash)?;
                match tier() {
                    Tier::Trace => tracing::trace!(?filter, "GetTransaction"),
                    _ => tracing::info!(%txid, "GetTransaction"),
                }
                let found = self.emu.with_chain(|chain| {
                    chain.transaction(&txid).map(|found| $proto::RawTransaction {
                        data: found.raw.to_vec(),
                        height: found.height.map(|h| h as u64).unwrap_or(0),
                    })
                });
                match tier() {
                    Tier::Trace => tracing::trace!(?found, "GetTransaction ->"),
                    _ => match &found {
                        Some(tx) => {
                            tracing::info!(height = tx.height, bytes = tx.data.len(), "GetTransaction ->")
                        }
                        None => tracing::info!("GetTransaction -> not found"),
                    },
                }
                found
                    .map(tonic::Response::new)
                    .ok_or_else(|| tonic::Status::not_found("transaction not found"))
            }

            async fn send_transaction(
                &self,
                request: tonic::Request<$proto::RawTransaction>,
            ) -> Result<tonic::Response<$proto::SendResponse>, tonic::Status> {
                let raw = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?raw, "SendTransaction"),
                    _ => tracing::info!(bytes = raw.data.len(), "SendTransaction"),
                }
                let response = match self.emu.submit(&raw.data) {
                    Ok(txid) => {
                        tracing::info!(%txid, "SendTransaction ->");
                        // lightwalletd echoes the txid back in error_message on
                        // success; wallets parse this field to learn their txid.
                        $proto::SendResponse {
                            error_code: 0,
                            error_message: txid.to_string(),
                        }
                    }
                    Err(e) => {
                        tracing::info!(error = %e, "SendTransaction -> rejected");
                        $proto::SendResponse {
                            error_code: 1,
                            error_message: e.to_string(),
                        }
                    }
                };
                Ok(tonic::Response::new(response))
            }

            type GetTaddressTxidsStream = Boxed<$proto::RawTransaction>;

            async fn get_taddress_txids(
                &self,
                request: tonic::Request<$proto::TransparentAddressBlockFilter>,
            ) -> Result<tonic::Response<Self::GetTaddressTxidsStream>, tonic::Status> {
                // Pure alias of GetTaddressTransactions: one code path.
                let filter = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?filter, "GetTaddressTxids"),
                    _ => tracing::info!(address = %filter.address, "GetTaddressTxids"),
                }
                let txs = taddress_transactions(&self.emu, filter)?;
                match tier() {
                    Tier::Trace => tracing::trace!(?txs, "GetTaddressTxids ->"),
                    _ => tracing::info!(txs = txs.len(), "GetTaddressTxids ->"),
                }
                Ok(tonic::Response::new(Box::pin(futures_util::stream::iter(
                    txs.into_iter().map(Ok),
                ))))
            }

            type GetTaddressTransactionsStream = Boxed<$proto::RawTransaction>;

            async fn get_taddress_transactions(
                &self,
                request: tonic::Request<$proto::TransparentAddressBlockFilter>,
            ) -> Result<tonic::Response<Self::GetTaddressTransactionsStream>, tonic::Status> {
                let filter = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?filter, "GetTaddressTransactions"),
                    _ => tracing::info!(address = %filter.address, "GetTaddressTransactions"),
                }
                let txs = taddress_transactions(&self.emu, filter)?;
                match tier() {
                    Tier::Trace => tracing::trace!(?txs, "GetTaddressTransactions ->"),
                    _ => tracing::info!(txs = txs.len(), "GetTaddressTransactions ->"),
                }
                Ok(tonic::Response::new(Box::pin(futures_util::stream::iter(
                    txs.into_iter().map(Ok),
                ))))
            }

            async fn get_taddress_balance(
                &self,
                request: tonic::Request<$proto::AddressList>,
            ) -> Result<tonic::Response<$proto::Balance>, tonic::Status> {
                let list = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?list, "GetTaddressBalance"),
                    _ => tracing::info!(addresses = list.addresses.len(), "GetTaddressBalance"),
                }
                let value_zat = self.emu.with_chain(|chain| {
                    list.addresses
                        .iter()
                        .filter_map(|s| chain.params().parse_taddr(s))
                        .map(|addr| chain.utxo_set().balance(&addr) as i64)
                        .sum()
                });
                tracing::info!(value_zat, "GetTaddressBalance ->");
                Ok(tonic::Response::new($proto::Balance { value_zat }))
            }

            async fn get_taddress_balance_stream(
                &self,
                request: tonic::Request<tonic::Streaming<$proto::Address>>,
            ) -> Result<tonic::Response<$proto::Balance>, tonic::Status> {
                use futures_util::StreamExt as _;
                tracing::info!("GetTaddressBalanceStream");
                let mut stream = request.into_inner();
                let mut value_zat = 0i64;
                while let Some(address) = stream.next().await {
                    let address = address?;
                    value_zat += self.emu.with_chain(|chain| {
                        chain
                            .params()
                            .parse_taddr(&address.address)
                            .map(|addr| chain.utxo_set().balance(&addr) as i64)
                            .unwrap_or(0)
                    });
                }
                tracing::info!(value_zat, "GetTaddressBalanceStream ->");
                Ok(tonic::Response::new($proto::Balance { value_zat }))
            }

            type GetMempoolTxStream = Boxed<$proto::CompactTx>;

            async fn get_mempool_tx(
                &self,
                request: tonic::Request<$proto::GetMempoolTxRequest>,
            ) -> Result<tonic::Response<Self::GetMempoolTxStream>, tonic::Status> {
                self.emu.record_mempool_poll();
                let args = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?args, "GetMempoolTx"),
                    _ => tracing::info!(exclude = args.exclude_txid_suffixes.len(), "GetMempoolTx"),
                }
                let filter = wire::PoolFilter::from_pool_types(&args.pool_types);
                let txs = self.emu.with_chain(|chain| {
                    let entries = chain.mempool();
                    let excluded: Vec<&[u8]> = args
                        .exclude_txid_suffixes
                        .iter()
                        .map(|s| s.as_slice())
                        .filter(|suffix| {
                            // A suffix excludes only a unique match.
                            entries
                                .iter()
                                .filter(|p| p.txid.as_ref().ends_with(suffix))
                                .count()
                                == 1
                        })
                        .collect();
                    entries
                        .iter()
                        .filter(|p| {
                            !excluded
                                .iter()
                                .any(|suffix| p.txid.as_ref().ends_with(suffix))
                        })
                        .map(|p| {
                            let mined = darkside_chain::MinedTx {
                                txid: p.txid,
                                raw: p.raw.clone(),
                                tx: p.tx.clone(),
                                corruption: p.corruption,
                            };
                            to_compact_tx(wire::compact_tx(&mined, usize::MAX, filter, false))
                        })
                        .collect::<Vec<_>>()
                });
                let txids: Vec<&[u8]> = txs.iter().map(|t| t.txid.as_slice()).collect();
                match tier() {
                    Tier::Trace => tracing::trace!(?txs, "GetMempoolTx ->"),
                    Tier::Debug => tracing::debug!(
                        txs = txs.len(),
                        txids = %$crate::service::short_ids(&txids, usize::MAX),
                        "GetMempoolTx ->"
                    ),
                    Tier::Info => tracing::info!(
                        txs = txs.len(),
                        txids = %$crate::service::short_ids(&txids, $crate::service::ID_CAP),
                        "GetMempoolTx ->"
                    ),
                }
                Ok(tonic::Response::new(Box::pin(futures_util::stream::iter(
                    txs.into_iter().map(Ok),
                ))))
            }

            type GetMempoolStreamStream = Boxed<$proto::RawTransaction>;

            async fn get_mempool_stream(
                &self,
                _request: tonic::Request<$proto::Empty>,
            ) -> Result<tonic::Response<Self::GetMempoolStreamStream>, tonic::Status> {
                use futures_util::StreamExt as _;
                tracing::info!("GetMempoolStream");
                self.emu.record_mempool_poll();
                let (items, epoch) = self.emu.with_chain(|chain| {
                    let tip = chain.tip_height() as u64;
                    let items: Vec<_> = chain
                        .mempool()
                        .iter()
                        .map(|p| {
                            Ok($proto::RawTransaction {
                                data: p.raw.clone(),
                                height: tip,
                            })
                        })
                        .collect();
                    (items, self.emu.observations().snapshot().epoch)
                });
                match tier() {
                    Tier::Trace => tracing::trace!(?items, "GetMempoolStream ->"),
                    _ => tracing::info!(txs = items.len(), "GetMempoolStream ->"),
                }
                // Emit the current entries, then hold the stream open until
                // the chain changes (a mined block), matching upstream
                // semantics.
                let emu = self.emu.clone();
                let close_on_next_block = futures_util::stream::once(async move {
                    emu.wait_epoch_change(epoch).await;
                })
                .filter_map(|()| async { None });
                Ok(tonic::Response::new(Box::pin(
                    futures_util::stream::iter(items).chain(close_on_next_block),
                )))
            }

            async fn get_tree_state(
                &self,
                request: tonic::Request<$proto::BlockId>,
            ) -> Result<tonic::Response<$proto::TreeState>, tonic::Status> {
                let id = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?id, "GetTreeState"),
                    _ => tracing::info!(height = id.height, "GetTreeState"),
                }
                let height = id.height as u32;
                let result = self
                    .emu
                    .with_chain(|chain| wire::tree_state(chain, height))
                    .map(to_tree_state);
                match tier() {
                    Tier::Trace => tracing::trace!(?result, "GetTreeState ->"),
                    _ => match &result {
                        Some(ts) => {
                            tracing::info!(height = ts.height, hash = %ts.hash, "GetTreeState ->")
                        }
                        None => tracing::info!(height, "GetTreeState -> not found"),
                    },
                }
                result.map(tonic::Response::new).ok_or_else(|| {
                    tonic::Status::not_found(format!("no tree state at height {height}"))
                })
            }

            async fn get_latest_tree_state(
                &self,
                _request: tonic::Request<$proto::Empty>,
            ) -> Result<tonic::Response<$proto::TreeState>, tonic::Status> {
                tracing::info!("GetLatestTreeState");
                let result = self
                    .emu
                    .with_chain(|chain| wire::tree_state(chain, chain.tip_height()))
                    .map(to_tree_state);
                match tier() {
                    Tier::Trace => tracing::trace!(?result, "GetLatestTreeState ->"),
                    _ => {
                        if let Some(ts) = &result {
                            tracing::info!(
                                height = ts.height,
                                hash = %ts.hash,
                                "GetLatestTreeState ->"
                            );
                        }
                    }
                }
                result
                    .map(tonic::Response::new)
                    .ok_or_else(|| tonic::Status::internal("no tree state at the tip"))
            }

            type GetSubtreeRootsStream = Boxed<$proto::SubtreeRoot>;

            async fn get_subtree_roots(
                &self,
                request: tonic::Request<$proto::GetSubtreeRootsArg>,
            ) -> Result<tonic::Response<Self::GetSubtreeRootsStream>, tonic::Status> {
                let args = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?args, "GetSubtreeRoots"),
                    _ => tracing::info!(
                        protocol = args.shielded_protocol,
                        start_index = args.start_index,
                        max_entries = args.max_entries,
                        "GetSubtreeRoots"
                    ),
                }
                let pool = wire::pool_of_shielded_protocol(args.shielded_protocol)
                    .ok_or_else(|| tonic::Status::invalid_argument("unknown shielded protocol"))?;
                let roots = self.emu.with_chain(|chain| {
                    let roots = chain.subtree_roots(pool);
                    let taken = roots
                        .iter()
                        .skip(args.start_index as usize)
                        .map(|r| {
                            Ok($proto::SubtreeRoot {
                                root_hash: r.root_hash.to_vec(),
                                completing_block_hash: r.completing_hash.to_vec(),
                                completing_block_height: r.completing_height as u64,
                            })
                        });
                    if args.max_entries == 0 {
                        taken.collect::<Vec<_>>()
                    } else {
                        taken.take(args.max_entries as usize).collect()
                    }
                });
                let root_hashes: Vec<&[u8]> = roots
                    .iter()
                    .filter_map(|r| r.as_ref().ok())
                    .map(|r| r.root_hash.as_slice())
                    .collect();
                match tier() {
                    Tier::Trace => tracing::trace!(?roots, "GetSubtreeRoots ->"),
                    Tier::Debug => tracing::debug!(
                        roots = roots.len(),
                        hashes = %$crate::service::short_ids(&root_hashes, usize::MAX),
                        "GetSubtreeRoots ->"
                    ),
                    Tier::Info => tracing::info!(
                        roots = roots.len(),
                        hashes = %$crate::service::short_ids(&root_hashes, $crate::service::ID_CAP),
                        "GetSubtreeRoots ->"
                    ),
                }
                Ok(tonic::Response::new(Box::pin(futures_util::stream::iter(
                    roots,
                ))))
            }

            async fn get_address_utxos(
                &self,
                request: tonic::Request<$proto::GetAddressUtxosArg>,
            ) -> Result<tonic::Response<$proto::GetAddressUtxosReplyList>, tonic::Status> {
                let args = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?args, "GetAddressUtxos"),
                    _ => tracing::info!(
                        addresses = args.addresses.len(),
                        start_height = args.start_height,
                        "GetAddressUtxos"
                    ),
                }
                let replies = address_utxos(&self.emu, args);
                let txids: Vec<&[u8]> = replies.iter().map(|r| r.txid.as_slice()).collect();
                match tier() {
                    Tier::Trace => tracing::trace!(?replies, "GetAddressUtxos ->"),
                    Tier::Debug => tracing::debug!(
                        utxos = replies.len(),
                        txids = %$crate::service::short_ids(&txids, usize::MAX),
                        "GetAddressUtxos ->"
                    ),
                    Tier::Info => tracing::info!(
                        utxos = replies.len(),
                        txids = %$crate::service::short_ids(&txids, $crate::service::ID_CAP),
                        "GetAddressUtxos ->"
                    ),
                }
                Ok(tonic::Response::new($proto::GetAddressUtxosReplyList {
                    address_utxos: replies,
                }))
            }

            type GetAddressUtxosStreamStream = Boxed<$proto::GetAddressUtxosReply>;

            async fn get_address_utxos_stream(
                &self,
                request: tonic::Request<$proto::GetAddressUtxosArg>,
            ) -> Result<tonic::Response<Self::GetAddressUtxosStreamStream>, tonic::Status> {
                let args = request.into_inner();
                match tier() {
                    Tier::Trace => tracing::trace!(?args, "GetAddressUtxosStream"),
                    _ => tracing::info!(
                        addresses = args.addresses.len(),
                        start_height = args.start_height,
                        "GetAddressUtxosStream"
                    ),
                }
                let replies = address_utxos(&self.emu, args);
                let txids: Vec<&[u8]> = replies.iter().map(|r| r.txid.as_slice()).collect();
                match tier() {
                    Tier::Trace => tracing::trace!(?replies, "GetAddressUtxosStream ->"),
                    Tier::Debug => tracing::debug!(
                        utxos = replies.len(),
                        txids = %$crate::service::short_ids(&txids, usize::MAX),
                        "GetAddressUtxosStream ->"
                    ),
                    Tier::Info => tracing::info!(
                        utxos = replies.len(),
                        txids = %$crate::service::short_ids(&txids, $crate::service::ID_CAP),
                        "GetAddressUtxosStream ->"
                    ),
                }
                Ok(tonic::Response::new(Box::pin(futures_util::stream::iter(
                    replies.into_iter().map(Ok),
                ))))
            }

            async fn get_lightd_info(
                &self,
                _request: tonic::Request<$proto::Empty>,
            ) -> Result<tonic::Response<$proto::LightdInfo>, tonic::Status> {
                tracing::info!("GetLightdInfo");
                let info = self.emu.with_chain(wire::lightd_info);
                tracing::info!(
                    block_height = info.block_height,
                    chain = %info.chain_name,
                    "GetLightdInfo ->"
                );
                Ok(tonic::Response::new($proto::LightdInfo {
                    version: info.version,
                    vendor: info.vendor,
                    taddr_support: true,
                    chain_name: info.chain_name,
                    sapling_activation_height: info.sapling_activation_height,
                    consensus_branch_id: info.consensus_branch_id,
                    block_height: info.block_height,
                    estimated_height: info.block_height,
                    lightwallet_protocol_version: info.lightwallet_protocol_version,
                    ..Default::default()
                }))
            }

            async fn ping(
                &self,
                request: tonic::Request<$proto::Duration>,
            ) -> Result<tonic::Response<$proto::PingResponse>, tonic::Status> {
                let interval_us = request.into_inner().interval_us;
                tracing::info!(interval_us, "Ping");
                let micros = interval_us.clamp(0, 1_000_000) as u64;
                if micros > 0 {
                    tokio::time::sleep(std::time::Duration::from_micros(micros)).await;
                }
                Ok(tonic::Response::new($proto::PingResponse {
                    entry: 0,
                    exit: 0,
                }))
            }

            $($extra_rpcs)*
        }

        fn taddress_transactions(
            emu: &$crate::Darkside,
            filter: $proto::TransparentAddressBlockFilter,
        ) -> Result<Vec<$proto::RawTransaction>, tonic::Status> {
            let (start, end) = filter
                .range
                .as_ref()
                .map(|r| {
                    (
                        r.start.as_ref().map(|b| b.height).unwrap_or_default(),
                        r.end.as_ref().map(|b| b.height).unwrap_or(u64::MAX),
                    )
                })
                .unwrap_or((0, u64::MAX));
            emu.with_chain(|chain| {
                let addr = chain
                    .params()
                    .parse_taddr(&filter.address)
                    .ok_or_else(|| tonic::Status::invalid_argument("unparseable t-address"))?;
                Ok(chain
                    .utxo_set()
                    .txids_for(&addr)
                    .iter()
                    .filter(|(_, h)| (*h as u64) >= start && (*h as u64) <= end)
                    .filter_map(|(txid, h)| {
                        chain.transaction(txid).map(|found| $proto::RawTransaction {
                            data: found.raw.to_vec(),
                            height: *h as u64,
                        })
                    })
                    .collect())
            })
        }

        fn address_utxos(
            emu: &$crate::Darkside,
            args: $proto::GetAddressUtxosArg,
        ) -> Vec<$proto::GetAddressUtxosReply> {
            for address in &args.addresses {
                emu.record_utxo_request(address);
            }
            emu.with_chain(|chain| {
                let mut replies: Vec<_> = args
                    .addresses
                    .iter()
                    .filter_map(|s| chain.params().parse_taddr(s).map(|a| (s.clone(), a)))
                    .flat_map(|(s, addr)| {
                        chain
                            .utxo_set()
                            .utxos_for(&addr)
                            .into_iter()
                            .map(|utxo| $proto::GetAddressUtxosReply {
                                address: s.clone(),
                                txid: utxo.outpoint.hash().to_vec(),
                                index: utxo.outpoint.n() as i32,
                                script: utxo.script.0.0.clone(),
                                value_zat: utxo.value.into_u64() as i64,
                                height: utxo.height as u64,
                            })
                            .collect::<Vec<_>>()
                    })
                    .filter(|reply| reply.height >= args.start_height)
                    .collect();
                replies.sort_by_key(|r| r.height);
                if args.max_entries > 0 {
                    replies.truncate(args.max_entries as usize);
                }
                replies
            })
        }

        /// Serve this variant in-process over a tokio duplex pipe and hand
        /// back a connected channel: one connection per pipe, so a silent
        /// reconnect cannot swap state.
        pub async fn serve_in_process(emu: $crate::Darkside) -> tonic::transport::Channel {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            tokio::spawn(
                tonic::transport::Server::builder()
                    .add_service(CompactTxStreamerServer::new(DarksideStreamer::new(emu)))
                    .serve_with_incoming(futures_util::stream::iter([Ok::<_, std::io::Error>(
                        server_io,
                    )])),
            );
            let mut client_io = Some(client_io);
            tonic::transport::Endpoint::from_static("http://in-memory.darkside")
                .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
                    let io = client_io.take();
                    async move {
                        io.map(hyper_util::rt::TokioIo::new).ok_or_else(|| {
                            std::io::Error::other("darkside duplex carries a single connection")
                        })
                    }
                }))
                .await
                .expect("connect to the in-process darkside server")
        }

        /// The tonic service, for callers that bind their own listener
        /// (`darkside`, behind the safety flags).
        pub fn service(emu: $crate::Darkside) -> CompactTxStreamerServer<DarksideStreamer> {
            CompactTxStreamerServer::new(DarksideStreamer::new(emu))
        }
    };
}

/// How many identifiers the info tier shows before eliding the rest. Debug
/// shows all of them.
pub(crate) const ID_CAP: usize = 8;

/// How many heights one chain-lock acquisition covers while a block range
/// streams. A range can span millions of unmined heights, so the lock has to
/// be handed back often enough for the miner and the control surface to get
/// their writes in, without paying an acquisition per block.
pub(crate) const RANGE_CHUNK: u64 = 256;

/// The active detail level for a request log. Tiers are exclusive: a call
/// emits one arrival and one return line at the highest enabled level, not
/// info stacked under debug stacked under trace.
pub(crate) enum Tier {
    /// Curated, capped, collapsed.
    Info,
    /// Every identifier, no payloads.
    Debug,
    /// Full decoded structs.
    Trace,
}

/// The highest tier the subscriber will accept, so the handler can pick one
/// line instead of emitting all three.
pub(crate) fn tier() -> Tier {
    if tracing::enabled!(tracing::Level::TRACE) {
        Tier::Trace
    } else if tracing::enabled!(tracing::Level::DEBUG) {
        Tier::Debug
    } else {
        Tier::Info
    }
}

/// The first 8 bytes of an id as hex: enough to recognise a hash or txid,
/// short enough to sit on an info line.
pub(crate) fn short_id(bytes: &[u8]) -> String {
    hex::encode(&bytes[..bytes.len().min(8)])
}

/// A bracketed list of short-hex ids, at most `keep` of them with a `+N`
/// tail for the rest. `keep = usize::MAX` gives the uncapped debug tier.
pub(crate) fn short_ids(ids: &[&[u8]], keep: usize) -> String {
    let shown = ids.len().min(keep);
    let mut out = String::from("[");
    for (i, id) in ids[..shown].iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&short_id(id));
    }
    if ids.len() > shown {
        out.push_str(&format!(", +{}", ids.len() - shown));
    }
    out.push(']');
    out
}
