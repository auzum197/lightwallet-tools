use crate::{CompactBlockHeader, NetworkParams, Result};
use futures_util::stream::BoxStream;

/// A per-variant handle to a lightwalletd-style indexer, over any transport.
/// Futures are `Send` so generic consumers (a wallet on a multi-thread runtime,
/// the fault harness) can spawn them.
///
/// This is only the block-sync path: the RPCs where code written against
/// `I: TestnetIndexer` is genuinely variant-agnostic. The rest of the
/// chain-wide surface (mempool stream, subtree roots, server info) lives as
/// inherent methods on each concrete indexer, returning that variant's own
/// generated types; a wallet talks to one variant chosen at startup, so that
/// surface never needs to be generic. The identity-bearing RPCs (transactions,
/// transparent-address queries, utxos) are not on the indexer at all: they
/// live on the per-domain identity clients so they cannot ride the sync
/// channel (docs/adr/0001).
#[trait_variant::make(Send)]
pub trait TestnetIndexer {
    /// This variant's generated compact-block type.
    type Block: CompactBlockHeader;
    /// This variant's generated note-commitment tree-state type.
    type TreeState;

    /// Height of the block at the tip of the best chain.
    async fn get_latest_height(&self) -> Result<u64>;
    /// The compact block at `height` on the best chain.
    async fn get_block(&self, height: u64) -> Result<Self::Block>;

    /// Consecutive blocks in `[start, end]`, streamed. `end` is inclusive. Each
    /// item is fallible: a stream can fail partway on a dropped connection.
    async fn get_block_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<BoxStream<'static, Result<Self::Block>>>;

    /// Note-commitment tree state as of the block at `height`.
    async fn get_tree_state(&self, height: u64) -> Result<Self::TreeState>;
    /// Note-commitment tree state at the chain tip.
    async fn get_latest_tree_state(&self) -> Result<Self::TreeState>;
    /// The per-deployment parameters this indexer was constructed with.
    fn network_params(&self) -> &NetworkParams;
}

/// One generic function checks header continuity across variants with no shared
/// block type and no conversion code. Generic over the indexer to prove its
/// associated `Block` type carries the capability through.
pub fn assert_continuity<I: TestnetIndexer>(prev: &I::Block, cur: &I::Block) -> bool {
    cur.height() == prev.height() + 1 && cur.prev_hash() == prev.hash()
}
