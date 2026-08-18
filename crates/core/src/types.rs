//! Transaction-value newtypes for the identity RPCs. A txid and a serialized
//! transaction are both `Vec<u8>` on the wire and sit on adjacent methods
//! (`get_transaction`, `send_transaction`); distinct types stop a caller
//! passing one where the other belongs. Transparent by design: no length or
//! content validation, matching the crate's "a borrow has no panic path"
//! stance on wire bytes (see `header.rs`).

/// A transaction identifier, as it appears on the wire (conventionally the
/// 32-byte txid). Distinct from [`TxBytes`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Txid(Vec<u8>);

impl Txid {
    /// Wrap raw identifier bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Consume the wrapper for the underlying bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for Txid {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// The bytes of a serialized transaction, ready to broadcast. Distinct from
/// [`Txid`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TxBytes(Vec<u8>);

impl TxBytes {
    /// Wrap the bytes of a serialized transaction.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Consume the wrapper for the underlying bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for TxBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}
