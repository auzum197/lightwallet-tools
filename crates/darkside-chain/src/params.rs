//! Consensus-rich parameters for a synthetic chain.

use zcash_protocol::consensus::{
    BlockHeight, BranchId, Network, NetworkType, NetworkUpgrade, Parameters,
};

/// The ten upgrades darkside models, in consensus order. Nu7 and later
/// are unmodeled and never activate.
const NU_ORDER: [NetworkUpgrade; 10] = [
    NetworkUpgrade::Overwinter,
    NetworkUpgrade::Sapling,
    NetworkUpgrade::Blossom,
    NetworkUpgrade::Heartwood,
    NetworkUpgrade::Canopy,
    NetworkUpgrade::Nu5,
    NetworkUpgrade::Nu6,
    NetworkUpgrade::Nu6_1,
    NetworkUpgrade::Nu6_2,
    NetworkUpgrade::Nu6_3,
];

/// Activation heights indexed by [`NU_ORDER`].
type Table = [Option<BlockHeight>; 10];

fn nu_index(nu: NetworkUpgrade) -> Option<usize> {
    NU_ORDER.iter().position(|&u| u == nu)
}

/// Upgrade names in [`NU_ORDER`] order, for override specs and diagnostics.
const NU_NAMES: [&str; 10] = [
    "overwinter",
    "sapling",
    "blossom",
    "heartwood",
    "canopy",
    "nu5",
    "nu6",
    "nu6.1",
    "nu6.2",
    "nu6.3",
];

/// The upgrade name accepted in an override spec, plus the `ironwood` alias.
fn name_index(name: &str) -> Option<usize> {
    let canonical = if name == "ironwood" { "nu6.3" } else { name };
    NU_NAMES.iter().position(|&n| n == canonical)
}

fn real_table(net: Network) -> Table {
    let mut table = [None; 10];
    for (slot, nu) in table.iter_mut().zip(NU_ORDER) {
        *slot = net.activation_height(nu);
    }
    table
}

/// Which network darkside presents as, and when each upgrade activates.
/// Main and test default to the real `zcash_protocol` schedule. Every entry
/// is overridable, darkside's lie about consensus timing (see
/// ADR 0002). The network kind fixes the address prefixes and branch-id
/// scheme. The table decides activation.
#[derive(Clone, Debug)]
pub enum SyntheticNetwork {
    /// Mainnet prefixes (`u1`, `t1`), real schedule by default.
    Main(Table),
    /// Testnet prefixes (`utest`, `tm`), real schedule by default.
    Test(Table),
    /// Regtest: every upgrade at height 1 by default.
    Regtest(Table),
}

impl SyntheticNetwork {
    /// Mainnet with the real activation schedule.
    pub fn main() -> Self {
        SyntheticNetwork::Main(real_table(Network::MainNetwork))
    }

    /// Testnet with the real activation schedule.
    pub fn test() -> Self {
        SyntheticNetwork::Test(real_table(Network::TestNetwork))
    }

    /// Regtest with every upgrade active from height 1.
    pub fn regtest() -> Self {
        SyntheticNetwork::Regtest([Some(BlockHeight::from_u32(1)); 10])
    }

    /// The Crosslink featurenet: testnet encoding carrying Shielded Labs'
    /// schedule, where everything through NU6 is active from height 1 and
    /// NU6.1 onward never activates. Mirrors their fork's collapsed
    /// `TESTNET_ACTIVATION_HEIGHTS`. Not tied to the Crosslink variant: the
    /// encoding and the served RPC surface are separate choices.
    pub fn crosslink_testnet() -> Self {
        let mut table = [None; 10];
        for slot in table.iter_mut().take(7) {
            *slot = Some(BlockHeight::from_u32(1));
        }
        SyntheticNetwork::Test(table)
    }

    /// Regtest with an explicit activation table (the declaration path).
    pub fn regtest_with(table: Table) -> Self {
        SyntheticNetwork::Regtest(table)
    }

    /// Build a given encoding with an explicit activation table. The
    /// declaration path for a custom chain that names a non-regtest encoding.
    pub fn with_encoding(encoding: NetworkType, heights: [Option<u32>; 10]) -> Self {
        let mut table = [None; 10];
        for (slot, height) in table.iter_mut().zip(heights) {
            *slot = height.map(BlockHeight::from_u32);
        }
        match encoding {
            NetworkType::Main => SyntheticNetwork::Main(table),
            NetworkType::Test => SyntheticNetwork::Test(table),
            NetworkType::Regtest => SyntheticNetwork::Regtest(table),
        }
    }

    /// An encoding's default schedule: the real one for main and test, every
    /// upgrade at height 1 for regtest.
    pub fn default_for(encoding: NetworkType) -> Self {
        match encoding {
            NetworkType::Main => SyntheticNetwork::main(),
            NetworkType::Test => SyntheticNetwork::test(),
            NetworkType::Regtest => SyntheticNetwork::regtest(),
        }
    }

    fn table(&self) -> &Table {
        match self {
            SyntheticNetwork::Main(t)
            | SyntheticNetwork::Test(t)
            | SyntheticNetwork::Regtest(t) => t,
        }
    }

    fn table_mut(&mut self) -> &mut Table {
        match self {
            SyntheticNetwork::Main(t)
            | SyntheticNetwork::Test(t)
            | SyntheticNetwork::Regtest(t) => t,
        }
    }

    /// Apply an override spec: comma-separated `key=value`, left to right
    /// over the current table. `key` is an upgrade name (or `all`). `value`
    /// is a height, `off` (never activates), or `on` (the earliest height
    /// the non-decreasing order allows). Explicit heights that would leave
    /// the schedule decreasing in upgrade order are rejected.
    pub fn apply_overrides(&mut self, spec: &str) -> Result<(), String> {
        for item in spec.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (key, value) = item
                .split_once('=')
                .ok_or_else(|| format!("activation override {item:?} is not key=value"))?;
            let (key, value) = (key.trim(), value.trim());
            let targets: Vec<usize> = if key == "all" {
                (0..NU_ORDER.len()).collect()
            } else {
                vec![name_index(key).ok_or_else(|| format!("unknown upgrade {key:?}"))?]
            };
            for index in targets {
                let table = self.table_mut();
                table[index] = resolve_value(value, index, table)?;
            }
        }
        check_monotone(self.table())
    }
}

fn resolve_value(value: &str, index: usize, table: &Table) -> Result<Option<BlockHeight>, String> {
    match value {
        "off" => Ok(None),
        "on" => {
            let floor = table[..index]
                .iter()
                .flatten()
                .map(|h| u32::from(*h))
                .max()
                .unwrap_or(1);
            Ok(Some(BlockHeight::from_u32(floor.max(1))))
        }
        _ => {
            let height = value
                .strip_prefix("0x")
                .map(|hex| u32::from_str_radix(hex, 16))
                .unwrap_or_else(|| value.parse())
                .map_err(|_| format!("activation height {value:?} is not a number, off, or on"))?;
            Ok(Some(BlockHeight::from_u32(height)))
        }
    }
}

fn check_monotone(table: &Table) -> Result<(), String> {
    let mut prev = 0u32;
    for (index, height) in table.iter().enumerate() {
        if let Some(h) = height {
            let h = u32::from(*h);
            if h < prev {
                return Err(format!(
                    "activation of {} at {h} is before an earlier upgrade at {prev}",
                    NU_NAMES[index],
                ));
            }
            prev = h;
        }
    }
    Ok(())
}

impl Parameters for SyntheticNetwork {
    fn network_type(&self) -> NetworkType {
        match self {
            SyntheticNetwork::Main(_) => NetworkType::Main,
            SyntheticNetwork::Test(_) => NetworkType::Test,
            SyntheticNetwork::Regtest(_) => NetworkType::Regtest,
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        nu_index(nu).and_then(|i| self.table()[i])
    }
}

/// Deployment parameters for one synthetic chain: the network and its
/// activation schedule, plus the data the RPC surface reports.
///
/// Everything `GetLightdInfo` claims derives from here, the same source the
/// block builder consults, so the two cannot drift apart.
#[derive(Clone, Debug)]
pub struct ChainParams {
    /// Network kind and activation schedule.
    pub network: SyntheticNetwork,
    /// Chain name reported over RPC.
    pub chain_name: String,
    /// Timestamp of block 0. Prehistory and scenario-driver block times
    /// derive from this plus [`Self::target_spacing`].
    pub genesis_time: u32,
    /// Block spacing in seconds for derived times.
    pub target_spacing: u32,
    /// Height of the first mined block, the synthetic present. Blocks below
    /// it are the empty prehistory (see ADR 0002).
    pub start_height: u32,
}

impl ChainParams {
    /// Regtest parameters with every upgrade active from height 1 and the
    /// chain rooted at genesis. All three shielded pools exist immediately.
    pub fn regtest() -> Self {
        ChainParams {
            network: SyntheticNetwork::regtest(),
            chain_name: "darkside-regtest".into(),
            genesis_time: 1_700_000_000,
            target_spacing: 30,
            start_height: 0,
        }
    }

    /// Mainnet parameters with the real schedule, rooted just past NU5 so
    /// Orchard is live from the first mined block.
    pub fn main() -> Self {
        let network = SyntheticNetwork::main();
        let start_height = default_start_height(&network);
        ChainParams {
            network,
            chain_name: "main".into(),
            genesis_time: 1_477_641_360,
            target_spacing: 75,
            start_height,
        }
    }

    /// Testnet parameters with the real schedule, rooted just past NU5.
    pub fn test() -> Self {
        let network = SyntheticNetwork::test();
        let start_height = default_start_height(&network);
        ChainParams {
            network,
            chain_name: "test".into(),
            genesis_time: 1_477_270_778,
            target_spacing: 75,
            start_height,
        }
    }

    /// Crosslink featurenet parameters: testnet encoding with Shielded Labs'
    /// collapsed schedule (see [`SyntheticNetwork::crosslink_testnet`]). Boots
    /// at height 1, where NU5 and thus Orchard are already live.
    pub fn crosslink_testnet() -> Self {
        let network = SyntheticNetwork::crosslink_testnet();
        let start_height = default_start_height(&network);
        ChainParams {
            network,
            chain_name: "test".into(),
            genesis_time: 1_477_270_778,
            target_spacing: 75,
            start_height,
        }
    }

    /// The boot height a network defaults to when none is given: NU5 for
    /// main and test (so Orchard is live), genesis for regtest. Recompute
    /// after applying activation overrides.
    pub fn default_start_height(&self) -> u32 {
        default_start_height(&self.network)
    }

    /// Consensus branch id in force at `height`.
    pub fn branch_id(&self, height: u32) -> BranchId {
        BranchId::for_height(&self.network, BlockHeight::from_u32(height))
    }

    /// Whether `nu` is active at `height`.
    pub fn is_active(&self, nu: NetworkUpgrade, height: u32) -> bool {
        self.network.is_nu_active(nu, BlockHeight::from_u32(height))
    }

    /// Activation height of `nu`, if it activates at all.
    pub fn activation(&self, nu: NetworkUpgrade) -> Option<u32> {
        self.network.activation_height(nu).map(u32::from)
    }

    /// Whether the Ironwood pool (NU6.3) exists at `height`.
    pub fn ironwood_active(&self, height: u32) -> bool {
        self.is_active(NetworkUpgrade::Nu6_3, height)
    }

    /// Derived block time for `height`.
    pub fn scheduled_time(&self, height: u32) -> u32 {
        self.genesis_time
            .saturating_add(height.saturating_mul(self.target_spacing))
    }

    /// Encode a transparent address to its string form for this network.
    pub fn encode_taddr(&self, addr: &zcash_transparent::address::TransparentAddress) -> String {
        addr.to_zcash_address(self.network.network_type()).encode()
    }

    /// Parse a transparent address string.
    pub fn parse_taddr(&self, s: &str) -> Option<zcash_transparent::address::TransparentAddress> {
        let addr: zcash_address::ZcashAddress = s.parse().ok()?;
        addr.convert().ok()
    }
}

fn default_start_height(network: &SyntheticNetwork) -> u32 {
    match network {
        SyntheticNetwork::Regtest(_) => 0,
        _ => network
            .activation_height(NetworkUpgrade::Nu5)
            .map(u32::from)
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u32) -> Option<BlockHeight> {
        Some(BlockHeight::from_u32(n))
    }

    #[test]
    fn regtest_activates_everything_at_one() {
        let net = SyntheticNetwork::regtest();
        assert!(matches!(net.network_type(), NetworkType::Regtest));
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6_3), h(1));
    }

    #[test]
    fn main_carries_the_real_schedule_and_prefix() {
        let net = SyntheticNetwork::main();
        assert!(matches!(net.network_type(), NetworkType::Main));
        // NU5 (where Orchard turns on) is a real mainnet height well above 1.
        assert!(u32::from(net.activation_height(NetworkUpgrade::Nu5).unwrap()) > 1_000_000);
        // The boot height defaults to that NU5 height, so Orchard is live.
        assert_eq!(default_start_height(&net), 1_687_104);
    }

    #[test]
    fn crosslink_testnet_collapses_through_nu6_and_disables_later() {
        let net = SyntheticNetwork::crosslink_testnet();
        assert!(matches!(net.network_type(), NetworkType::Test));
        assert_eq!(net.activation_height(NetworkUpgrade::Sapling), h(1));
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6), h(1));
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6_1), None);

        let params = ChainParams::crosslink_testnet();
        // Boots at height 1 (NU5 is live there), and the branch in force is
        // NU6's, not a later upgrade that never activates.
        assert_eq!(params.start_height, 1);
        assert_eq!(params.branch_id(1), BranchId::Nu6);
    }

    #[test]
    fn overrides_flatten_the_schedule_but_keep_the_prefix() {
        let mut net = SyntheticNetwork::main();
        // all=1 gives mainnet prefixes with a regtest-flat schedule; nu6.3
        // toggled off then on resolves to the earliest legal height, 1.
        net.apply_overrides("all=1, nu6.3=off, nu6.3=on").unwrap();
        assert!(matches!(net.network_type(), NetworkType::Main));
        assert_eq!(net.activation_height(NetworkUpgrade::Sapling), h(1));
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6_3), h(1));
    }

    #[test]
    fn off_disables_an_upgrade_and_on_rides_the_previous() {
        let mut net = SyntheticNetwork::regtest();
        net.apply_overrides("all=100, nu6.3=off").unwrap();
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6_3), None);
        net.apply_overrides("nu6.3=on").unwrap();
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6_3), h(100));
    }

    #[test]
    fn decreasing_explicit_heights_are_rejected() {
        let mut net = SyntheticNetwork::regtest();
        assert!(net.apply_overrides("overwinter=200, sapling=100").is_err());
    }

    #[test]
    fn ironwood_alias_resolves_and_unknown_upgrades_error() {
        let mut net = SyntheticNetwork::regtest();
        net.apply_overrides("ironwood=500").unwrap();
        assert_eq!(net.activation_height(NetworkUpgrade::Nu6_3), h(500));
        assert!(net.apply_overrides("frobnicate=1").is_err());
        assert!(net.apply_overrides("nu6=banana").is_err());
    }
}
