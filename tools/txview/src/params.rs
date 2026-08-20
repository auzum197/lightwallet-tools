//! Per-variant consensus parameters, enough to turn a block height into the
//! `BranchId` a historical transaction decoded under. The parser owns the
//! height→branch mapping (see [`crate::parse`]), so both tools feed it the same
//! [`ChainParams`] and can never disagree on which ruleset a tx was read with.
//!
//! Canonical chains reuse `zcash_protocol`'s built-in `Network` schedules,
//! selected by the `chain_name` the server reports. The Crosslink featurenet has
//! no upstream schedule crate, so [`FeatureNet`] hand-codes it: every upgrade
//! through Nu6 is live from height 1.

use zcash_protocol::consensus::{BlockHeight, Network, NetworkType, NetworkUpgrade, Parameters};

/// A Crosslink featurenet: every network upgrade active from block 1, on the
/// test address prefixes. A season reset is just a fresh chain that starts over
/// at these same heights, so nothing here needs versioning.
#[derive(Clone, Copy, Debug)]
pub struct FeatureNet;

impl Parameters for FeatureNet {
    fn network_type(&self) -> NetworkType {
        NetworkType::Test
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match nu {
            NetworkUpgrade::Overwinter
            | NetworkUpgrade::Sapling
            | NetworkUpgrade::Blossom
            | NetworkUpgrade::Heartwood
            | NetworkUpgrade::Canopy
            | NetworkUpgrade::Nu5
            | NetworkUpgrade::Nu6
            | NetworkUpgrade::Nu6_1
            | NetworkUpgrade::Nu6_2
            | NetworkUpgrade::Nu6_3 => Some(BlockHeight::from_u32(1)),
        }
    }
}

/// The consensus parameters for one running explorer, carried into every
/// [`crate::parse`] call. One value per connection, chosen from the variant and
/// the chain name the server reports at startup.
#[derive(Clone, Copy, Debug)]
pub enum ChainParams {
    /// A canonical main or test network, on `zcash_protocol`'s schedule.
    Canonical(Network),
    /// The Crosslink featurenet.
    Featurenet(FeatureNet),
}

impl ChainParams {
    /// The canonical params for a `GetLightdInfo` chain name, or `None` for an
    /// unrecognized one (a caller then falls back to the featurenet or a
    /// default).
    pub fn canonical(chain_name: &str) -> Option<Self> {
        match chain_name {
            "main" | "mainnet" => Some(Self::Canonical(Network::MainNetwork)),
            "test" | "testnet" => Some(Self::Canonical(Network::TestNetwork)),
            _ => None,
        }
    }

    /// The Crosslink featurenet params.
    pub fn featurenet() -> Self {
        Self::Featurenet(FeatureNet)
    }
}

impl Parameters for ChainParams {
    fn network_type(&self) -> NetworkType {
        match self {
            Self::Canonical(n) => n.network_type(),
            Self::Featurenet(f) => f.network_type(),
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Self::Canonical(n) => n.activation_height(nu),
            Self::Featurenet(f) => f.activation_height(nu),
        }
    }
}
