//! The producer half: hold a connection open, drain `GetMempoolStream`, and
//! push updates to the UI over a channel. A clean stream end is a block
//! boundary (reconnect at once, repopulate); an error backs off.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use lightwallet_core::{
    CanonicalIndexerClient, CrosslinkIndexerClient, IndexerClient, NetworkParams,
};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

use std::time::Instant;

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

/// The reconnect loop. Returns when the UI drops the receiver.
pub async fn run(url: String, variant: Variant, tx: UnboundedSender<Update>) {
    let mut backoff = BACKOFF_START;
    loop {
        if tx.send(Update::Phase(Phase::Connecting)).is_err() {
            return;
        }
        match cycle(&url, variant, &tx).await {
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

async fn cycle(url: &str, variant: Variant, tx: &UnboundedSender<Update>) -> Result<Ended, String> {
    let ix = connect(url, variant).await.map_err(|e| format!("{e:#}"))?;

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

    let mut stream = ix.mempool().await.map_err(|e| format!("{e:#}"))?;
    while let Some(item) = stream.next().await {
        let raw = item.map_err(|e| format!("{e}"))?;
        let row = Row {
            seq: 0,
            first_seen: Instant::now(),
            tx: lightwallet_txview::parse(&raw.data, branch),
        };
        if tx.send(Update::Tx(row)).is_err() {
            return Ok(Ended::Closed);
        }
    }
    Ok(Ended::Block)
}

async fn connect(url: &str, variant: Variant) -> Result<Ix> {
    let channel = channel(url).await?;
    Ok(match variant {
        Variant::Canonical => Ix::Canonical(CanonicalIndexerClient::new(channel, empty_params())),
        Variant::Crosslink => Ix::Crosslink(CrosslinkIndexerClient::new(channel, empty_params())),
    })
}

/// Direct transport only for now. TLS follows the scheme, matching lwcli.
async fn channel(url: &str) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("invalid endpoint url {url}"))?
        .connect_timeout(Duration::from_secs(10));
    if url.starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .context("tls configuration")?;
    }
    endpoint
        .connect()
        .await
        .with_context(|| format!("connecting to {url}"))
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
