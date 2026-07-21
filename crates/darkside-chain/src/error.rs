//! Error type for chain construction and ingestion.

use core::fmt;

/// Failures surfaced by the chain state machine.
///
/// Everything here is either an authoring error (caught while building a
/// declared world) or a structurally invalid submission. Consensus-style
/// rejection does not exist in this crate by design.
#[derive(Debug)]
pub enum Error {
    /// Key derivation from a declared seed failed.
    Derivation(String),
    /// A `send` overdraws its source pool at its height.
    InsufficientFunds {
        /// The declared account being spent from.
        account: String,
        /// Pool the spend selects notes in.
        pool: crate::Pool,
        /// Spendable value present, in zatoshis.
        available: u64,
        /// Value the send needs, in zatoshis.
        requested: u64,
    },
    /// A height argument violates a structural invariant, e.g. forking
    /// above the tip or funding a pool before its activation.
    InvalidHeight(String),
    /// Raw transaction bytes failed to parse. The only rejection the
    /// accept path performs.
    TxParse(std::io::Error),
    /// A monetary amount is outside the valid zatoshi range.
    Amount(String),
    /// An address literal failed to parse or carries no usable receiver.
    Address(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Derivation(msg) => write!(f, "key derivation failed: {msg}"),
            Error::InsufficientFunds {
                account,
                pool,
                available,
                requested,
            } => write!(
                f,
                "insufficient funds: {account} has {available} zatoshis in {pool:?}, send needs {requested}"
            ),
            Error::InvalidHeight(msg) => write!(f, "invalid height: {msg}"),
            Error::TxParse(e) => write!(f, "transaction failed to parse: {e}"),
            Error::Amount(msg) => write!(f, "invalid amount: {msg}"),
            Error::Address(msg) => write!(f, "invalid address: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Result alias over [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
