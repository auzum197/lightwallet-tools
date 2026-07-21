//! Note records: darkside's own account of wallet-visible shielded state.
//!
//! Records are created by trial-decrypting mined transactions with the
//! retained UFVKs, exactly the way a rigorous wallet holding the same keys
//! would build its state. Ground truth is this record set, so a corrupt
//! output that a rigorous scanner rejects never credits anyone here either.

use zcash_protocol::TxId;

/// A shielded pool. Re-exported from `zcash_protocol`. Ironwood is a third
/// pool, not a parameter change.
pub use zcash_protocol::ShieldedPool as Pool;

/// One note a declared account received, with everything needed to assert
/// on it or spend it. The nullifier is derived at scan time from the note,
/// its position, and the `nk` in the retained UFVK, so the record alone is
/// enough for the fabricator to spend the note later.
#[derive(Clone, Debug)]
pub struct NoteRecord {
    /// Index of the owning declared account.
    pub account: usize,
    /// Pool the note lives in.
    pub pool: Pool,
    /// Value in zatoshis.
    pub value: u64,
    /// Height of the mining block.
    pub height: u32,
    /// Transaction that created it.
    pub txid: TxId,
    /// Leaf position in the pool's commitment tree.
    pub position: u64,
    /// The real nullifier.
    pub nullifier: [u8; 32],
    /// Height of the transaction that spent it, if any.
    pub spent: Option<u32>,
}

/// An outgoing payment recovered through the sender's OVK: the value
/// and recipient of a declared account's payment, including payments to
/// addresses nobody declared.
#[derive(Clone, Debug)]
pub struct OutgoingPayment {
    /// Index of the sending declared account.
    pub from: usize,
    /// Pool the output landed in.
    pub pool: Pool,
    /// Value in zatoshis.
    pub value: u64,
    /// Height of the mining block.
    pub height: u32,
    /// Transaction carrying the payment.
    pub txid: TxId,
    /// Whether the recipient is one of this chain's declared accounts (a
    /// payment between declared accounts also shows up as a note record).
    pub to_declared: Option<usize>,
}
