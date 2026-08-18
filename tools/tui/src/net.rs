//! The producer half: hold a connection open, drain `GetMempoolStream`, and
//! push updates to the UI over a channel. A clean stream end is a block
//! boundary (reconnect at once, repopulate); an error backs off.
//!
//! A mempool transaction's bytes carry its transparent outputs' values but not
//! its inputs': an input names the funding output it spends. This producer
//! follows those references so a row shows the transparent input total and an
//! exact fee, not the omission ADR 0003 settled for. Resolution is eager (every
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
use lightwallet_txview::{OutPoint, parse};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use crate::app::{Phase, Row, Update};

/// Which protocol surface the endpoint serves.
#[derive(Clone, Copy)]
pub enum Variant {
    Canonical,
    Crosslink,
}

/// A mempool transaction, normalized off both variants' generated types so
/// nothing downstream is variant-aware.
struct Raw {
    data: Vec<u8>,
}

const BACKOFF_MAX: Duration = Duration::from_secs(10);
const BACKOFF_START: Duration = Duration::from_millis(500);

/// The most funding lookups in flight at once. Bounds the block-boundary burst,
/// so a refill of transparent-input transactions queues behind this rather than
/// opening a socket per input.
const RESOLVE_CONCURRENCY: usize = 12;

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

/// The reconnect loop. Returns when the UI drops the receiver.
pub async fn run(url: String, variant: Variant, tx: UnboundedSender<Update>) {
    let endpoint = match build_endpoint(&url) {
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
    let id = Arc::new(build_identity(variant, endpoint.clone()));
    let cache: FundingCache = Arc::new(Mutex::new(HashMap::new()));
    let sem = Arc::new(Semaphore::new(RESOLVE_CONCURRENCY));
    let mut next_seq: u64 = 0;

    let mut backoff = BACKOFF_START;
    loop {
        if tx.send(Update::Phase(Phase::Connecting)).is_err() {
            return;
        }
        match cycle(&endpoint, variant, &tx, &id, &cache, &sem, &mut next_seq).await {
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

#[allow(clippy::too_many_arguments)]
async fn cycle(
    endpoint: &Endpoint,
    variant: Variant,
    tx: &UnboundedSender<Update>,
    id: &Arc<Id>,
    cache: &FundingCache,
    sem: &Arc<Semaphore>,
    next_seq: &mut u64,
) -> Result<Ended, String> {
    let ix = connect(endpoint, variant)
        .await
        .map_err(|e| format!("{e:#}"))?;

    // Branch id for the parser and the current tip for the header. Both come
    // from the server; a failure here is just a reconnect, not fatal.
    let branch = ix
        .params()
        .await
        .map(|p| p.consensus_branch_id)
        .unwrap_or(0);
    let tip = ix.latest_height().await.unwrap_or(0);
    // The tip block's miner timestamp, so "since block" counts from when the
    // block was actually mined, not from when we noticed. Best-effort: a failed
    // fetch just leaves the clock blank this cycle.
    let mined_unix = if tip > 0 {
        ix.block_time(tip).await.ok()
    } else {
        None
    };

    // Block marker clears the prior tip's pending set before the fresh stream
    // repopulates it.
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
        let parsed = parse(&raw.data, branch);
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
                branch,
                sem,
                tx.clone(),
            );
        }

        let row = Row {
            seq,
            first_seen: Instant::now(),
            tx: parsed,
        };
        if tx.send(Update::Tx(Box::new(row))).is_err() {
            return Ok(Ended::Closed);
        }
    }
    Ok(Ended::Block)
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
    branch: u32,
    sem: &Arc<Semaphore>,
    tx: UnboundedSender<Update>,
) {
    let in_stream = Arc::clone(in_stream);
    let cache = Arc::clone(cache);
    let id = Arc::clone(id);
    let sem = Arc::clone(sem);
    tokio::spawn(async move {
        // A closed semaphore only happens on shutdown; drop the job.
        let Ok(_permit) = sem.acquire_owned().await else {
            return;
        };
        let mut total: i64 = 0;
        for op in &prevouts {
            match funder_value(op, &in_stream, &cache, &id, branch).await {
                Some(value) => total += value,
                // One prevout we cannot read makes the whole total (and the fee)
                // unresolvable. Report that, never a wrong partial sum.
                None => {
                    let _ = tx.send(Update::ValueResolved { seq, total: None });
                    return;
                }
            }
        }
        let _ = tx.send(Update::ValueResolved {
            seq,
            total: Some(total),
        });
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
    branch: u32,
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
    match id.funder_values(op, branch).await {
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

async fn connect(endpoint: &Endpoint, variant: Variant) -> Result<Ix> {
    let channel = endpoint.connect().await.context("connecting to indexer")?;
    Ok(match variant {
        Variant::Canonical => Ix::Canonical(CanonicalIndexerClient::new(channel, empty_params())),
        Variant::Crosslink => Ix::Crosslink(CrosslinkIndexerClient::new(channel, empty_params())),
    })
}

/// Direct transport only for now. TLS follows the scheme, matching lwcli.
fn build_endpoint(url: &str) -> Result<Endpoint> {
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

    /// The miner-reported Unix time of the block at `height`.
    async fn block_time(&self, height: u64) -> lightwallet_core::Result<u32> {
        match self {
            Ix::Canonical(c) => c.get_block(height).await.map(|b| b.time),
            Ix::Crosslink(c) => c.get_block(height).await.map(|b| b.time),
        }
    }

    async fn mempool(
        &self,
    ) -> lightwallet_core::Result<BoxStream<'static, lightwallet_core::Result<Raw>>> {
        match self {
            Ix::Canonical(c) => Ok(c
                .get_mempool_stream()
                .await?
                .map(|r| r.map(|t| Raw { data: t.data }))
                .boxed()),
            Ix::Crosslink(c) => Ok(c
                .get_mempool_stream()
                .await?
                .map(|r| r.map(|t| Raw { data: t.data }))
                .boxed()),
        }
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
    async fn funder_values(&self, op: &OutPoint, branch: u32) -> Funder {
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
        let parsed = parse(&data, branch);
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
