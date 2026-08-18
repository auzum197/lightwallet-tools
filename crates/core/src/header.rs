use std::fmt;

/// The header-continuity fields every variant's compact block exposes.
///
/// Deliberately narrow: prevHash continuity (reorg detection) is the one
/// block-level property lightwalletd documents as checked identically across
/// variants. Nothing else is forced into this shape. A variant whose block
/// lacks these fields, or whose `height` means something structurally
/// different, simply does not implement the trait.
///
/// Deciding what a broken chain means (reorg depth, how far to rewind) is the
/// consumer's sync policy. The one primitive underneath it, "does `cur` sit
/// directly on `prev`", is a two-field comparison. Subtract with `checked_sub`,
/// not `-`: `cur.height()` comes off the wire, and a genesis block (height 0)
/// or a hostile `u64::MAX` would otherwise underflow.
///
/// ```
/// use lightwallet_core::CompactBlockHeader;
///
/// fn extends(prev: &impl CompactBlockHeader, cur: &impl CompactBlockHeader) -> bool {
///     cur.height().checked_sub(1) == Some(prev.height()) && cur.prev_hash() == prev.hash()
/// }
/// ```
pub trait CompactBlockHeader {
    /// Block height, consecutive along a chain.
    fn height(&self) -> u64;

    /// This block's hash, as it arrived on the wire. Borrowed and untyped
    /// because proto3 `bytes` carries no length guarantee: a non-conformant
    /// server can send any length, and a borrow has no panic path.
    fn hash(&self) -> &[u8];

    /// The predecessor's hash, as it arrived on the wire.
    fn prev_hash(&self) -> &[u8];

    /// This block's hash as a fixed 32-byte id, for callers that need a typed
    /// hash. Errors if the server sent a hash of some other length.
    fn block_hash(&self) -> Result<[u8; 32], HashLen> {
        hash32(self.hash())
    }

    /// The predecessor's hash as a fixed 32-byte id.
    fn prev_block_hash(&self) -> Result<[u8; 32], HashLen> {
        hash32(self.prev_hash())
    }
}

/// A block-hash field whose length was not 32 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashLen {
    /// The length the server actually sent.
    pub len: usize,
}

impl fmt::Display for HashLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected a 32-byte block hash, got {} bytes", self.len)
    }
}

impl std::error::Error for HashLen {}

fn hash32(bytes: &[u8]) -> Result<[u8; 32], HashLen> {
    bytes.try_into().map_err(|_| HashLen { len: bytes.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Hdr {
        hash: Vec<u8>,
        prev: Vec<u8>,
    }

    impl CompactBlockHeader for Hdr {
        fn height(&self) -> u64 {
            0
        }
        fn hash(&self) -> &[u8] {
            &self.hash
        }
        fn prev_hash(&self) -> &[u8] {
            &self.prev
        }
    }

    fn hdr(hash: Vec<u8>, prev: Vec<u8>) -> Hdr {
        Hdr { hash, prev }
    }

    #[test]
    fn empty_hash_reports_zero_length() {
        assert_eq!(
            hdr(Vec::new(), Vec::new()).block_hash(),
            Err(HashLen { len: 0 })
        );
    }

    #[test]
    fn oversized_hash_reports_its_length() {
        assert_eq!(
            hdr(vec![1; 33], Vec::new()).block_hash(),
            Err(HashLen { len: 33 })
        );
        assert_eq!(hdr(vec![2; 32], Vec::new()).block_hash(), Ok([2u8; 32]));
    }

    #[test]
    fn prev_block_hash_converts_like_block_hash() {
        assert_eq!(
            hdr(Vec::new(), vec![3; 32]).prev_block_hash(),
            Ok([3u8; 32])
        );
        assert_eq!(
            hdr(Vec::new(), vec![4; 20]).prev_block_hash(),
            Err(HashLen { len: 20 })
        );
    }
}
