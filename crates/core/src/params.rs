use std::collections::BTreeMap;

/// Per-deployment runtime configuration for one indexer endpoint.
///
/// A carried value: nothing in this crate derives from it. It holds values,
/// not consensus logic, so a Crosslink featurenet that resets each season is
/// just a fresh instance. When downstream transaction logic needs branch-id
/// derivation, this is where it grows (likely onto `zcash_protocol`).
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
