//! The machine-readable consumer: the same `Update` stream the TUI renders,
//! serialized one JSON object per line to stdout. A `block` event marks each
//! boundary, so a consumer can reset its pending set and dedup the mempool the
//! server re-sends on reconnect.

use std::io::Write;

use anyhow::Result;
use lightwallet_txview::ParsedTx;
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Phase, Update};

/// One line of the feed. `type` discriminates; a consumer switches on it. The
/// `tx` variant flattens the shared [`ParsedTx`] projection, so lwtui's feed and
/// lwcli's parse the same fields into the same JSON.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    /// A block boundary. The pending set that preceded it is now stale.
    Block { height: u64, time: Option<u32> },
    /// A mempool transaction.
    Tx(ParsedTx),
    /// A connection state change.
    Status {
        state: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl From<Update> for Event {
    fn from(update: Update) -> Self {
        match update {
            Update::Block { tip, mined_unix } => Event::Block {
                height: tip,
                time: mined_unix,
            },
            Update::Tx(row) => Event::Tx(row.tx),
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
        }
    }
}

/// Serialize each update as it arrives, flushing per line so a pipe sees the
/// feed live. Exits cleanly when the reader closes the pipe.
pub async fn run(mut rx: UnboundedReceiver<Update>) -> Result<()> {
    let mut out = std::io::stdout().lock();
    while let Some(update) = rx.recv().await {
        let line = serde_json::to_string(&Event::from(update))?;
        if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(e.into());
        }
    }
    Ok(())
}
