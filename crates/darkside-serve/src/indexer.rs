//! A direct `lightwallet_core::IndexerClient` over darkside: the
//! generic sync loop runs against darkside with no socket in between.

use std::collections::BTreeMap;

use futures_util::stream::BoxStream;
use lightwallet_core::{IndexerClient, NetworkParams};
use lightwallet_proto_canonical as proto;
use zcash_protocol::consensus::NetworkUpgrade;

use crate::wire::PoolFilter;
use crate::{Darkside, canonical};

/// An in-process indexer client over darkside, using the Canonical
/// variant's generated types. Requests are recorded in the observations,
/// so scenario barriers see this client like any wallet.
pub struct DarksideIndexerClient {
    darkside: Darkside,
    params: NetworkParams,
}

const UPGRADES: [(NetworkUpgrade, &str); 10] = [
    (NetworkUpgrade::Overwinter, "Overwinter"),
    (NetworkUpgrade::Sapling, "Sapling"),
    (NetworkUpgrade::Blossom, "Blossom"),
    (NetworkUpgrade::Heartwood, "Heartwood"),
    (NetworkUpgrade::Canopy, "Canopy"),
    (NetworkUpgrade::Nu5, "NU5"),
    (NetworkUpgrade::Nu6, "NU6"),
    (NetworkUpgrade::Nu6_1, "NU6.1"),
    (NetworkUpgrade::Nu6_2, "NU6.2"),
    (NetworkUpgrade::Nu6_3, "NU6.3"),
];

impl DarksideIndexerClient {
    /// Wrap a darkside, deriving the deployment description from the
    /// served chain's parameters, the same source `GetLightdInfo` reports.
    pub fn new(darkside: Darkside) -> Self {
        let params = darkside.with_chain(|chain| {
            let chain_params = chain.params();
            let mut activation_heights = BTreeMap::new();
            for (nu, name) in UPGRADES {
                if let Some(height) = chain_params.activation(nu) {
                    activation_heights.insert(name.to_owned(), height as u64);
                }
            }
            NetworkParams {
                chain_name: chain_params.chain_name.clone(),
                activation_heights,
                consensus_branch_id: u32::from(chain_params.branch_id(chain.tip_height())),
            }
        });
        DarksideIndexerClient { darkside, params }
    }
}

impl IndexerClient for DarksideIndexerClient {
    type Block = proto::CompactBlock;
    type TreeState = proto::TreeState;

    async fn get_latest_height(&self) -> lightwallet_core::Result<u64> {
        Ok(self.darkside.tip() as u64)
    }

    async fn get_block(&self, height: u64) -> lightwallet_core::Result<Self::Block> {
        canonical::get_block_at(
            &self.darkside,
            proto::BlockId {
                height,
                hash: Vec::new(),
            },
            PoolFilter::all(),
            false,
        )
        .map_err(Into::into)
    }

    async fn get_block_range(
        &self,
        start: u64,
        end: u64,
    ) -> lightwallet_core::Result<BoxStream<'static, lightwallet_core::Result<Self::Block>>> {
        use futures_util::StreamExt as _;

        Ok(Box::pin(
            canonical::compact_block_stream(
                self.darkside.clone(),
                start,
                end,
                PoolFilter::all(),
                false,
            )
            .map(|r| r.map_err(Into::into)),
        ))
    }

    async fn get_tree_state(&self, height: u64) -> lightwallet_core::Result<Self::TreeState> {
        self.darkside
            .with_chain(|chain| crate::wire::tree_state(chain, height as u32))
            .map(canonical::to_tree_state)
            .ok_or_else(|| tonic::Status::not_found(format!("no tree state at {height}")).into())
    }

    async fn get_latest_tree_state(&self) -> lightwallet_core::Result<Self::TreeState> {
        let tip = self.darkside.tip();
        self.get_tree_state(tip as u64).await
    }

    fn network_params(&self) -> &NetworkParams {
        &self.params
    }
}
