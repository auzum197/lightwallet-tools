//! `lwtui`: a live terminal monitor and read-only block explorer for the Zcash
//! lightwallet indexers. It holds one connection open and tails the chain: a
//! mempool view that repopulates each block, and a block-explorer view with tx
//! drill-down and `/` search. The interactive sibling to lwcli, built on the
//! same `lightwallet-core` calls.

mod app;
mod health;
mod ndjson;
mod net;
mod osc52;
mod theme;
mod ui;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use app::App;
use net::Variant;

#[derive(Parser)]
#[command(
    name = "lwtui",
    version,
    about = "Live mempool monitor and block explorer for Zcash lightwallet indexers"
)]
struct Args {
    /// Indexer endpoint, e.g. `https://zec.rocks:443` (https verifies against
    /// webpki roots, http is plaintext)
    #[arg(short, long)]
    url: String,

    /// Protocol variant the endpoint serves
    #[arg(long, value_enum, default_value_t = VariantArg::Canonical)]
    variant: VariantArg,

    /// Target block spacing in seconds, the health heuristic's reference. The
    /// default suits canonical main/test; pass a featurenet's own spacing.
    #[arg(long)]
    target_spacing: Option<u64>,

    /// Output mode: the interactive terminal UI, or a machine-readable NDJSON
    /// feed on stdout (one typed event per line, with block-boundary markers)
    #[arg(long, value_enum, default_value_t = OutputMode::Tui)]
    output: OutputMode,
}

#[derive(Clone, Copy, ValueEnum)]
enum VariantArg {
    Canonical,
    Crosslink,
}

#[derive(Clone, Copy, ValueEnum, PartialEq)]
enum OutputMode {
    Tui,
    Ndjson,
}

impl From<VariantArg> for Variant {
    fn from(v: VariantArg) -> Self {
        match v {
            VariantArg::Canonical => Variant::Canonical,
            VariantArg::Crosslink => Variant::Crosslink,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let variant: Variant = args.variant.into();
    let target = args.target_spacing.unwrap_or(App::DEFAULT_TARGET);

    let (tx, rx) = mpsc::unbounded_channel();
    let tail = tokio::spawn(net::run(args.url.clone(), variant, tx.clone()));

    let outcome = match args.output {
        OutputMode::Tui => {
            let (req_tx, req_rx) = mpsc::unbounded_channel();
            let search = tokio::spawn(net::run_search(args.url, variant, req_rx, tx));
            let state = App::new(req_tx, target);
            let mut terminal = ratatui::init();
            let r = run(&mut terminal, rx, state).await;
            ratatui::restore();
            search.abort();
            r.context("render loop")
        }
        OutputMode::Ndjson => ndjson::run(rx).await.context("ndjson output"),
    };
    tail.abort();
    outcome
}

/// The frame loop. A ~30fps ticker drives redraws (so the shimmer animates and
/// ages tick over) and drains queued updates; key events are handled as they
/// arrive. Draining is skipped while paused, so the list holds still and the
/// channel buffers.
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    mut rx: mpsc::UnboundedReceiver<app::Update>,
    mut state: App,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(33));

    loop {
        terminal.draw(|f| ui::draw(f, &state))?;
        tokio::select! {
            _ = ticker.tick() => {
                if !state.paused {
                    state.drain(&mut rx);
                }
            }
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        let quit = state.on_key(key);
                        // A copy key stashes its payload; emit it out-of-band so
                        // the render stays a pure function of state.
                        if let Some(payload) = state.take_copy() {
                            let _ = osc52::copy(&payload);
                        }
                        if quit {
                            return Ok(());
                        }
                    }
                    Some(Err(e)) => return Err(e).context("terminal event stream"),
                    _ => {}
                }
            }
        }
    }
}
