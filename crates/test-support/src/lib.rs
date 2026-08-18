//! In-memory mock indexer endpoints for both variants.
//!
//! Each variant module implements its generated `CompactTxStreamer` server
//! trait and serves it over a tokio duplex pipe: real prost encode/decode,
//! real HTTP/2, real gRPC status mapping. No ports, no processes, no
//! `#[ignore]`d tests, and endpoint-level fault injection is one
//! `with_fault` call.
//!
//! Both ends of the pipe use this workspace's generated types, so passing
//! tests prove self-consistency, not protocol conformance. The live suite
//! (`crates/core/tests/live.rs`) is what checks the wire against servers
//! that were not co-designed with the trait.

mod mock;

pub mod canonical;
pub mod crosslink;
pub mod socks5;

/// The RPCs the mocks answer, addressable for fault injection. Anything not
/// listed here (the deprecated nullifier pair) responds `UNIMPLEMENTED`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rpc {
    GetLatestBlock,
    GetBlock,
    GetBlockRange,
    GetTransaction,
    SendTransaction,
    GetTaddressTransactions,
    GetTaddressTxids,
    GetTaddressBalance,
    GetTaddressBalanceStream,
    GetMempoolTx,
    GetMempoolStream,
    GetTreeState,
    GetLatestTreeState,
    GetSubtreeRoots,
    GetAddressUtxos,
    GetAddressUtxosStream,
    GetLightdInfo,
    Ping,
    /// CROSSLINK only.
    GetRoster,
    /// CROSSLINK only.
    GetBondInfo,
    /// CROSSLINK only.
    RequestFaucetDonation,
}

/// The deterministic 32-byte hash `linked_blocks` assigns to `height`, so
/// tests can predict any block's hash without holding the block.
pub fn mock_hash(height: u64) -> [u8; 32] {
    let mut hash = [0u8; 32];
    hash[..8].copy_from_slice(&height.to_le_bytes());
    hash
}

/// True when `cur` sits directly on `prev`: adjacent height and matching
/// prev-hash link. The subtraction is checked so a genesis block (height 0)
/// or a `u64::MAX` from a hostile server returns false instead of underflowing.
///
/// A test assertion, not a wallet primitive: deciding what a broken chain
/// means is sync policy the consumer owns. See `CompactBlockHeader` in
/// `lightwallet-core` for the one-liner a consumer writes inline.
pub fn is_continuous<H: lightwallet_core::CompactBlockHeader>(prev: &H, cur: &H) -> bool {
    cur.height().checked_sub(1) == Some(prev.height()) && cur.prev_hash() == prev.hash()
}

#[cfg(test)]
mod tests {
    use super::is_continuous;
    use lightwallet_proto_canonical::CompactBlock as Canonical;
    use lightwallet_proto_crosslink::CompactBlock as Crosslink;

    #[test]
    fn holds_for_a_linked_next_block_in_both_variants() {
        let a = Canonical {
            height: 100,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let b = Canonical {
            height: 101,
            prev_hash: vec![7u8; 32],
            ..Default::default()
        };
        assert!(is_continuous(&a, &b));

        let c = Crosslink {
            height: 100,
            hash: vec![9u8; 32],
            ..Default::default()
        };
        let d = Crosslink {
            height: 101,
            prev_hash: vec![9u8; 32],
            ..Default::default()
        };
        assert!(is_continuous(&c, &d));
    }

    #[test]
    fn rejects_skipped_repeated_and_mismatched() {
        let base = Canonical {
            height: 100,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let skipped = Canonical {
            height: 102,
            prev_hash: vec![7u8; 32],
            ..Default::default()
        };
        let repeated = Canonical {
            height: 100,
            prev_hash: vec![7u8; 32],
            ..Default::default()
        };
        let mismatched = Canonical {
            height: 101,
            prev_hash: vec![8u8; 32],
            ..Default::default()
        };
        assert!(!is_continuous(&base, &skipped));
        assert!(!is_continuous(&base, &repeated));
        assert!(!is_continuous(&base, &mismatched));
    }

    #[test]
    fn genesis_successor_does_not_underflow() {
        let prev = Canonical {
            height: 0,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        let genesis = Canonical {
            height: 0,
            hash: vec![7u8; 32],
            ..Default::default()
        };
        assert!(!is_continuous(&prev, &genesis));
    }
}
