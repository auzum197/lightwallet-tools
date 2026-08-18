//! `lwtui`: a live terminal monitor for the Zcash lightwallet indexers. It holds
//! one connection open and renders a variant's mempool as a list that
//! repopulates at each block boundary. The interactive sibling to lwcli, built
//! on the same `lightwallet-core` calls.

mod app;
mod ndjson;
mod net;
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
    about = "Live mempool monitor for Zcash lightwallet indexers"
)]
struct Args {
    /// Indexer endpoint, e.g. `https://zec.rocks:443` (https verifies against
    /// webpki roots, http is plaintext)
    #[arg(short, long)]
    url: String,

    /// Protocol variant the endpoint serves
    #[arg(long, value_enum, default_value_t = VariantArg::Canonical)]
    variant: VariantArg,

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

#[derive(Clone, Copy, ValueEnum)]
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

    let (tx, rx) = mpsc::unbounded_channel();
    let network = tokio::spawn(net::run(args.url, args.variant.into(), tx));

    let outcome = match args.output {
        OutputMode::Tui => {
            let mut terminal = ratatui::init();
            let r = run(&mut terminal, rx).await;
            ratatui::restore();
            r.context("render loop")
        }
        OutputMode::Ndjson => ndjson::run(rx).await.context("ndjson output"),
    };
    network.abort();
    outcome
}

/// The frame loop. A ~30fps ticker drives redraws (so the shimmer animates and
/// ages tick over) and drains queued updates; key events are handled as they
/// arrive. Draining is skipped while paused, so the list holds still and the
/// channel buffers.
async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    mut rx: mpsc::UnboundedReceiver<app::Update>,
) -> Result<()> {
    let mut state = App::new();
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
                        if state.on_key(key) {
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
