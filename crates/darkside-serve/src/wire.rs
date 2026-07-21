//! Variant-agnostic extraction of wire data from chain state. The macro in
//! `service.rs` maps these plain structs into each proto crate's generated
//! types, so the conversion logic exists once.

use darkside_chain::{Block, Chain, Corruption, MinedTx, Pool};

/// Which pools a compact response carries. `poolTypes` empty means the
/// legacy behavior: shielded pools only, no transparent data.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoolFilter {
    pub transparent: bool,
    pub sapling: bool,
    pub orchard: bool,
    pub ironwood: bool,
}

impl PoolFilter {
    pub(crate) fn all() -> Self {
        PoolFilter {
            transparent: true,
            sapling: true,
            orchard: true,
            ironwood: true,
        }
    }

    /// From a request's `poolTypes` values (1 transparent, 2 sapling,
    /// 3 orchard, 4 ironwood).
    pub(crate) fn from_pool_types(pool_types: &[i32]) -> Self {
        if pool_types.is_empty() {
            return PoolFilter {
                transparent: false,
                sapling: true,
                orchard: true,
                ironwood: true,
            };
        }
        PoolFilter {
            transparent: pool_types.contains(&1),
            sapling: pool_types.contains(&2),
            orchard: pool_types.contains(&3),
            ironwood: pool_types.contains(&4),
        }
    }
}

pub(crate) struct CompactOutData {
    pub cmu: Vec<u8>,
    pub epk: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub(crate) struct CompactActionData {
    pub nullifier: Vec<u8>,
    pub cmx: Vec<u8>,
    pub epk: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub(crate) struct CompactTxData {
    pub index: u64,
    pub txid: Vec<u8>,
    pub spends: Vec<Vec<u8>>,
    pub outputs: Vec<CompactOutData>,
    pub actions: Vec<CompactActionData>,
    pub ironwood_actions: Vec<CompactActionData>,
    pub vin: Vec<(Vec<u8>, u32)>,
    pub vout: Vec<(u64, Vec<u8>)>,
}

const COMPACT_CIPHERTEXT: usize = 52;

fn compact_ciphertext(full: &[u8], divergent: bool) -> Vec<u8> {
    let mut prefix = full[..COMPACT_CIPHERTEXT].to_vec();
    if divergent {
        // The compact form and the GetTransaction bytes disagree:
        // damage the compact prefix, leave the full bytes intact.
        prefix[0] ^= 0x01;
    }
    prefix
}

fn orchard_actions(
    bundle: &orchard_bundle::Bundle,
    divergent: bool,
    nullifiers_only: bool,
) -> Vec<CompactActionData> {
    bundle
        .actions()
        .iter()
        .map(|action| {
            if nullifiers_only {
                CompactActionData {
                    nullifier: action.nullifier().to_bytes().to_vec(),
                    cmx: Vec::new(),
                    epk: Vec::new(),
                    ciphertext: Vec::new(),
                }
            } else {
                let note = action.encrypted_note();
                CompactActionData {
                    nullifier: action.nullifier().to_bytes().to_vec(),
                    cmx: action.cmx().to_bytes().to_vec(),
                    epk: note.epk_bytes.to_vec(),
                    ciphertext: compact_ciphertext(&note.enc_ciphertext, divergent),
                }
            }
        })
        .collect()
}

mod orchard_bundle {
    pub(super) type Bundle =
        orchard::Bundle<orchard::bundle::Authorized, zcash_protocol::value::ZatBalance>;
}

/// Extract one mined transaction's compact form. `nullifiers_only` serves
/// the deprecated `GetBlockNullifiers` pair: nullifier fields only, no
/// output data, no transparent data.
pub(crate) fn compact_tx(
    mined: &MinedTx,
    index: usize,
    filter: PoolFilter,
    nullifiers_only: bool,
) -> CompactTxData {
    let divergent = mined.corruption == Some(Corruption::Divergence);
    let tx = &mined.tx;
    let mut data = CompactTxData {
        index: index as u64,
        txid: mined.txid.as_ref().to_vec(),
        spends: Vec::new(),
        outputs: Vec::new(),
        actions: Vec::new(),
        ironwood_actions: Vec::new(),
        vin: Vec::new(),
        vout: Vec::new(),
    };
    if filter.sapling
        && let Some(bundle) = tx.sapling_bundle()
    {
        data.spends = bundle
            .shielded_spends()
            .iter()
            .map(|s| s.nullifier().0.to_vec())
            .collect();
        if !nullifiers_only {
            data.outputs = bundle
                .shielded_outputs()
                .iter()
                .map(|o| CompactOutData {
                    cmu: o.cmu().to_bytes().to_vec(),
                    epk: o.ephemeral_key().0.to_vec(),
                    ciphertext: compact_ciphertext(o.enc_ciphertext(), divergent),
                })
                .collect();
        }
    }
    if filter.orchard
        && let Some(bundle) = tx.orchard_bundle()
    {
        data.actions = orchard_actions(bundle, divergent, nullifiers_only);
    }
    if filter.ironwood
        && let Some(bundle) = tx.ironwood_bundle()
    {
        data.ironwood_actions = orchard_actions(bundle, divergent, nullifiers_only);
    }
    if filter.transparent
        && !nullifiers_only
        && let Some(bundle) = tx.transparent_bundle()
    {
        // The null-outpoint coinbase input is omitted; light clients test
        // `index == 0` instead.
        if index != 0 {
            data.vin = bundle
                .vin
                .iter()
                .map(|txin| (txin.prevout().hash().to_vec(), txin.prevout().n()))
                .collect();
        }
        data.vout = bundle
            .vout
            .iter()
            .map(|txout| (txout.value().into_u64(), txout.script_pubkey().0.0.clone()))
            .collect();
    }
    data
}

pub(crate) struct CompactBlockData {
    pub height: u64,
    pub hash: Vec<u8>,
    pub prev_hash: Vec<u8>,
    pub time: u32,
    pub txs: Vec<CompactTxData>,
    /// (sapling, orchard, ironwood) tree sizes. Absent for the
    /// nullifiers-only form.
    pub tree_sizes: Option<(u32, u32, u32)>,
}

pub(crate) fn compact_block(
    chain: &Chain,
    block: &Block,
    filter: PoolFilter,
    nullifiers_only: bool,
) -> CompactBlockData {
    let tree_sizes = (!nullifiers_only)
        .then(|| chain.tree_states(block.height))
        .flatten()
        .map(|t| {
            (
                t.sapling_size as u32,
                t.orchard_size as u32,
                t.ironwood_size as u32,
            )
        });
    CompactBlockData {
        height: block.height as u64,
        hash: block.hash.0.to_vec(),
        prev_hash: block.prev_hash.0.to_vec(),
        time: block.time,
        txs: block
            .txs
            .iter()
            .enumerate()
            .map(|(i, mined)| compact_tx(mined, i, filter, nullifiers_only))
            .collect(),
        tree_sizes,
    }
}

pub(crate) struct TreeStateData {
    pub network: String,
    pub height: u64,
    pub hash: String,
    pub time: u32,
    pub sapling_tree: String,
    pub orchard_tree: String,
    pub ironwood_tree: String,
}

pub(crate) fn tree_state(chain: &Chain, height: u32) -> Option<TreeStateData> {
    let block = chain.block_at(height)?;
    let trees = chain.tree_states(height)?;
    Some(TreeStateData {
        network: chain.params().chain_name.clone(),
        height: height as u64,
        // Block ids are textual: byte-reversed hex, as explorers print.
        hash: block.hash.to_string(),
        time: block.time,
        sapling_tree: hex::encode(trees.sapling),
        orchard_tree: hex::encode(trees.orchard),
        ironwood_tree: hex::encode(trees.ironwood),
    })
}

pub(crate) fn pool_of_shielded_protocol(value: i32) -> Option<Pool> {
    match value {
        0 => Some(Pool::Sapling),
        1 => Some(Pool::Orchard),
        2 => Some(Pool::Ironwood),
        _ => None,
    }
}

pub(crate) struct LightdInfoData {
    pub version: String,
    pub vendor: String,
    pub chain_name: String,
    pub sapling_activation_height: u64,
    pub consensus_branch_id: String,
    pub block_height: u64,
    pub lightwallet_protocol_version: String,
}

pub(crate) fn lightd_info(chain: &Chain) -> LightdInfoData {
    let params = chain.params();
    let tip = chain.tip_height();
    let sapling = params
        .activation(zcash_protocol::consensus::NetworkUpgrade::Sapling)
        .unwrap_or_default();
    LightdInfoData {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        // Unmistakably darkside.
        vendor: "darkside".to_owned(),
        chain_name: params.chain_name.clone(),
        sapling_activation_height: sapling as u64,
        consensus_branch_id: format!("{:08x}", u32::from(params.branch_id(tip))),
        block_height: tip as u64,
        lightwallet_protocol_version: "v0.5.0".to_owned(),
    }
}
