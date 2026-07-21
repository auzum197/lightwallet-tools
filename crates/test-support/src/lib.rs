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
