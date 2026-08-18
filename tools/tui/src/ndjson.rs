//! The machine-readable consumer: the same `Update` stream the TUI renders,
//! serialized one JSON object per line to stdout. A `block` event marks each
//! mempool boundary; `mined_block`, `info`, and `reorg` carry the explorer
//! tail. Search events never occur here (NDJSON issues no queries), so they are
//! dropped.

use std::io::Write;

use anyhow::Result;
use lightwallet_txview::ParsedTx;
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Phase, Update};

/// One line of the feed. `type` discriminates; a consumer switches on it.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    /// A mempool block boundary. The pending set that preceded it is now stale.
    Block { height: u64, time: Option<u32> },
    /// A mempool transaction.
    Tx(ParsedTx),
    /// A block entering the explorer tail.
    MinedBlock {
        height: u64,
        time: u32,
        txs: usize,
        inputs: usize,
        outputs: usize,
    },
    /// A `GetLightdInfo` sample.
    Info {
        block_height: u64,
        estimated_height: u64,
    },
    /// A reorg replaced the chain from `at_height`.
    Reorg { at_height: u64 },
    /// A connection state change.
    Status {
        state: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Map an update to a feed line, or `None` for updates with no NDJSON form
/// (the search results, which this mode never requests).
fn event_of(update: Update) -> Option<Event> {
    Some(match update {
        Update::Block { tip, mined_unix } => Event::Block {
            height: tip,
            time: mined_unix,
        },
        Update::Tx(row) => Event::Tx(row.tx),
        Update::MinedBlock(b) => Event::MinedBlock {
            height: b.height,
            time: b.time,
            txs: b.txs,
            inputs: b.inputs,
            outputs: b.outputs,
        },
        Update::Info {
            block_height,
            estimated_height,
        } => Event::Info {
            block_height,
            estimated_height,
        },
        Update::Reorg { at_height } => Event::Reorg { at_height },
        Update::Phase(Phase::Connecting) => Event::Status {
            state: "connecting",
            detail: None,
        },
        Update::Phase(Phase::Live) => Event::Status {
            state: "live",
            detail: None,
        },
        Update::Phase(Phase::Reconnecting(err)) => Event::Status {
            state: "reconnecting",
            detail: Some(err),
        },
        Update::SearchTx(_)
        | Update::SearchBlock(_)
        | Update::SearchTaddr { .. }
        | Update::SearchError(_) => return None,
    })
}

/// Serialize each update as it arrives, flushing per line so a pipe sees the
/// feed live. Exits cleanly when the reader closes the pipe.
pub async fn run(mut rx: UnboundedReceiver<Update>) -> Result<()> {
    let mut out = std::io::stdout().lock();
    while let Some(update) = rx.recv().await {
        let Some(event) = event_of(update) else {
            continue;
        };
        let line = serde_json::to_string(&event)?;
        if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(e.into());
        }
    }
    Ok(())
}
