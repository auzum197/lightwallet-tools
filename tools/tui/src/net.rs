//! The producer half. Two tasks feed the UI over one `Update` channel:
//!
//! - the **tail** holds a connection open, seeds and gap-fills the block ring off
//!   the mempool-stream close edge, samples chain health, detects reorgs, and
//!   drains `GetMempoolStream`. A clean stream end is a block boundary (reconnect
//!   at once and fill the gap); an error backs off.
//! - the **search** task services on-demand block / txid / t-address lookups,
//!   independent of the tail loop.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use lightwallet_core::{
    CanonicalIdentityClient, CanonicalIndexerClient, CrosslinkIdentityClient,
    CrosslinkIndexerClient, IdentityTransport, IndexerClient, NetworkParams,
};
use lightwallet_txview::{BlockHeight, ChainParams};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use std::time::Instant;

use crate::app::{BlockRow, BlockTx, Phase, Row, TaddrHit, TxDetail, Update};

/// Which protocol surface the endpoint serves.
#[derive(Clone, Copy)]
pub enum Variant {
    Canonical,
    Crosslink,
}

/// An on-demand lookup from the search bar.
pub enum Request {
    Block(u64),
    /// A txid as display-order hex (no `0x`).
    Transaction(String),
    Taddr {
        addr: String,
        start: u64,
        end: u64,
    },
}

/// A mempool transaction, normalized off both variants' generated types.
struct Raw {
    data: Vec<u8>,
    height: u64,
}

/// The block ring depth, and how far back a reorg walk refetches.
const MAX_BLOCKS: u64 = 256;
const REORG_DEPTH: u64 = 10;

const BACKOFF_MAX: Duration = Duration::from_secs(10);
const BACKOFF_START: Duration = Duration::from_millis(500);

/// The tail reconnect loop. Returns when the UI drops the receiver.
pub async fn run(url: String, variant: Variant, tx: UnboundedSender<Update>) {
    let mut backoff = BACKOFF_START;
    // Carried across cycles: the tip height and hash last seen, so a fill knows
    // where the gap starts and whether the chain reorged under it.
    let mut last: Option<(u64, Vec<u8>)> = None;
    loop {
        if tx.send(Update::Phase(Phase::Connecting)).is_err() {
            return;
        }
        match cycle(&url, variant, &tx, &mut last).await {
            Ok(Ended::Block) => backoff = BACKOFF_START,
            Ok(Ended::Closed) => return,
            Err(e) => {
                if tx.send(Update::Phase(Phase::Reconnecting(e))).is_err() {
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Why one connection cycle ended.
enum Ended {
    /// The stream closed at a block boundary. Reconnect immediately.
    Block,
    /// The UI receiver is gone. Stop.
    Closed,
}

async fn cycle(
    url: &str,
    variant: Variant,
    tx: &UnboundedSender<Update>,
    last: &mut Option<(u64, Vec<u8>)>,
) -> Result<Ended, String> {
    let ix = connect(url, variant).await.map_err(|e| format!("{e:#}"))?;
    let params = ix.params().await.ok();
    let chain = chain_params(variant, params.as_ref());
    let tip = ix.latest_height().await.unwrap_or(0);

    // Seed on cold start, gap-fill in steady state; either way `blocks` is the
    // range to emit, oldest-first, and `new_head` its newest (height, hash).
    let mut new_head: Option<(u64, Vec<u8>)> = None;
    if tip > 0 {
        let blocks = fill_range(&ix, last, tip, tx)
            .await
            .map_err(|e| format!("{e:#}"))?;
        if let Some(b) = blocks.last() {
            new_head = Some((b.height, b.hash.clone()));
        }
        for b in blocks {
            if tx.send(Update::MinedBlock(b)).is_err() {
                return Ok(Ended::Closed);
            }
        }
    }
    if let Some(head) = new_head {
        *last = Some(head);
    }

    // Health: sample GetLightdInfo once per edge.
    if let Ok(info) = ix.lightd_info().await
        && tx
            .send(Update::Info {
                block_height: info.0,
                estimated_height: info.1,
            })
            .is_err()
    {
        return Ok(Ended::Closed);
    }

    let mined_unix = if tip > 0 {
        ix.block_time(tip).await.ok()
    } else {
        None
    };

    if tx.send(Update::Block { tip, mined_unix }).is_err() {
        return Ok(Ended::Closed);
    }
    if tx.send(Update::Phase(Phase::Live)).is_err() {
        return Ok(Ended::Closed);
    }

    let mut stream = ix.mempool().await.map_err(|e| format!("{e:#}"))?;
    while let Some(item) = stream.next().await {
        let raw = item.map_err(|e| format!("{e}"))?;
        // A mempool tx is height 0; decode it under the tip's rules.
        let height = if raw.height == 0 { tip } else { raw.height };
        let row = Row {
            seq: 0,
            first_seen: Instant::now(),
            tx: lightwallet_txview::parse(&raw.data, BlockHeight::from_u32(height as u32), &chain),
            raw: raw.data,
        };
        if tx.send(Update::Tx(row)).is_err() {
            return Ok(Ended::Closed);
        }
    }
    Ok(Ended::Block)
}

/// Seed (cold) or gap-fill (steady) the block ring, capped to `MAX_BLOCKS`.
/// Detects a reorg at the fill boundary and, on one, widens the refetch and
/// emits `Update::Reorg` before returning the fresh blocks.
async fn fill_range(
    ix: &Ix,
    last: &Option<(u64, Vec<u8>)>,
    tip: u64,
    tx: &UnboundedSender<Update>,
) -> Result<Vec<BlockRow>> {
    let floor = tip.saturating_sub(MAX_BLOCKS - 1);
    match last {
        None => ix.block_range_pools(floor, tip).await,
        Some((old, old_hash)) => {
            if tip <= *old {
                return Ok(Vec::new());
            }
            let start = floor.max(old + 1);
            let blocks = ix.block_range_pools(start, tip).await?;
            // A gap-fill that starts right after the old tip must chain onto it;
            // a mismatch means the chain reorged.
            let reorged =
                start == old + 1 && blocks.first().is_some_and(|b| b.prev_hash != *old_hash);
            if reorged {
                let at = old.saturating_sub(REORG_DEPTH).max(floor);
                let _ = tx.send(Update::Reorg { at_height: at });
                return ix.block_range_pools(at, tip).await;
            }
            Ok(blocks)
        }
    }
}

/// The chain's consensus params: a canonical main/test schedule when the server
/// names one, else the featurenet (which also covers Crosslink).
fn chain_params(variant: Variant, params: Option<&NetworkParams>) -> ChainParams {
    match variant {
        Variant::Crosslink => ChainParams::featurenet(),
        Variant::Canonical => params
            .and_then(|p| ChainParams::canonical(&p.chain_name))
            .unwrap_or_else(ChainParams::featurenet),
    }
}

/// The search task: build the clients once, then serve requests as they arrive.
pub async fn run_search(
    url: String,
    variant: Variant,
    mut rx: UnboundedReceiver<Request>,
    tx: UnboundedSender<Update>,
) {
    let client = match SearchClient::connect(&url, variant).await {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Update::SearchError(format!("search unavailable: {e:#}")));
            return;
        }
    };
    while let Some(req) = rx.recv().await {
        let update = client.serve(req).await;
        if tx.send(update).is_err() {
            return;
        }
    }
}

async fn connect(url: &str, variant: Variant) -> Result<Ix> {
    let channel = endpoint(url)?
        .connect()
        .await
        .with_context(|| format!("connecting to {url}"))?;
    Ok(match variant {
        Variant::Canonical => Ix::Canonical(CanonicalIndexerClient::new(channel, empty_params())),
        Variant::Crosslink => Ix::Crosslink(CrosslinkIndexerClient::new(channel, empty_params())),
    })
}

/// Build an endpoint; TLS follows the scheme, matching lwcli.
fn endpoint(url: &str) -> Result<Endpoint> {
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("invalid endpoint url {url}"))?
        .connect_timeout(Duration::from_secs(10));
    if url.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .context("tls configuration")?;
    }
    Ok(endpoint)
}

/// Params are filled from the server via `params()`; construction only needs a
/// placeholder, the same shortcut lwcli takes.
fn empty_params() -> NetworkParams {
    NetworkParams {
        chain_name: String::new(),
        activation_heights: Default::default(),
        consensus_branch_id: 0,
    }
}

/// One of the two concrete indexer clients, hiding the variant behind a
/// normalized surface.
enum Ix {
    Canonical(CanonicalIndexerClient<Channel>),
    Crosslink(CrosslinkIndexerClient<Channel>),
}

/// A compact tx's input and output counts, summed across pools. Orchard and
/// ironwood actions count on both sides (each carries a nullifier and a note
/// commitment). The two variants' `CompactTx` share the fields but no type, so
/// a trait is the seam.
trait CompactTxCounts {
    fn inputs(&self) -> usize;
    fn outputs(&self) -> usize;
    /// The server-computed fee, `0` when it couldn't provide one.
    fn fee_zats(&self) -> u32;
    /// The txid in protocol (internal) byte order.
    fn txid_bytes(&self) -> &[u8];
}

macro_rules! impl_compact_tx_counts {
    ($ty:ty) => {
        impl CompactTxCounts for $ty {
            fn inputs(&self) -> usize {
                self.vin.len()
                    + self.spends.len()
                    + self.actions.len()
                    + self.ironwood_actions.len()
            }
            fn outputs(&self) -> usize {
                self.vout.len()
                    + self.outputs.len()
                    + self.actions.len()
                    + self.ironwood_actions.len()
            }
            fn fee_zats(&self) -> u32 {
                self.fee
            }
            fn txid_bytes(&self) -> &[u8] {
                &self.txid
            }
        }
    };
}

impl_compact_tx_counts!(lightwallet_core::proto::canonical::CompactTx);
impl_compact_tx_counts!(lightwallet_core::proto::crosslink::CompactTx);

fn tx_inputs<T: CompactTxCounts>(t: &T) -> usize {
    t.inputs()
}

fn tx_outputs<T: CompactTxCounts>(t: &T) -> usize {
    t.outputs()
}

/// The server-provided fee for the tx in `vtx` whose display-order txid matches,
/// or `None` when it isn't there or the server gave no fee.
fn block_fee<T: CompactTxCounts>(vtx: &[T], txid_display: &str) -> Option<i64> {
    vtx.iter()
        .find(|t| {
            let mut id = t.txid_bytes().to_vec();
            id.reverse();
            hex::encode(id) == txid_display
        })
        .map(|t| t.fee_zats())
        .filter(|fee| *fee != 0)
        .map(i64::from)
}

/// Project a compact block into a [`BlockRow`], counting inputs and outputs.
macro_rules! block_row {
    ($b:expr) => {{
        let b = $b;
        BlockRow {
            // Assigned by the app on insert; the network side leaves it 0.
            seq: 0,
            height: b.height,
            time: b.time,
            hash: b.hash.clone(),
            prev_hash: b.prev_hash.clone(),
            txs: b.vtx.len(),
            inputs: b.vtx.iter().map(tx_inputs).sum(),
            outputs: b.vtx.iter().map(tx_outputs).sum(),
            tx_rows: b
                .vtx
                .iter()
                .map(|t| {
                    // Txids are stored in protocol order; reverse for display.
                    let mut id = t.txid.clone();
                    id.reverse();
                    BlockTx {
                        txid: hex::encode(id),
                        inputs: tx_inputs(t),
                        outputs: tx_outputs(t),
                    }
                })
                .collect(),
            reorged: false,
        }
    }};
}

impl Ix {
    async fn params(&self) -> lightwallet_core::Result<NetworkParams> {
        match self {
            Ix::Canonical(c) => c.discover_params().await,
            Ix::Crosslink(c) => c.discover_params().await,
        }
    }

    async fn latest_height(&self) -> lightwallet_core::Result<u64> {
        match self {
            Ix::Canonical(c) => c.get_latest_height().await,
            Ix::Crosslink(c) => c.get_latest_height().await,
        }
    }

    async fn block_time(&self, height: u64) -> lightwallet_core::Result<u32> {
        match self {
            Ix::Canonical(c) => c.get_block(height).await.map(|b| b.time),
            Ix::Crosslink(c) => c.get_block(height).await.map(|b| b.time),
        }
    }

    /// `(block_height, estimated_height)` from `GetLightdInfo`.
    async fn lightd_info(&self) -> lightwallet_core::Result<(u64, u64)> {
        match self {
            Ix::Canonical(c) => c
                .get_lightd_info()
                .await
                .map(|i| (i.block_height, i.estimated_height)),
            Ix::Crosslink(c) => c
                .get_lightd_info()
                .await
                .map(|i| (i.block_height, i.estimated_height)),
        }
    }

    /// Blocks `[start, end]` with all pools, projected to [`BlockRow`]s. Requests
    /// every pool so the list's transparent counts are populated where the
    /// server can prune; a server that can't returns shielded-only and the
    /// transparent column reads 0.
    async fn block_range_pools(&self, start: u64, end: u64) -> Result<Vec<BlockRow>> {
        match self {
            Ix::Canonical(c) => {
                use lightwallet_core::proto::canonical::PoolType;
                let pools = vec![
                    PoolType::Transparent,
                    PoolType::Sapling,
                    PoolType::Orchard,
                    PoolType::Ironwood,
                ];
                let mut stream = c.get_block_range_pools(start, end, pools).await?;
                let mut out = Vec::new();
                while let Some(item) = stream.next().await {
                    out.push(block_row!(&item?));
                }
                Ok(out)
            }
            Ix::Crosslink(c) => {
                use lightwallet_core::proto::crosslink::PoolType;
                let pools = vec![
                    PoolType::Transparent,
                    PoolType::Sapling,
                    PoolType::Orchard,
                    PoolType::Ironwood,
                ];
                let mut stream = c.get_block_range_pools(start, end, pools).await?;
                let mut out = Vec::new();
                while let Some(item) = stream.next().await {
                    out.push(block_row!(&item?));
                }
                Ok(out)
            }
        }
    }

    async fn mempool(
        &self,
    ) -> lightwallet_core::Result<BoxStream<'static, lightwallet_core::Result<Raw>>> {
        match self {
            Ix::Canonical(c) => Ok(c
                .get_mempool_stream()
                .await?
                .map(|r| {
                    r.map(|t| Raw {
                        data: t.data,
                        height: t.height,
                    })
                })
                .boxed()),
            Ix::Crosslink(c) => Ok(c
                .get_mempool_stream()
                .await?
                .map(|r| {
                    r.map(|t| Raw {
                        data: t.data,
                        height: t.height,
                    })
                })
                .boxed()),
        }
    }
}

/// The search task's clients: an indexer for block lookups and an identity
/// client for txid / t-address lookups, plus the chain params for parsing.
enum SearchClient {
    Canonical {
        ix: CanonicalIndexerClient<Channel>,
        id: CanonicalIdentityClient<Channel>,
        chain: ChainParams,
    },
    Crosslink {
        ix: CrosslinkIndexerClient<Channel>,
        id: CrosslinkIdentityClient<Channel>,
        chain: ChainParams,
    },
}

impl SearchClient {
    async fn connect(url: &str, variant: Variant) -> Result<Self> {
        let ep = endpoint(url)?;
        let channel = ep
            .clone()
            .connect()
            .await
            .with_context(|| format!("connecting to {url}"))?;
        Ok(match variant {
            Variant::Canonical => {
                let ix = CanonicalIndexerClient::new(channel, empty_params());
                let chain = chain_params(variant, ix.discover_params().await.ok().as_ref());
                let id = CanonicalIdentityClient::new(IdentityTransport::connect_lazy(ep));
                SearchClient::Canonical { ix, id, chain }
            }
            Variant::Crosslink => {
                let ix = CrosslinkIndexerClient::new(channel, empty_params());
                let id = CrosslinkIdentityClient::new(IdentityTransport::connect_lazy(ep));
                SearchClient::Crosslink {
                    ix,
                    id,
                    chain: ChainParams::featurenet(),
                }
            }
        })
    }

    fn chain(&self) -> &ChainParams {
        match self {
            SearchClient::Canonical { chain, .. } | SearchClient::Crosslink { chain, .. } => chain,
        }
    }

    async fn serve(&self, req: Request) -> Update {
        match req {
            Request::Block(height) => self.serve_block(height).await,
            Request::Transaction(hex) => self.serve_tx(hex).await,
            Request::Taddr { addr, start, end } => self.serve_taddr(addr, start, end).await,
        }
    }

    async fn serve_block(&self, height: u64) -> Update {
        let row = match self {
            SearchClient::Canonical { ix, .. } => {
                ix.get_block(height).await.map(|b| block_row!(&b))
            }
            SearchClient::Crosslink { ix, .. } => {
                ix.get_block(height).await.map(|b| block_row!(&b))
            }
        };
        match row {
            Ok(row) => Update::SearchBlock(row),
            Err(_) => Update::SearchError(format!("No tx or block matched {height}")),
        }
    }

    async fn serve_tx(&self, display_hex: String) -> Update {
        // Reverse display hex to internal byte order for TxFilter.hash.
        let Some(mut bytes) = hex::decode(&display_hex).ok().filter(|b| b.len() == 32) else {
            return Update::SearchError(format!("No tx or block matched {display_hex}"));
        };
        bytes.reverse();
        let txid = lightwallet_core::Txid::new(bytes);
        // Each variant returns its own RawTransaction type, so normalize to
        // (data, height) inside the arm before leaving the match.
        let raw = match self {
            SearchClient::Canonical { id, .. } => {
                id.get_transaction(txid).await.map(|r| (r.data, r.height))
            }
            SearchClient::Crosslink { id, .. } => {
                id.get_transaction(txid).await.map(|r| (r.data, r.height))
            }
        };
        match raw {
            Ok((data, height)) => {
                // An unmined result (height 0) decodes under the tip's rules; the
                // chain params yield the tip branch for a high height.
                let h = if height == 0 {
                    end_of_chain()
                } else {
                    height as u32
                };
                let mut parsed =
                    lightwallet_txview::parse(&data, BlockHeight::from_u32(h), self.chain());
                // A tx with transparent inputs can't be priced from its bytes;
                // the mining block carries the server-computed fee, so fetch it.
                if parsed.fee.is_none() && height > 0 {
                    parsed.fee = self.mined_fee(height, parsed.txid.as_deref()).await;
                }
                Update::SearchTx(TxDetail { parsed, raw: data })
            }
            Err(_) => Update::SearchError(format!("No tx or block matched {display_hex}")),
        }
    }

    /// The server-provided fee for a mined tx, read from its block's compact
    /// data. `None` when the block or tx is unavailable or carried no fee.
    async fn mined_fee(&self, height: u64, txid: Option<&str>) -> Option<i64> {
        let txid = txid?;
        match self {
            SearchClient::Canonical { ix, .. } => {
                block_fee(&ix.get_block(height).await.ok()?.vtx, txid)
            }
            SearchClient::Crosslink { ix, .. } => {
                block_fee(&ix.get_block(height).await.ok()?.vtx, txid)
            }
        }
    }

    async fn serve_taddr(&self, addr: String, start: u64, end: u64) -> Update {
        let raws = match self {
            SearchClient::Canonical { id, .. } => {
                drain_taddr(id.get_taddress_transactions(addr.clone(), start, end).await).await
            }
            SearchClient::Crosslink { id, .. } => {
                drain_taddr(id.get_taddress_transactions(addr.clone(), start, end).await).await
            }
        };
        let raws = match raws {
            Ok(raws) => raws,
            Err(e) => return Update::SearchError(format!("t-addr lookup failed: {e}")),
        };
        let hits = raws
            .into_iter()
            .map(|(data, height)| {
                let height = if height == 0 { end } else { height };
                let parsed = lightwallet_txview::parse(
                    &data,
                    BlockHeight::from_u32(height as u32),
                    self.chain(),
                );
                TaddrHit {
                    height,
                    detail: TxDetail { parsed, raw: data },
                }
            })
            .collect();
        Update::SearchTaddr {
            addr,
            window: crate::app::SEARCH_WINDOW,
            hits,
        }
    }
}

/// A large height standing in for an unmined tx, so the chain params resolve to
/// the tip branch.
fn end_of_chain() -> u32 {
    u32::MAX / 2
}

/// Drain a t-address transaction stream into `(data, height)` pairs, stopping at
/// the first error item.
async fn drain_taddr<S>(
    stream: lightwallet_core::Result<BoxStream<'static, lightwallet_core::Result<S>>>,
) -> lightwallet_core::Result<Vec<(Vec<u8>, u64)>>
where
    S: RawTx,
{
    let mut stream = stream?;
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(r) => {
                let height = r.height();
                out.push((r.data(), height));
            }
            Err(_) => break,
        }
    }
    Ok(out)
}

/// The two variants' `RawTransaction` share these fields but no trait; this is
/// the seam that lets one drain serve both.
trait RawTx {
    fn data(self) -> Vec<u8>;
    fn height(&self) -> u64;
}

impl RawTx for lightwallet_core::proto::canonical::RawTransaction {
    fn data(self) -> Vec<u8> {
        self.data
    }
    fn height(&self) -> u64 {
        self.height
    }
}

impl RawTx for lightwallet_core::proto::crosslink::RawTransaction {
    fn data(self) -> Vec<u8> {
        self.data
    }
    fn height(&self) -> u64 {
        self.height
    }
}
