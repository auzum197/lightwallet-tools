//! The machine-readable consumer: the same `Update` stream the TUI renders,
//! serialized one JSON object per line to stdout. A `block` event marks each
//! boundary, so a consumer can reset its pending set and dedup the mempool the
//! server re-sends on reconnect.
//!
//! A transparent input total arrives after its transaction, once the funding
//! outputs are followed, so a `tx` line carries a `seq` and a later
//! `value_resolved` line patches it by that `seq`. This mirrors the TUI, where
//! a row's fee turns from `pending` to exact in place.

use std::collections::HashMap;
use std::io::Write;

use anyhow::Result;
use lightwallet_txview::ParsedTx;
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Phase, Update};

/// One line of the feed. `type` discriminates; a consumer switches on it. The
/// `tx` variant flattens the shared [`ParsedTx`] projection, so lwtui's feed and
/// lwcli's parse the same fields into the same JSON, plus a `seq` a
/// `value_resolved` line refers back to.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    /// A block boundary. The pending set that preceded it is now stale.
    Block { height: u64, time: Option<u32> },
    /// A mempool transaction.
    Tx {
        seq: u64,
        #[serde(flatten)]
        tx: ParsedTx,
    },
    /// A transparent input total, followed after the `tx` line with this `seq`.
    /// Both amounts are null when a funding lookup failed, so the fee is
    /// unresolvable rather than wrong.
    ValueResolved {
        seq: u64,
        inputs_value_zat: Option<i64>,
        fee_zat: Option<i64>,
    },
    /// A connection state change.
    Status {
        state: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Serialize each update as it arrives, flushing per line so a pipe sees the
/// feed live. Exits cleanly when the reader closes the pipe.
pub async fn run(mut rx: UnboundedReceiver<Update>) -> Result<()> {
    let mut out = std::io::stdout().lock();
    // A pending tx's fee needs its input-independent term to finish once the
    // inputs resolve; hold it by seq until the patch lands or the block clears.
    let mut fee_base: HashMap<u64, i64> = HashMap::new();
    while let Some(update) = rx.recv().await {
        let event = match update {
            Update::Block { tip, mined_unix } => {
                fee_base.clear();
                Event::Block {
                    height: tip,
                    time: mined_unix,
                }
            }
            Update::Tx(row) => {
                if let Some(base) = row.tx.fee_base
                    && !row.tx.prevouts.is_empty()
                {
                    fee_base.insert(row.seq, base);
                }
                Event::Tx {
                    seq: row.seq,
                    tx: row.tx,
                }
            }
            Update::ValueResolved { seq, total } => {
                let fee_zat = total.zip(fee_base.remove(&seq)).map(|(t, base)| base + t);
                Event::ValueResolved {
                    seq,
                    inputs_value_zat: total,
                    fee_zat,
                }
            }
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
