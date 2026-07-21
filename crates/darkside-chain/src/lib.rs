//! Deterministic Zcash chain state machine for the darkside.
//!
//! Pure state: no network, no tonic, no I/O, no clock. Chains are values,
//! a reorg is serving a different chain that shares a prefix, and all
//! derived state (commitment trees, nullifier sets, the UTXO set) is
//! computed from transactions, never set directly. Block time is an input
//! to mining, supplied by whichever driver sits above.
//!
//! Same seed, same chain: every fabricated `rseed`, ephemeral key, and
//! trapdoor is drawn from the seeded RNG, so one declaration yields
//! byte-identical transactions on every run.

mod account;
mod block;
mod chain;
mod error;
mod fabricate;
mod mempool;
mod notes;
mod params;
mod scan;
mod seed;
mod transparent;
mod tree;

pub use account::{Account, seed_bytes};
pub use block::{Block, Corruption, MinedTx};
pub use chain::{Chain, FundSpec, Receiver, Recipient, SendSpec, TreeStates, TxLookup};
pub use error::{Error, Result};
pub use mempool::{Eviction, PendingState, PendingTx, TxSource};
pub use notes::{NoteRecord, OutgoingPayment, Pool};
pub use params::{ChainParams, SyntheticNetwork};
pub use seed::Seed;
pub use transparent::{Utxo, UtxoSet};
pub use tree::SubtreeRoot;
