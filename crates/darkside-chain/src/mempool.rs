//! Pending transactions: the mempool and the height-anchored schedule.
//!
//! One structure serves both. A declared `send ... at 7 pending from 4`
//! is an entry that becomes visible when the tip reaches 4 and is drained
//! into block 7. A wallet submission is an entry visible immediately and
//! drained into the next block unless withheld.

use zcash_primitives::transaction::Transaction;
use zcash_protocol::TxId;

use crate::block::Corruption;

/// Where a pending transaction came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxSource {
    /// Authored by darkside from a declaration or the Rust API. Mined
    /// only at its scheduled height.
    Fabricated,
    /// Accepted over `SendTransaction`. Mined in the next block unless the
    /// withhold knob is on.
    Submitted,
}

/// Why a pending transaction left the pool without being mined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eviction {
    /// `expiryHeight` passed without inclusion.
    Expired,
    /// A conflicting transaction (same nullifier or outpoint) was drained
    /// first at mine time.
    Conflict,
}

/// Lifecycle of a pending transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingState {
    /// Waiting in (or scheduled for) the mempool.
    Pending,
    /// Mined at the given height.
    Mined(u32),
    /// Evicted at the given height.
    Evicted(u32, Eviction),
}

/// One pending transaction.
#[derive(Clone)]
pub struct PendingTx {
    /// Transaction id.
    pub txid: TxId,
    /// Full serialized bytes.
    pub raw: Vec<u8>,
    /// Parsed form.
    pub tx: Transaction,
    /// Height at which the entry becomes visible in the mempool.
    pub visible_from: u32,
    /// Scheduled inclusion height for fabricated entries. `None` means
    /// next-block for submissions and never for fabricated entries.
    pub mine_at: Option<u32>,
    /// Height after which the entry is evicted unmined. Fabricated entries
    /// take this from the declaration. Submissions from the transaction's
    /// own `expiryHeight` (zero meaning no expiry).
    pub expires_after: Option<u32>,
    /// Corruption carried into the block when mined.
    pub corruption: Option<Corruption>,
    /// Origin, which decides draining behavior.
    pub source: TxSource,
    /// Current lifecycle state.
    pub state: PendingState,
}

impl PendingTx {
    /// Whether this entry is visible in the mempool at `tip`.
    pub fn in_mempool(&self, tip: u32) -> bool {
        self.state == PendingState::Pending && self.visible_from <= tip
    }

    /// Whether this entry has expired for inclusion in a block at `height`.
    pub(crate) fn expired_at(&self, height: u32) -> bool {
        self.expires_after.is_some_and(|e| height > e)
    }
}
