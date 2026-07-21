//! Mined blocks: format-valid, hash-chained, nothing more.

use rand::RngCore;
use zcash_primitives::block::{BlockHash, BlockHeaderData};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::TxId;

use crate::seed::Seed;

/// The declared corruption vocabulary. Attached to a
/// fabricated transaction. Inconsistency must require asking for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corruption {
    /// The output decrypts to a plausible note whose commitment does not
    /// match the one in the block. A rigorous scanner rejects it. A naive
    /// one shows phantom balance.
    Commitment,
    /// Nullifier lies: the spend half carries a nullifier for a note that
    /// was never created.
    Spentness,
    /// The compact form and the `GetTransaction` bytes disagree: the
    /// compact ciphertext prefix is damaged, the full bytes are intact.
    Divergence,
}

/// One transaction inside a mined block.
#[derive(Clone)]
pub struct MinedTx {
    /// ZIP-244 txid (or double-SHA256 for pre-v5).
    pub txid: TxId,
    /// Full serialized bytes, served verbatim by `GetTransaction`.
    pub raw: Vec<u8>,
    /// Parsed form, the source for compact conversion.
    pub tx: Transaction,
    /// Corruption applied to this transaction, if any.
    pub corruption: Option<Corruption>,
}

/// One mined block. Index 0 is always the synthetic coinbase.
#[derive(Clone)]
pub struct Block {
    /// Height, gapless from 0.
    pub height: u32,
    /// This block's hash.
    pub hash: BlockHash,
    /// Hash of the previous block. Held separately from the header so the
    /// `corrupt_prev_hash` escape hatch can break continuity deliberately.
    pub prev_hash: BlockHash,
    /// Block time, an input supplied by the driver.
    pub time: u32,
    /// Transactions in mine order.
    pub txs: Vec<MinedTx>,
}

/// Compute a block hash from a format-valid header. The nonce is drawn from
/// the chain seed so the hash reproduces run to run. The fork id is mixed in
/// so a fork's post-divergence blocks differ from the parent's even when
/// their content is identical.
pub(crate) fn block_hash(
    height: u32,
    fork_id: u64,
    prev: BlockHash,
    time: u32,
    seed: &Seed,
) -> BlockHash {
    let mut nonce = [0u8; 32];
    seed.rng_for(&format!("block-nonce-{fork_id}"), height as u64)
        .fill_bytes(&mut nonce);
    let header = BlockHeaderData {
        version: 4,
        prev_block: prev,
        merkle_root: [0u8; 32],
        final_sapling_root: [0u8; 32],
        time,
        bits: 0x1f07_ffff,
        nonce,
        solution: Vec::new(),
    };
    header
        .freeze()
        .expect("serializing a header to memory cannot fail")
        .hash()
}

/// The hash of an empty prehistory block: a pure function of fork id and
/// height, so `prev_hash(h) = hash(h-1)` holds without materializing or
/// recursing over the whole prehistory (ADR 0002).
pub(crate) fn empty_block_hash(seed: &Seed, fork_id: u64, height: u32) -> BlockHash {
    let mut hash = [0u8; 32];
    seed.rng_for(&format!("empty-block-{fork_id}"), height as u64)
        .fill_bytes(&mut hash);
    BlockHash(hash)
}
