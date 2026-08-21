//! The producer half. Two tasks feed the UI over one `Update` channel:
//!
//! - the **tail** holds a connection open, seeds and gap-fills the block ring off
//!   the mempool-stream close edge, samples chain health, detects reorgs, and
//!   drains `GetMempoolStream`. A clean stream end is a block boundary (reconnect
//!   at once and fill the gap); an error backs off.
//! - the **search** task services on-demand block / txid / t-address lookups,
//!   independent of the tail loop.
//!
//! A mempool transaction's bytes carry its transparent outputs' values but not
//! its inputs': an input names the funding output it spends. The tail follows
//! those references so a row shows the transparent input total and an exact fee.
//! Resolution is eager (every
//! transparent-input tx as it streams) and identity-bearing (`GetTransaction`),
//! so it rides a dedicated identity client, not the identity-free sync stream.
//! Two shortcuts keep the burst survivable: a funding-txid cache, immutable so
//! it never invalidates, and an in-stream index that resolves an unconfirmed
//! parent already in the pending set with no RPC at all.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use lightwallet_core::{
    CanonicalIdentityClient, CanonicalIndexerClient, CrosslinkIdentityClient,
    CrosslinkIndexerClient, IdentityTransport, IndexerClient, NetworkParams, Txid,
};
use lightwallet_txview::{BlockHeight, ChainParams, InputTotal, OutPoint, parse};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

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
    /// A txid as display-order hex (no `0x`). Supersedes any earlier
    /// `Transaction` still resolving: its funder lookups are aborted.
    Transaction(String),
    /// The drill closed, so whatever its tx was still resolving is wasted
    /// work: abort it.
    CloseTx,
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

/// The most funding lookups in flight at once, across the mempool resolvers and
/// the drill together. Bounds the block-boundary burst and a many-input drill
/// alike: funders queue behind this rather than opening a stream per input.
const RESOLVE_CONCURRENCY: usize = 16;

/// The shared funding-lookup bound. One per process, handed to both tasks.
pub type ResolveGate = Arc<Semaphore>;

pub fn resolve_gate() -> ResolveGate {
    Arc::new(Semaphore::new(RESOLVE_CONCURRENCY))
}

/// Funding txid (display order) to its transparent output values. `None` marks a
/// funder we looked up and could not read, so a second input spending it does
/// not refetch. Confirmed outputs are immutable, so nothing here goes stale.
type FundingCache = Arc<Mutex<HashMap<String, Option<Vec<i64>>>>>;

/// Funding txid (display order) to its transparent output values, for the
/// transactions in the current pending set. An input funded by an unconfirmed
/// parent resolves here for free. Rebuilt each cycle, since the pending set is
/// relative to the tip.
type InStream = Arc<Mutex<HashMap<String, Vec<i64>>>>;

/// Lock a resolver map, recovering the inner data if a holder panicked. These
/// maps carry no invariant a panic could break, so a poisoned lock is worth
/// stepping over rather than propagating.
fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn new_cache() -> FundingCache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// The tail reconnect loop. Returns when the UI drops the receiver.
pub async fn run(url: String, variant: Variant, gate: ResolveGate, tx: UnboundedSender<Update>) {
    let endpoint = match endpoint(&url) {
        Ok(endpoint) => endpoint,
        // A malformed url will not fix itself; report it and stop rather than
        // back off forever.
        Err(e) => {
            let _ = tx.send(Update::Phase(Phase::Reconnecting(format!("{e:#}"))));
            return;
        }
    };
    // One identity client for the whole session: every funding lookup shares one
    // unlinkability domain, and the cache and concurrency bound span reconnects.
    let id = Arc::new(build_identity(variant, endpoint));
    let cache: FundingCache = Arc::new(Mutex::new(HashMap::new()));
    let sem = gate;
    let mut next_seq: u64 = 0;

    let mut backoff = BACKOFF_START;
    // Carried across cycles: the tip height and hash last seen, so a fill knows
    // where the gap starts and whether the chain reorged under it.
    let mut last: Option<(u64, Vec<u8>)> = None;
    // A clean stream end is a block boundary, not a lost connection. Reopening
    // the stream then is routine, so don't flip the phase back to Connecting for
    // it, or a fast chain reads as perpetual reconnecting. Only cold start and a
    // real error announce Connecting.
    let mut block_boundary = false;
    loop {
        if !block_boundary && tx.send(Update::Phase(Phase::Connecting)).is_err() {
            return;
        }
        block_boundary = false;
        match cycle(
            &url,
            variant,
            &tx,
            &mut last,
            &id,
            &cache,
            &sem,
            &mut next_seq,
        )
        .await
        {
            Ok(Ended::Block) => {
                backoff = BACKOFF_START;
                block_boundary = true;
            }
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

#[allow(clippy::too_many_arguments)]
async fn cycle(
    url: &str,
    variant: Variant,
    tx: &UnboundedSender<Update>,
    last: &mut Option<(u64, Vec<u8>)>,
    id: &Arc<Id>,
    cache: &FundingCache,
    sem: &Arc<Semaphore>,
    next_seq: &mut u64,
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

    // Fresh per cycle: the pending set is relative to this tip.
    let in_stream: InStream = Arc::new(Mutex::new(HashMap::new()));

    let mut stream = ix.mempool().await.map_err(|e| format!("{e:#}"))?;
    while let Some(item) = stream.next().await {
        let raw = item.map_err(|e| format!("{e}"))?;
        // A mempool tx is height 0; decode it under the tip's rules.
        let height = if raw.height == 0 { tip } else { raw.height };
        let block_height = BlockHeight::from_u32(height as u32);
        let parsed = parse(&raw.data, block_height, &chain);
        let seq = *next_seq;
        *next_seq += 1;

        // Offer this tx's outputs to later spenders in the same pending set
        // before dispatching its own inputs, so a child streamed just after its
        // parent resolves for free.
        if let Some(txid) = &parsed.txid
            && !parsed.vout_values.is_empty()
        {
            guard(&in_stream).insert(txid.clone(), parsed.vout_values.clone());
        }
        if !parsed.prevouts.is_empty() {
            spawn_resolve(
                seq,
                parsed.prevouts.clone(),
                &in_stream,
                cache,
                id,
                chain,
                block_height,
                sem,
                tx.clone(),
            );
        }

        let row = Row {
            seq,
            first_seen: Instant::now(),
            tx: parsed,
            raw: raw.data,
        };
        if tx.send(Update::Tx(Box::new(row))).is_err() {
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

/// The search task: build the clients once, then serve each request on its own
/// task, so a tx whose funders are still resolving never holds up the next
/// lookup. At most one tx resolves at a time: a new `Transaction` (or `CloseTx`)
/// aborts the one before it, since the pane it fed is gone.
pub async fn run_search(
    url: String,
    variant: Variant,
    gate: ResolveGate,
    mut rx: UnboundedReceiver<Request>,
    tx: UnboundedSender<Update>,
) {
    let client = match SearchClient::connect(&url, variant, gate).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            let _ = tx.send(Update::SearchError(format!("search unavailable: {e:#}")));
            return;
        }
    };
    let mut tx_job: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(req) = rx.recv().await {
        let client = Arc::clone(&client);
        let tx = tx.clone();
        match req {
            Request::Transaction(hex) => {
                if let Some(job) =
                    tx_job.replace(tokio::spawn(async move { client.serve_tx(hex, &tx).await }))
                {
                    job.abort();
                }
            }
            Request::CloseTx => {
                if let Some(job) = tx_job.take() {
                    job.abort();
                }
            }
            Request::Block(height) => {
                tokio::spawn(async move {
                    let _ = tx.send(client.serve_block(height).await);
                });
            }
            Request::Taddr { addr, start, end } => {
                tokio::spawn(async move {
                    let _ = tx.send(client.serve_taddr(addr, start, end).await);
                });
            }
        }
    }
    if let Some(job) = tx_job {
        job.abort();
    }
}

/// Spawn a bounded task that follows one transaction's prevouts and reports the
/// summed transparent input total back to the row with this `seq`.
#[allow(clippy::too_many_arguments)]
fn spawn_resolve(
    seq: u64,
    prevouts: Vec<OutPoint>,
    in_stream: &InStream,
    cache: &FundingCache,
    id: &Arc<Id>,
    chain: ChainParams,
    height: BlockHeight,
    sem: &Arc<Semaphore>,
    tx: UnboundedSender<Update>,
) {
    let in_stream = Arc::clone(in_stream);
    let cache = Arc::clone(cache);
    let id = Arc::clone(id);
    let sem = Arc::clone(sem);
    tokio::spawn(async move {
        // Each funder takes its own permit, so the gate bounds lookups rather
        // than transactions: one wide tx can't hog it, and many narrow ones
        // still fan out. Outpoints move into the closure: a borrowed param would
        // pin the async block to a late-bound lifetime the spawned task can't carry.
        let values = futures_util::stream::iter(prevouts)
            .map(|op| {
                let sem = &sem;
                let in_stream = &in_stream;
                let cache = &cache;
                let id = &id;
                let chain = &chain;
                async move {
                    // A closed semaphore only happens on shutdown; drop the job.
                    let _permit = sem.acquire().await.ok()?;
                    funder_value(&op, in_stream, cache, id, chain, height).await
                }
            })
            .buffer_unordered(RESOLVE_CONCURRENCY)
            // One prevout we cannot read makes the whole total (and the fee)
            // unresolvable. Report that, never a wrong partial sum.
            .collect::<Vec<Option<i64>>>()
            .await;
        let total = values.into_iter().sum::<Option<i64>>();
        let _ = tx.send(Update::ValueResolved { seq, total });
    });
}

/// The value of the output an input spends: from the pending set if its funder
/// is there, else the cache, else a `GetTransaction`. `None` when the funder
/// cannot be read or the index is out of range.
async fn funder_value(
    op: &OutPoint,
    in_stream: &InStream,
    cache: &FundingCache,
    id: &Id,
    chain: &ChainParams,
    height: BlockHeight,
) -> Option<i64> {
    let key = op.txid();
    let idx = op.index as usize;

    if let Some(value) = guard(in_stream)
        .get(&key)
        .and_then(|vals| vals.get(idx).copied())
    {
        return Some(value);
    }
    if let Some(entry) = guard(cache).get(&key) {
        return entry.as_ref().and_then(|vals| vals.get(idx).copied());
    }

    // Cache a definite answer (read or genuinely absent), so no spender refetches
    // it. A transient failure is not cached, so a later spender can still succeed
    // rather than inheriting a wrong "unresolvable".
    match id.funder_values(op, chain, height).await {
        Funder::Values(vals) => {
            let value = vals.get(idx).copied();
            guard(cache).insert(key, Some(vals));
            value
        }
        Funder::Absent => {
            guard(cache).insert(key, None);
            None
        }
        Funder::Unavailable => None,
    }
}

fn build_identity(variant: Variant, endpoint: Endpoint) -> Id {
    match variant {
        Variant::Canonical => Id::Canonical(CanonicalIdentityClient::new(
            IdentityTransport::connect_lazy(endpoint),
        )),
        Variant::Crosslink => Id::Crosslink(CrosslinkIdentityClient::new(
            IdentityTransport::connect_lazy(endpoint),
        )),
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
        cache: FundingCache,
        gate: ResolveGate,
    },
    Crosslink {
        ix: CrosslinkIndexerClient<Channel>,
        id: CrosslinkIdentityClient<Channel>,
        chain: ChainParams,
        cache: FundingCache,
        gate: ResolveGate,
    },
}

impl SearchClient {
    async fn connect(url: &str, variant: Variant, gate: ResolveGate) -> Result<Self> {
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
                SearchClient::Canonical {
                    ix,
                    id,
                    chain,
                    cache: new_cache(),
                    gate,
                }
            }
            Variant::Crosslink => {
                let ix = CrosslinkIndexerClient::new(channel, empty_params());
                let id = CrosslinkIdentityClient::new(IdentityTransport::connect_lazy(ep));
                SearchClient::Crosslink {
                    ix,
                    id,
                    chain: ChainParams::featurenet(),
                    cache: new_cache(),
                    gate,
                }
            }
        })
    }

    fn chain(&self) -> &ChainParams {
        match self {
            SearchClient::Canonical { chain, .. } | SearchClient::Crosslink { chain, .. } => chain,
        }
    }

    fn cache(&self) -> &FundingCache {
        match self {
            SearchClient::Canonical { cache, .. } | SearchClient::Crosslink { cache, .. } => cache,
        }
    }

    fn gate(&self) -> &Semaphore {
        match self {
            SearchClient::Canonical { gate, .. } | SearchClient::Crosslink { gate, .. } => gate,
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

    async fn serve_tx(&self, display_hex: String, tx: &UnboundedSender<Update>) {
        // Reverse display hex to internal byte order for TxFilter.hash.
        let Some(mut bytes) = hex::decode(&display_hex).ok().filter(|b| b.len() == 32) else {
            let _ = tx.send(Update::SearchError(format!(
                "No tx or block matched {display_hex}"
            )));
            return;
        };
        bytes.reverse();
        let txid = Txid::new(bytes);
        // Each variant returns its own RawTransaction type; normalize to
        // (data, height) inside the arm before leaving the match.
        let raw = match self {
            SearchClient::Canonical { id, .. } => {
                id.get_transaction(txid).await.map(|r| (r.data, r.height))
            }
            SearchClient::Crosslink { id, .. } => {
                id.get_transaction(txid).await.map(|r| (r.data, r.height))
            }
        };
        let Ok((data, height)) = raw else {
            let _ = tx.send(Update::SearchError(format!(
                "No tx or block matched {display_hex}"
            )));
            return;
        };
        // An unmined result (height 0) decodes under the tip's rules; the chain
        // params yield the tip branch for a high height.
        let h = if height == 0 {
            end_of_chain()
        } else {
            height as u32
        };
        let parsed = lightwallet_txview::parse(&data, BlockHeight::from_u32(h), self.chain());
        let pending = matches!(parsed.input_total, InputTotal::Pending);
        let display_txid = parsed.txid.clone();
        let prevouts = parsed.prevouts.clone();
        let fee_base = parsed.fee_base;
        // Open the pane at once; the transparent input total streams in behind it.
        if tx
            .send(Update::SearchTx {
                detail: TxDetail { parsed, raw: data },
                height,
            })
            .is_err()
        {
            return;
        }
        // Only a mined tx with transparent inputs left pending needs resolving.
        let (Some(txid), true) = (display_txid, pending) else {
            return;
        };
        if prevouts.is_empty() {
            return;
        }
        // Follow the funders: this yields each input's value (for the pane) and,
        // when all read, the total. If a funder is unreadable, fall back to the
        // mining block's server fee for the total alone.
        let (mut total, values) = self
            .resolve_inputs(&txid, &prevouts, BlockHeight::from_u32(h), tx)
            .await;
        if total.is_none()
            && height > 0
            && let Some(fee) = self.mined_fee(height, Some(&txid)).await
            && let Some(base) = fee_base
        {
            total = Some(fee - base);
        }
        let _ = tx.send(Update::DrillResolved {
            txid,
            total,
            values,
        });
    }

    /// The raw bytes of a funding transaction, or `None` if the server can't
    /// return it.
    async fn funder_data(&self, txid: Txid) -> Option<Vec<u8>> {
        match self {
            SearchClient::Canonical { id, .. } => {
                id.get_transaction(txid).await.ok().map(|r| r.data)
            }
            SearchClient::Crosslink { id, .. } => {
                id.get_transaction(txid).await.ok().map(|r| r.data)
            }
        }
    }

    /// Follow each transparent input to the output it spends, returning its value
    /// per input (in vin order) and their sum. Funders are fetched concurrently
    /// under the shared gate, and each completion reports progress as a probe.
    /// The total is `Some` only when every funder read; a single unreadable
    /// funder leaves its value `None` and the total `None`. A definite answer
    /// (read or genuinely absent) is cached; a transport miss is not.
    async fn resolve_inputs(
        &self,
        spend_txid: &str,
        prevouts: &[OutPoint],
        height: BlockHeight,
        tx: &UnboundedSender<Update>,
    ) -> (Option<i64>, Vec<Option<i64>>) {
        let chain = *self.chain();
        let of = prevouts.len();
        let probe = |done: Option<usize>| {
            let _ = tx.send(Update::DrillProbe {
                txid: spend_txid.to_string(),
                progress: done.map(|d| (d, of)),
            });
        };
        probe(Some(0));
        let mut values = vec![None; of];
        let mut done = 0;
        let mut fetched = futures_util::stream::iter(prevouts.iter().cloned().enumerate())
            .map(|(i, op)| {
                let chain = &chain;
                async move { (i, self.funder_value(&op, height, chain).await) }
            })
            .buffer_unordered(RESOLVE_CONCURRENCY);
        while let Some((i, value)) = fetched.next().await {
            values[i] = value;
            done += 1;
            probe(Some(done));
        }
        probe(None);
        let total = values.iter().copied().sum::<Option<i64>>();
        (total, values)
    }

    /// One input's value: from the cache, else a gated `GetTransaction`.
    async fn funder_value(
        &self,
        op: &OutPoint,
        height: BlockHeight,
        chain: &ChainParams,
    ) -> Option<i64> {
        let key = op.txid();
        let idx = op.index as usize;
        if let Some(entry) = guard(self.cache()).get(&key) {
            return entry.as_ref().and_then(|vals| vals.get(idx).copied());
        }
        // A closed gate only happens on shutdown; report unreadable.
        let _permit = self.gate().acquire().await.ok()?;
        let data = self.funder_data(Txid::new(op.hash.to_vec())).await?;
        let vals = (!data.is_empty())
            .then(|| parse(&data, height, chain))
            .filter(|p| p.error.is_none())
            .map(|p| p.vout_values);
        let value = vals.as_ref().and_then(|v| v.get(idx).copied());
        guard(self.cache()).insert(key, vals);
        value
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

/// One of the two concrete identity clients, carrying the identity-bearing
/// `GetTransaction` used to read a confirmed funder's outputs.
enum Id {
    Canonical(CanonicalIdentityClient<Channel>),
    Crosslink(CrosslinkIdentityClient<Channel>),
}

impl Id {
    /// The transparent output values of the funding transaction `op` spends.
    /// Parsed under the spending tx's chain params: a confirmed funder's
    /// transparent output values read the same regardless of branch.
    async fn funder_values(
        &self,
        op: &OutPoint,
        chain: &ChainParams,
        height: BlockHeight,
    ) -> Funder {
        let txid = Txid::new(op.hash.to_vec());
        let data = match self {
            Id::Canonical(c) => c.get_transaction(txid).await.ok().map(|r| r.data),
            Id::Crosslink(c) => c.get_transaction(txid).await.ok().map(|r| r.data),
        };
        // A transport error might clear on retry; an empty or unreadable
        // response is a definite "the server cannot give me this funder".
        let Some(data) = data else {
            return Funder::Unavailable;
        };
        if data.is_empty() {
            return Funder::Absent;
        }
        let parsed = parse(&data, height, chain);
        if parsed.error.is_some() {
            return Funder::Absent;
        }
        Funder::Values(parsed.vout_values)
    }
}

/// The outcome of a funding lookup, split so a retryable failure is not cached
/// as a permanent one.
enum Funder {
    /// The funder's transparent output values.
    Values(Vec<i64>),
    /// The server has no readable transaction for this outpoint.
    Absent,
    /// The lookup failed transiently and may succeed later.
    Unavailable,
}
