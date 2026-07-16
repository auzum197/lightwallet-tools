//! The identity-bearing half of the wire surface: RPCs whose request content
//! names a wallet-specific identifier (a txid, a transparent address, a list
//! of held transactions). They are deliberately absent from the indexers, so
//! a request that names an identity cannot ride the sync channel and the
//! wallet's partition of its own activity is expressed by how many identity
//! clients it constructs (see docs/adr/0001 and CONTEXT.md). As with
//! `streamer.rs`, one macro emits the same surface for both variants.

/// Emit the identity-bearing RPC surface as inherent methods on `$client`,
/// with all message types resolved through the `$proto` crate. `$client` must
/// have a `client: CompactTxStreamerClient<T>` field.
macro_rules! impl_identity_methods {
    ($client:ident, $proto:ident) => {
        impl<T: $crate::transport::GrpcTransport> $client<T> {
            /// The full (not compact) transaction with the given txid.
            pub async fn get_transaction(
                &self,
                txid: Vec<u8>,
            ) -> $crate::error::Result<$proto::RawTransaction> {
                let mut client = self.client.clone();
                Ok(client
                    .get_transaction($proto::TxFilter {
                        block: None,
                        index: 0,
                        hash: txid,
                    })
                    .await?
                    .into_inner())
            }

            /// Submit a serialized transaction to the network.
            pub async fn send_transaction(
                &self,
                data: Vec<u8>,
            ) -> $crate::error::Result<$proto::SendResponse> {
                let mut client = self.client.clone();
                Ok(client
                    .send_transaction($proto::RawTransaction { data, height: 0 })
                    .await?
                    .into_inner())
            }

            /// Deprecated upstream (misnamed, returns full transactions): use
            /// `get_taddress_transactions`.
            #[deprecated = "use get_taddress_transactions"]
            #[allow(deprecated)]
            pub async fn get_taddress_txids(
                &self,
                address: String,
                start: u64,
                end: u64,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<
                    'static,
                    $crate::error::Result<$proto::RawTransaction>,
                >,
            > {
                let mut client = self.client.clone();
                let stream = client
                    .get_taddress_txids($crate::streamer::taddr_filter!(
                        $proto, address, start, end
                    ))
                    .await?
                    .into_inner();
                Ok($crate::error::wrap_stream(stream))
            }

            /// Transactions involving `address` within `[start, end]`. Mempool
            /// transactions are excluded.
            pub async fn get_taddress_transactions(
                &self,
                address: String,
                start: u64,
                end: u64,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<
                    'static,
                    $crate::error::Result<$proto::RawTransaction>,
                >,
            > {
                let mut client = self.client.clone();
                let stream = client
                    .get_taddress_transactions($crate::streamer::taddr_filter!(
                        $proto, address, start, end
                    ))
                    .await?
                    .into_inner();
                Ok($crate::error::wrap_stream(stream))
            }

            /// Total balance across the given transparent addresses.
            pub async fn get_taddress_balance(
                &self,
                addresses: Vec<String>,
            ) -> $crate::error::Result<$proto::Balance> {
                let mut client = self.client.clone();
                Ok(client
                    .get_taddress_balance($proto::AddressList { addresses })
                    .await?
                    .into_inner())
            }

            /// Total balance across a client-streamed sequence of transparent
            /// addresses.
            pub async fn get_taddress_balance_stream(
                &self,
                addresses: impl tonic::IntoStreamingRequest<Message = $proto::Address>,
            ) -> $crate::error::Result<$proto::Balance> {
                let mut client = self.client.clone();
                Ok(client
                    .get_taddress_balance_stream(addresses)
                    .await?
                    .into_inner())
            }

            /// Compact transactions currently in the mempool. `exclude_txid_suffixes`
            /// suppresses transactions the caller already has (empty returns all);
            /// that list names held transactions, which is what puts this RPC here.
            pub async fn get_mempool_tx(
                &self,
                exclude_txid_suffixes: Vec<Vec<u8>>,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<'static, $crate::error::Result<$proto::CompactTx>>,
            > {
                let mut client = self.client.clone();
                let stream = client
                    .get_mempool_tx($proto::GetMempoolTxRequest {
                        exclude_txid_suffixes,
                        pool_types: Vec::new(),
                    })
                    .await?
                    .into_inner();
                Ok($crate::error::wrap_stream(stream))
            }

            /// Unspent transparent outputs for the given addresses, from
            /// `start_height`. `max_entries` of 0 means unlimited.
            pub async fn get_address_utxos(
                &self,
                addresses: Vec<String>,
                start_height: u64,
                max_entries: u32,
            ) -> $crate::error::Result<$proto::GetAddressUtxosReplyList> {
                let mut client = self.client.clone();
                Ok(client
                    .get_address_utxos($proto::GetAddressUtxosArg {
                        addresses,
                        start_height,
                        max_entries,
                    })
                    .await?
                    .into_inner())
            }

            /// Streaming form of `get_address_utxos`.
            pub async fn get_address_utxos_stream(
                &self,
                addresses: Vec<String>,
                start_height: u64,
                max_entries: u32,
            ) -> $crate::error::Result<
                futures_util::stream::BoxStream<
                    'static,
                    $crate::error::Result<$proto::GetAddressUtxosReply>,
                >,
            > {
                let mut client = self.client.clone();
                let stream = client
                    .get_address_utxos_stream($proto::GetAddressUtxosArg {
                        addresses,
                        start_height,
                        max_entries,
                    })
                    .await?
                    .into_inner();
                Ok($crate::error::wrap_stream(stream))
            }
        }
    };
}

pub(crate) use impl_identity_methods;
