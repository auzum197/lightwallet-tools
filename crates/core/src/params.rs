use crate::Result;
use crate::error::{Error, MalformedInfo};
use std::collections::BTreeMap;

/// Per-deployment runtime configuration for one indexer endpoint.
///
/// A carried value: nothing in this crate derives from it. It holds values,
/// not consensus logic, so a Crosslink featurenet that resets each season is
/// just a fresh instance. When downstream transaction logic needs branch-id
/// derivation, this is where it grows (likely onto `zcash_protocol`).
///
/// Populate it by hand, or bootstrap it from a live server with
/// [`NetworkParams::from_lightd_info`] or the indexer client's
/// `discover_params`, which fill the chain name, the consensus branch id, and
/// the activation heights `GetLightdInfo` exposes. That is the path for a
/// deployment whose branch id is not known ahead of time, a featurenet
/// especially.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkParams {
    /// The chain this endpoint serves, as `GetLightdInfo` reports it
    /// (`"main"`, `"test"`, a featurenet name).
    pub chain_name: String,
    /// Network-upgrade name to its activation height for this deployment.
    pub activation_heights: BTreeMap<String, u64>,
    /// Consensus branch id currently in force on this deployment.
    pub consensus_branch_id: u32,
}

/// The `GetLightdInfo` fields [`NetworkParams::from_lightd_info`] reads, one
/// accessor per field so both variants' generated `LightdInfo` satisfy it with
/// no shared type. Implemented for the generated `LightdInfo` of each enabled
/// variant; there is no reason to implement it elsewhere.
pub trait LightdInfoView {
    /// The chain name (`"main"`, `"test"`, a featurenet name).
    fn chain_name(&self) -> &str;
    /// The consensus branch id in force, as the wire hex string.
    fn consensus_branch_id_hex(&self) -> &str;
    /// The Sapling activation height for this chain.
    fn sapling_activation_height(&self) -> u64;
    /// The next pending upgrade's name, empty when none is scheduled.
    fn pending_upgrade_name(&self) -> &str;
    /// The next pending upgrade's height, zero when none is scheduled.
    fn pending_upgrade_height(&self) -> u64;
}

impl NetworkParams {
    /// Build params from a server's `GetLightdInfo`. Fills `chain_name`, the
    /// `consensus_branch_id` parsed from the wire hex, and the activation
    /// heights the info carries: Sapling, plus the next pending upgrade when the
    /// server names one. The full historical schedule is not on the wire, so a
    /// consumer that needs every past height derives it elsewhere. Errors only
    /// when the branch id is not valid hex.
    pub fn from_lightd_info<I: LightdInfoView>(info: &I) -> Result<Self> {
        let hex = info.consensus_branch_id_hex();
        let consensus_branch_id =
            u32::from_str_radix(hex.trim_start_matches("0x"), 16).map_err(|_| {
                Error::Info(MalformedInfo {
                    field: "consensusBranchId",
                    value: hex.to_owned(),
                })
            })?;

        let mut activation_heights = BTreeMap::new();
        activation_heights.insert("sapling".to_owned(), info.sapling_activation_height());
        let pending = info.pending_upgrade_name();
        if !pending.is_empty() {
            activation_heights.insert(pending.to_owned(), info.pending_upgrade_height());
        }

        Ok(Self {
            chain_name: info.chain_name().to_owned(),
            activation_heights,
            consensus_branch_id,
        })
    }
}
