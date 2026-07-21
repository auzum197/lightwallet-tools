use crate::error::wrap_stream;
use crate::identity::{impl_identity_ctors, impl_identity_methods};
use crate::streamer::impl_streamer_methods;
use crate::transport::GrpcTransport;
use crate::{CompactBlockHeader, IndexerClient, NetworkParams, Result};
use futures_util::stream::BoxStream;
use lightwallet_proto_canonical as proto;
use lightwallet_proto_canonical::compact_tx_streamer_client::CompactTxStreamerClient;
use lightwallet_proto_canonical::{BlockId, BlockRange, ChainSpec, CompactBlock, Empty, TreeState};

impl CompactBlockHeader for CompactBlock {
    fn height(&self) -> u64 {
        self.height
    }
    fn hash(&self) -> &[u8] {
        &self.hash
    }
    fn prev_hash(&self) -> &[u8] {
        &self.prev_hash
    }
}

impl crate::params::LightdInfoView for proto::LightdInfo {
    fn chain_name(&self) -> &str {
        &self.chain_name
    }
    fn consensus_branch_id_hex(&self) -> &str {
        &self.consensus_branch_id
    }
    fn sapling_activation_height(&self) -> u64 {
        self.sapling_activation_height
    }
    fn pending_upgrade_name(&self) -> &str {
        &self.upgrade_name
    }
    fn pending_upgrade_height(&self) -> u64 {
        self.upgrade_height
    }
}

/// Indexer for the CANONICAL variant.
pub struct CanonicalIndexerClient<T> {
    client: CompactTxStreamerClient<T>,
    params: NetworkParams,
}

impl<T: GrpcTransport> CanonicalIndexerClient<T> {
    /// Wrap `transport` as a Canonical indexer client carrying `params`.
    pub fn new(transport: T, params: NetworkParams) -> Self {
        Self {
            client: CompactTxStreamerClient::new(transport),
            params,
        }
    }
}

impl_streamer_methods!(CanonicalIndexerClient, proto);

/// One unlinkability domain on the CANONICAL variant: the identity-bearing
/// RPCs, over a transport of their own. Mint one per identity the wallet
/// wants a server to see as a stranger (each transparent address, each
/// broadcast, each confirmation poll); see docs/adr/0001.
pub struct CanonicalIdentityClient<T> {
    client: CompactTxStreamerClient<T>,
}

impl_identity_ctors!(CanonicalIdentityClient);
impl_identity_methods!(CanonicalIdentityClient, proto);

impl<T: GrpcTransport> IndexerClient for CanonicalIndexerClient<T> {
    type Block = CompactBlock;
    type TreeState = TreeState;

    async fn get_latest_height(&self) -> Result<u64> {
        let mut client = self.client.clone();
        Ok(client
            .get_latest_block(ChainSpec {})
            .await?
            .into_inner()
            .height)
    }

    async fn get_block(&self, height: u64) -> Result<CompactBlock> {
        let mut client = self.client.clone();
        let block = client
            .get_block(BlockId {
                height,
                hash: Vec::new(),
            })
            .await?
            .into_inner();
        Ok(block)
    }

    async fn get_block_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<BoxStream<'static, Result<CompactBlock>>> {
        let mut client = self.client.clone();
        let blocks = client
            .get_block_range(BlockRange {
                start: Some(BlockId {
                    height: start,
                    hash: Vec::new(),
                }),
                end: Some(BlockId {
                    height: end,
                    hash: Vec::new(),
                }),
                pool_types: Vec::new(),
            })
            .await?
            .into_inner();
        Ok(wrap_stream(blocks))
    }

    async fn get_tree_state(&self, height: u64) -> Result<TreeState> {
        let mut client = self.client.clone();
        let state = client
            .get_tree_state(BlockId {
                height,
                hash: Vec::new(),
            })
            .await?
            .into_inner();
        Ok(state)
    }

    async fn get_latest_tree_state(&self) -> Result<TreeState> {
        let mut client = self.client.clone();
        Ok(client.get_latest_tree_state(Empty {}).await?.into_inner())
    }

    fn network_params(&self) -> &NetworkParams {
        &self.params
    }
}
