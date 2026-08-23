//! UI-owned state and the input/update handling. The app is the single owner of
//! the mempool and block lists; the network task feeds it `Update`s over one
//! channel and receives search `Request`s over another. Render is a pure
//! function of this state.

use std::collections::VecDeque;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lightwallet_txview::ParsedTx;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::dither::Charset;
use crate::health::{self, DEFAULT_TARGET_SPACING, Health};
use crate::net::Request;

/// A mempool transaction as the monitor tracks it: the shared projection, the
/// raw bytes (for the tx detail's raw view), and the two view-local facts
/// txview doesn't carry. `seq` is assigned by the producer and is what both a
/// selection and a later value patch bind to, so incoming rows never move the
/// cursor off the tx being read and a resolved input total lands on the right
/// row.
pub struct Row {
    pub seq: u64,
    pub first_seen: Instant,
    pub tx: ParsedTx,
    pub raw: Vec<u8>,
}

/// One block in the explorer tail. `inputs`/`outputs` sum every pool's spends
/// and creates over the block's compact txs; an orchard/ironwood action counts
/// on both sides (it carries a nullifier and a note commitment).
#[derive(Clone)]
pub struct BlockRow {
    /// Assigned on insertion, unique and stable: what a selection binds to, so a
    /// reorg's kept-and-dimmed row never collides with the fresh row at the same
    /// height. The network side leaves it 0; `insert_block` fills it.
    pub seq: u64,
    pub height: u64,
    pub time: u32,
    pub hash: Vec<u8>,
    pub prev_hash: Vec<u8>,
    pub txs: usize,
    pub inputs: usize,
    pub outputs: usize,
    /// The block's transactions, for the block detail.
    pub tx_rows: Vec<BlockTx>,
    /// Orphaned by a reorg: kept and dimmed rather than dropped.
    pub reorged: bool,
}

/// One transaction inside a block: its txid (display order) and its own input
/// and output counts, summed across pools as on [`BlockRow`].
#[derive(Clone)]
pub struct BlockTx {
    pub txid: String,
    pub inputs: usize,
    pub outputs: usize,
}

/// The node's view of chain height: where its own tip stands and where it
/// believes the chain's tip is. Equal when caught up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChainHeights {
    pub block_height: u64,
    pub estimated_height: u64,
}

/// A transaction opened in the tx detail: the parsed view and the bytes behind
/// the raw toggle.
#[derive(Clone)]
pub struct TxDetail {
    pub parsed: ParsedTx,
    pub raw: Vec<u8>,
}

/// A borrowed view of one transaction: its parsed form and the bytes behind it.
pub struct TxRef<'a> {
    pub parsed: &'a ParsedTx,
    pub raw: &'a [u8],
}

/// How far the open tx detail's transparent inputs have resolved: funders read
/// so far out of the total.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResolveProgress {
    pub done: usize,
    pub of: usize,
}

/// What following a tx's transparent inputs produced. `total` is `Some` only
/// when every funder read. `values` is the per-input value in vin order, `None`
/// where that funder could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedInputs {
    pub total: Option<i64>,
    pub values: Vec<Option<i64>>,
}

/// One transaction in a t-address search result set.
#[derive(Clone)]
pub struct TaddrHit {
    pub height: u64,
    pub detail: TxDetail,
}

/// Which list the body shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Mempool,
    Blocks,
    /// The transient t-address search results list.
    Results,
}

/// Where keys route. `Help` remembers where it was opened from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    List,
    /// The block-detail pane: block fields plus a selectable tx list.
    Block,
    Tx,
    Search,
    Help,
}

/// What opened the tx detail, which decides where Left/Esc returns and what
/// `n`/`N` step through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxOrigin {
    /// A block's tx list: back returns there, and the tx detail is a third column.
    Block,
    /// A t-address result set: back returns to it, `n`/`N` walk the set.
    Results,
    /// A standalone tx (mempool row or a bare txid search): back returns to the
    /// list, `n`/`N` do nothing.
    Standalone,
}

/// What the connection is doing right now, for the header status line.
#[derive(Clone)]
pub enum Phase {
    Connecting,
    Live,
    Reconnecting(String),
}

/// A message from the network task.
pub enum Update {
    /// A new mempool transaction. `seq` is already assigned by the producer.
    /// Boxed: it dwarfs the other variants, and they share this channel at the
    /// same rate under a refill.
    Tx(Box<Row>),
    /// A transparent input total, followed to its funding outputs after the row
    /// arrived. `total` is `Some` when every prevout resolved, `None` when one
    /// could not be, which shows as unresolvable rather than a wrong fee. Lands
    /// on the row with this `seq`, or is dropped if that row is already gone.
    ValueResolved {
        seq: u64,
        total: Option<i64>,
    },
    /// A block boundary: clear the pending set and adopt the new tip. Carries
    /// the tip block's miner timestamp when the server gave one.
    Block {
        tip: u64,
        mined_unix: Option<u32>,
    },
    Phase(Phase),
    /// A block for the explorer tail, oldest-first within a fill.
    MinedBlock(BlockRow),
    /// A `GetLightdInfo` sample, feeding the syncing health signal.
    Info(ChainHeights),
    /// A reorg replaced the chain from `at_height`; rows at or above it orphan.
    Reorg {
        at_height: u64,
    },
    /// A found transaction, to open in the tx detail, with the block height it
    /// was mined at (0 for a mempool tx). A standalone txid search uses the
    /// height to pull in that block for context.
    SearchTx {
        detail: TxDetail,
        height: u64,
    },
    /// Progress while the open tx detail's transparent inputs resolve, or
    /// `None` once that step ends. Guarded by `txid` so a late report for a
    /// closed or replaced tx detail is dropped.
    ResolveProgress {
        txid: String,
        progress: Option<ResolveProgress>,
    },
    /// The resolved transparent input total for the open tx detail, backing out its
    /// fee. `Some` when every funder read, `None` when one could not (the fee
    /// shows unresolvable, never wrong). `values` is the per-input value in vin
    /// order, each `None` where that funder could not be read. Guarded by `txid`.
    DetailResolved {
        txid: String,
        total: Option<i64>,
        values: Vec<Option<i64>>,
    },
    /// A found block, to insert into the tail (if within the window) and select.
    SearchBlock(BlockRow),
    /// A t-address result set.
    SearchTaddr {
        addr: String,
        window: u64,
        hits: Vec<TaddrHit>,
    },
    /// A search that didn't route: a footer message, no view change.
    SearchError(String),
}

/// The t-address search window: blocks back from tip, snapshotted at submit.
pub const SEARCH_WINDOW: u64 = 10_000;

const MAX_ROWS: usize = 5_000;
const MAX_BLOCKS: usize = 256;
const MAX_BLOCK_TIMES: usize = 32;

/// The whole monitor state. Rendered as a pure function of this plus the clock.
pub struct App {
    pub view: View,
    pub focus: Focus,
    pub rows: VecDeque<Row>,
    pub blocks: VecDeque<BlockRow>,
    pub phase: Phase,
    pub tip: u64,
    pub tip_mined_unix: Option<u32>,
    pub freed_at: Option<Instant>,
    pub paused: bool,

    /// `(block_height, estimated_height)` from the last `GetLightdInfo`.
    pub info: Option<ChainHeights>,
    /// Recent block times, newest last, for the cadence median.
    pub block_times: VecDeque<u32>,
    pub target_spacing: u64,

    /// The search bar text and cursor (byte index).
    pub search_input: String,
    pub search_cursor: usize,
    pub search_history: Vec<String>,
    history_idx: Option<usize>,
    /// A transient footer message, cleared on the next relevant action.
    pub footer_msg: Option<String>,

    pub tx_detail: Option<TxDetail>,
    pub raw_mode: bool,
    pub detail_scroll: u16,
    pub detail_origin: TxOrigin,
    /// How far the open tx detail's transparent inputs have resolved. `None`
    /// when nothing is resolving.
    pub resolve_progress: Option<ResolveProgress>,
    /// Per-input resolved value (zats) in vin order, once the tx detail's funders are
    /// read. `None` at an index whose funder could not be read. Empty until then.
    pub detail_input_values: Vec<Option<i64>>,
    /// Where a pending `Request::Transaction` will land when it returns.
    pending_origin: TxOrigin,
    /// A tx fetch the user still wants. A `Request::Transaction` is async, so a
    /// response can arrive after the user has closed the tx detail or walked past
    /// the tx that asked for it. Gating `SearchTx` on this drops stale
    /// responses instead of reopening the tx detail under them.
    pub(crate) awaiting_tx: bool,
    /// Height of a block being fetched to become the context for a standalone
    /// tx search, so its arrival attaches to the open tx detail instead of taking
    /// over the view.
    pending_ctx_block: Option<u64>,

    /// The block opened in the block-detail pane, and the selected tx within it.
    pub block_detail: Option<BlockRow>,
    pub block_tx_sel: usize,

    pub taddr_hits: Vec<TaddrHit>,
    pub taddr_idx: usize,
    pub taddr_label: String,

    /// A clipboard payload the render loop should emit via OSC 52, then clear.
    pending_copy: Option<String>,

    return_focus: Focus,
    /// Selected mempool row, by `seq`.
    selected_row: Option<u64>,
    /// Selected block, by `seq` (heights aren't unique across a reorg).
    selected_block_seq: Option<u64>,
    /// Selected result index.
    selected_hit: usize,
    next_block_seq: u64,
    start: Instant,
    req: UnboundedSender<Request>,

    /// The dither gradient's charset when it's on (a flag was passed and the
    /// terminal can render it), `None` when off.
    pub dither: Option<Charset>,
    /// Accumulated field time, advancing only while the gradient is active, so
    /// pausing freezes the animation on its last frame.
    dither_secs: f32,
    /// Wall clock of the last tick, to measure the field's per-frame step.
    last_tick: Instant,
}

impl App {
    pub fn new(
        req: UnboundedSender<Request>,
        target_spacing: u64,
        dither: Option<Charset>,
    ) -> Self {
        let now = Instant::now();
        Self {
            view: View::Blocks,
            focus: Focus::List,
            rows: VecDeque::new(),
            blocks: VecDeque::new(),
            phase: Phase::Connecting,
            tip: 0,
            tip_mined_unix: None,
            freed_at: None,
            paused: false,
            info: None,
            block_times: VecDeque::new(),
            target_spacing,
            search_input: String::new(),
            search_cursor: 0,
            search_history: Vec::new(),
            history_idx: None,
            footer_msg: None,
            tx_detail: None,
            raw_mode: false,
            detail_scroll: 0,
            detail_origin: TxOrigin::Standalone,
            resolve_progress: None,
            detail_input_values: Vec::new(),
            pending_origin: TxOrigin::Standalone,
            awaiting_tx: false,
            pending_ctx_block: None,
            block_detail: None,
            block_tx_sel: 0,
            taddr_hits: Vec::new(),
            taddr_idx: 0,
            taddr_label: String::new(),
            pending_copy: None,
            return_focus: Focus::List,
            selected_row: None,
            selected_block_seq: None,
            selected_hit: 0,
            next_block_seq: 0,
            start: now,
            req,
            dither,
            dither_secs: 0.0,
            last_tick: now,
        }
    }

    /// Seconds since start, the shimmer's animation clock.
    pub fn elapsed(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Advance the field clock by the elapsed frame, but only while the gradient
    /// is active. Paused or focused on search/help, the clock holds and the
    /// surface freezes on its last frame.
    pub fn tick_dither(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        if self.dither_active() {
            self.dither_secs += dt;
        }
    }

    fn dither_active(&self) -> bool {
        // The gradient runs continuously while visible: only a manual pause or
        // search/help (which take over the footer) freeze it. It never idles
        // out, so it keeps drifting whether or not the user is typing.
        self.dither.is_some() && !self.paused && !matches!(self.focus, Focus::Search | Focus::Help)
    }

    /// The field time for the current frame, `t` advancing 0.5 units a second of
    /// active animation, quantized to ~11fps so repaints land at most every 90ms
    /// and the frozen frames between diff to nothing.
    pub fn dither_time(&self) -> f32 {
        const STEP: f32 = 0.09;
        (self.dither_secs / STEP).floor() * STEP * 0.5
    }

    /// Current chain health, evaluated fresh each call.
    pub fn health(&self, now: u64) -> Health {
        let ChainHeights {
            block_height,
            estimated_height,
        } = self.info.unwrap_or(ChainHeights {
            block_height: self.tip,
            estimated_height: 0,
        });
        health::evaluate(
            &health::Inputs {
                last_block_time: self.block_times.back().copied().or(self.tip_mined_unix),
                block_height,
                estimated_height,
                block_times: &self.block_times,
                target: self.target_spacing,
            },
            now,
        )
    }

    /// Drain everything the network task has queued. Called while running (not
    /// paused), so a paused list holds still and the channel buffers.
    pub fn drain(&mut self, rx: &mut UnboundedReceiver<Update>) {
        while let Ok(update) = rx.try_recv() {
            self.apply(update);
        }
    }

    pub(crate) fn apply(&mut self, update: Update) {
        match update {
            Update::Tx(row) => {
                self.rows.push_front(*row);
                self.rows.truncate(MAX_ROWS);
            }
            Update::ValueResolved { seq, total } => {
                // A late patch for a row cleared at a block boundary has no
                // target; dropping it is correct, the tx is gone.
                if let Some(row) = self.rows.iter_mut().find(|r| r.seq == seq) {
                    row.tx.resolve(total);
                }
            }
            Update::Block { tip, mined_unix } => {
                if self.tip != 0 && tip > self.tip {
                    self.freed_at = Some(Instant::now());
                }
                self.rows.clear();
                self.selected_row = None;
                self.tip = tip;
                self.tip_mined_unix = mined_unix;
            }
            Update::Phase(phase) => self.phase = phase,
            Update::MinedBlock(block) => self.insert_block(block),
            Update::Info(heights) => self.info = Some(heights),
            Update::Reorg { at_height } => {
                for b in self.blocks.iter_mut().filter(|b| b.height >= at_height) {
                    b.reorged = true;
                }
                self.footer_msg = Some(format!("reorg: height {at_height} replaced"));
            }
            Update::SearchTx { detail, height } => {
                if self.awaiting_tx {
                    self.awaiting_tx = false;
                    let origin = self.pending_origin;
                    self.open_detail(detail, origin);
                    // A standalone txid search also pulls in its mining block, so
                    // the block pane shows alongside the tx. A mempool hit
                    // (height 0) has no block.
                    if origin == TxOrigin::Standalone && height > 0 {
                        self.pending_ctx_block = Some(height);
                        let _ = self.req.send(Request::Block(height));
                    }
                }
            }
            Update::ResolveProgress { txid, progress } => {
                if self.detail_txid() == Some(txid.as_str()) {
                    self.resolve_progress = progress;
                }
            }
            Update::DetailResolved {
                txid,
                total,
                values,
            } => {
                if let Some(d) = &mut self.tx_detail
                    && d.parsed.txid.as_deref() == Some(txid.as_str())
                {
                    d.parsed.resolve(total);
                    self.detail_input_values = values;
                    self.resolve_progress = None;
                }
            }
            Update::SearchBlock(block) => {
                if self.pending_ctx_block == Some(block.height) {
                    self.pending_ctx_block = None;
                    self.attach_ctx_block(block);
                } else {
                    let height = block.height;
                    self.block_detail = Some(block.clone());
                    self.block_tx_sel = 0;
                    self.insert_block(block);
                    self.view = View::Blocks;
                    self.focus = Focus::Block;
                    self.selected_block_seq = self
                        .blocks
                        .iter()
                        .find(|b| b.height == height && !b.reorged)
                        .map(|b| b.seq);
                    self.footer_msg = None;
                }
            }
            Update::SearchTaddr { addr, window, hits } => {
                self.taddr_hits = hits;
                self.taddr_idx = 0;
                self.selected_hit = 0;
                self.taddr_label = format!("{addr} · last {window} blocks");
                self.view = View::Results;
                self.focus = Focus::List;
                self.footer_msg = None;
            }
            Update::SearchError(msg) => {
                // A failed context-block fetch just leaves the tx standalone.
                self.pending_ctx_block = None;
                self.footer_msg = Some(msg);
            }
        }
    }

    /// Attach a fetched block as the open tx detail's context: the block detail and
    /// tx list show beside the tx, and the tx detail becomes block-origin so back
    /// steps to the block's tx list. The tx detail itself is left as it is.
    fn attach_ctx_block(&mut self, block: BlockRow) {
        // Highlight the open tx within the block's tx list.
        let sel = self
            .detail_txid()
            .and_then(|id| block.tx_rows.iter().position(|t| t.txid == id));
        let height = block.height;
        self.block_tx_sel = sel.unwrap_or(0);
        self.block_detail = Some(block.clone());
        self.detail_origin = TxOrigin::Block;
        self.insert_block(block);
        self.view = View::Blocks;
        self.selected_block_seq = self
            .blocks
            .iter()
            .find(|b| b.height == height && !b.reorged)
            .map(|b| b.seq);
        self.footer_msg = None;
    }

    /// Insert a tail block newest-front, replacing a live row at the same height
    /// (a reorg re-emits) and pruning the window.
    fn insert_block(&mut self, mut block: BlockRow) {
        block.seq = self.next_block_seq;
        self.next_block_seq += 1;
        // Feed the cadence clock only when time advances, so a backfilled or
        // reorged older block doesn't inject a negative interval.
        if self.block_times.back().is_none_or(|t| block.time >= *t) {
            self.push_block_time(block.time);
        }
        // Keep newest at the front. Fills arrive oldest-first, so a new block is
        // usually higher than the current front.
        let pos = self
            .blocks
            .iter()
            .position(|b| b.height <= block.height)
            .unwrap_or(self.blocks.len());
        // Drop an existing live row at this exact height (idempotent re-fill).
        if let Some(existing) = self
            .blocks
            .iter()
            .position(|b| b.height == block.height && !b.reorged)
        {
            self.blocks.remove(existing);
        }
        let pos = pos.min(self.blocks.len());
        self.blocks.insert(pos, block);
        self.blocks.truncate(MAX_BLOCKS);
    }

    fn push_block_time(&mut self, time: u32) {
        self.block_times.push_back(time);
        while self.block_times.len() > MAX_BLOCK_TIMES {
            self.block_times.pop_front();
        }
    }

    /// The txid of the tx open in the tx detail, if any and if it parsed.
    fn detail_txid(&self) -> Option<&str> {
        self.tx_detail
            .as_ref()
            .and_then(|d| d.parsed.txid.as_deref())
    }

    fn open_detail(&mut self, detail: TxDetail, origin: TxOrigin) {
        self.tx_detail = Some(detail);
        self.detail_origin = origin;
        self.raw_mode = false;
        self.detail_scroll = 0;
        self.resolve_progress = None;
        self.detail_input_values = Vec::new();
        // A block-opened tx keeps its block-detail column and its tx list to
        // return to; every other origin opens over the plain list.
        if origin != TxOrigin::Block {
            self.block_detail = None;
        }
        self.focus = Focus::Tx;
        self.footer_msg = None;
    }

    /// Drop the open tx and tell the search task to stop resolving it.
    fn close_detail(&mut self) {
        self.tx_detail = None;
        self.resolve_progress = None;
        self.detail_input_values = Vec::new();
        let _ = self.req.send(Request::CloseTx);
    }

    /// A clipboard payload queued by a copy key, taken by the render loop to
    /// emit via OSC 52.
    pub fn take_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    /// The transaction currently in focus: the open tx detail, else the selected
    /// mempool or results row. Block rows carry no full tx.
    fn current_tx(&self) -> Option<TxRef<'_>> {
        if let Some(d) = &self.tx_detail {
            return Some(TxRef {
                parsed: &d.parsed,
                raw: &d.raw,
            });
        }
        match self.view {
            View::Mempool => {
                let row = self.selected_index().and_then(|i| self.rows.get(i))?;
                Some(TxRef {
                    parsed: &row.tx,
                    raw: &row.raw,
                })
            }
            View::Results => {
                let hit = self.taddr_hits.get(self.selected_hit)?;
                Some(TxRef {
                    parsed: &hit.detail.parsed,
                    raw: &hit.detail.raw,
                })
            }
            View::Blocks => None,
        }
    }

    fn copy_json(&mut self) {
        match self
            .current_tx()
            .and_then(|t| serde_json::to_string_pretty(t.parsed).ok())
        {
            Some(json) => {
                self.pending_copy = Some(json);
                self.footer_msg = Some("copied tx JSON to clipboard".to_string());
            }
            None => self.footer_msg = Some("no transaction to copy".to_string()),
        }
    }

    fn copy_raw(&mut self) {
        match self.current_tx().map(|t| hex::encode(t.raw)) {
            Some(hex) => {
                self.pending_copy = Some(hex);
                self.footer_msg = Some("copied raw tx hex to clipboard".to_string());
            }
            None => self.footer_msg = Some("no transaction to copy".to_string()),
        }
    }

    fn copy_block_txid(&mut self) {
        if let Some(txid) = self
            .block_detail
            .as_ref()
            .and_then(|b| b.tx_rows.get(self.block_tx_sel))
            .map(|t| t.txid.clone())
        {
            self.pending_copy = Some(txid);
            self.footer_msg = Some("copied txid to clipboard".to_string());
        }
    }

    /// The selected index in the active list, if any.
    pub fn selected_index(&self) -> Option<usize> {
        match self.view {
            View::Mempool => self
                .selected_row
                .and_then(|seq| self.rows.iter().position(|r| r.seq == seq)),
            View::Blocks => self
                .selected_block_seq
                .and_then(|s| self.blocks.iter().position(|b| b.seq == s)),
            View::Results => (!self.taddr_hits.is_empty()).then_some(self.selected_hit),
        }
    }

    /// Handle a keypress. Returns true to quit.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        match self.focus {
            Focus::Search => self.search_key(key),
            Focus::Help => self.focus = self.return_focus,
            Focus::List | Focus::Block | Focus::Tx => return self.list_or_detail_key(key),
        }
        false
    }

    fn list_or_detail_key(&mut self, key: KeyEvent) -> bool {
        // The global set runs before the focus-local keys.
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('?') => {
                self.return_focus = self.focus;
                self.focus = Focus::Help;
                return false;
            }
            KeyCode::Char(' ') => {
                self.paused = !self.paused;
                return false;
            }
            _ => {}
        }
        match self.focus {
            Focus::Tx => self.detail_key(key),
            Focus::Block => self.block_key(key),
            _ => self.list_key(key),
        }
        false
    }

    fn block_key(&mut self, key: KeyEvent) {
        let count = self
            .block_detail
            .as_ref()
            .map(|b| b.tx_rows.len())
            .unwrap_or(0);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(i) = self.step(Some(self.block_tx_sel), count, 1) {
                    self.block_tx_sel = i;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(i) = self.step(Some(self.block_tx_sel), count, -1) {
                    self.block_tx_sel = i;
                }
            }
            // n/N walk the list and open each into the (unfocused) detail pane,
            // a quick browse; Right/Enter focuses the pane to scroll it.
            KeyCode::Char('n') => self.walk_block_tx(1),
            KeyCode::Char('N') => self.walk_block_tx(-1),
            KeyCode::Enter | KeyCode::Right => self.open_block_tx(),
            // No full tx here, so `y` copies the txid; open the tx detail for JSON.
            KeyCode::Char('y') => self.copy_block_txid(),
            KeyCode::Esc | KeyCode::Left => {
                // Leaving the block context drops the tx kept in the pane, so it
                // never leaks onto the plain list.
                self.block_detail = None;
                self.close_detail();
                self.awaiting_tx = false;
                self.focus = Focus::List;
            }
            _ => {}
        }
    }

    /// Fetch the selected block-tx's full bytes into the tx detail; the result
    /// lands as a block-origin tx detail (third column, back to the tx list).
    fn open_block_tx(&mut self) {
        let Some(txid) = self
            .block_detail
            .as_ref()
            .and_then(|b| b.tx_rows.get(self.block_tx_sel))
            .map(|t| t.txid.clone())
        else {
            return;
        };
        // The selected tx is already the open tx detail (the user stepped back to the
        // list without closing it): just re-focus the pane, no refetch.
        if self.detail_txid() == Some(txid.as_str()) {
            self.focus = Focus::Tx;
            return;
        }
        self.pending_origin = TxOrigin::Block;
        self.awaiting_tx = true;
        let _ = self.req.send(Request::Transaction(txid));
        self.footer_msg = Some("searching tx…".to_string());
    }

    fn list_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => self.cycle_view(),
            KeyCode::Char('/') => {
                self.focus = Focus::Search;
                self.search_input.clear();
                self.search_cursor = 0;
                self.history_idx = None;
                self.footer_msg = None;
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('y') => self.copy_json(),
            KeyCode::Char('Y') => self.copy_raw(),
            KeyCode::Enter | KeyCode::Right => self.open_selected(),
            KeyCode::Esc => {
                // Leave the transient results view back to the tail.
                if self.view == View::Results {
                    self.view = View::Blocks;
                }
            }
            _ => {}
        }
    }

    fn detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('r') => self.raw_mode = !self.raw_mode,
            // Arrows (and j/k) scroll the pane, so a tall tx with many inputs and
            // outputs reads in full; n/N walk between txs (block list or results).
            KeyCode::Char('j') | KeyCode::Down => {
                self.detail_scroll = self.detail_scroll.saturating_add(1)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1)
            }
            KeyCode::Char('n') => self.detail_step(1),
            KeyCode::Char('N') => self.detail_step(-1),
            KeyCode::Char('y') => self.copy_json(),
            KeyCode::Char('Y') => self.copy_raw(),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Left => {
                self.awaiting_tx = false;
                // A block-opened tx pops back to its block's tx list but stays
                // shown in the tx pane, dimmed like any unfocused column; every
                // other origin closes the tx detail back to the plain list.
                if self.detail_origin == TxOrigin::Block && self.block_detail.is_some() {
                    self.focus = Focus::Block;
                } else {
                    self.close_detail();
                    self.focus = Focus::List;
                }
            }
            _ => {}
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.focus = Focus::List;
                self.search_input.clear();
                self.search_cursor = 0;
            }
            KeyCode::Enter => self.submit_search(),
            KeyCode::Up => self.history_step(1),
            KeyCode::Down => self.history_step(-1),
            KeyCode::Left => self.search_cursor = self.search_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.search_cursor = (self.search_cursor + 1).min(self.search_input.len())
            }
            KeyCode::Backspace => {
                if self.search_cursor > 0 {
                    self.search_cursor -= 1;
                    self.search_input.remove(self.search_cursor);
                }
            }
            KeyCode::Char('a') if ctrl => self.search_cursor = 0,
            KeyCode::Char('e') if ctrl => self.search_cursor = self.search_input.len(),
            KeyCode::Char('u') if ctrl => {
                self.search_input.clear();
                self.search_cursor = 0;
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Char(c) if !ctrl => {
                self.search_input.insert(self.search_cursor, c);
                self.search_cursor += c.len_utf8();
            }
            _ => {}
        }
    }

    fn delete_word(&mut self) {
        let head = &self.search_input[..self.search_cursor];
        let trimmed = head.trim_end_matches(' ');
        let cut = trimmed.rfind(' ').map(|i| i + 1).unwrap_or(0);
        self.search_input.replace_range(cut..self.search_cursor, "");
        self.search_cursor = cut;
    }

    fn history_step(&mut self, dir: i32) {
        if self.search_history.is_empty() {
            return;
        }
        let last = self.search_history.len() - 1;
        let next = match (self.history_idx, dir) {
            (None, 1) => Some(last),
            (Some(0), 1) => Some(0),
            (Some(i), 1) => Some(i - 1),
            (Some(i), -1) if i >= last => None,
            (Some(i), -1) => Some(i + 1),
            (None, _) => None,
            _ => self.history_idx,
        };
        self.history_idx = next;
        self.search_input = next
            .map(|i| self.search_history[i].clone())
            .unwrap_or_default();
        self.search_cursor = self.search_input.len();
    }

    fn submit_search(&mut self) {
        let q = self.search_input.trim().to_string();
        self.search_input.clear();
        self.search_cursor = 0;
        self.history_idx = None;
        self.focus = Focus::List;
        if q.is_empty() {
            return;
        }
        self.search_history.push(q.clone());
        self.route_query(&q);
    }

    fn route_query(&mut self, q: &str) {
        if q.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(h) = q.parse::<u64>() {
                let _ = self.req.send(Request::Block(h));
                self.footer_msg = Some(format!("searching block {h}…"));
            }
        } else if let Some(hex) = as_txid_hex(q) {
            self.pending_origin = TxOrigin::Standalone;
            self.awaiting_tx = true;
            let _ = self.req.send(Request::Transaction(hex));
            self.footer_msg = Some("searching tx…".to_string());
        } else if is_transparent_addr(q) {
            let start = self.tip.saturating_sub(SEARCH_WINDOW);
            let _ = self.req.send(Request::Taddr {
                addr: q.to_string(),
                start,
                end: self.tip,
            });
            self.footer_msg = Some(format!("t-addr · last {SEARCH_WINDOW} blocks"));
        } else if is_shielded_addr(q) {
            self.footer_msg = Some(
                "shielded address — explorer is transparent-only (no viewing key)".to_string(),
            );
        } else {
            self.footer_msg = Some("unrecognized query".to_string());
        }
    }

    fn cycle_view(&mut self) {
        self.view = match self.view {
            View::Mempool => View::Blocks,
            _ => View::Mempool,
        };
    }

    fn open_selected(&mut self) {
        match self.view {
            View::Mempool => {
                if self.selected_row.is_none() {
                    self.selected_row = self.rows.front().map(|r| r.seq);
                }
                if let Some(row) = self.selected_index().and_then(|i| self.rows.get(i)) {
                    let detail = TxDetail {
                        parsed: row.tx.clone(),
                        raw: row.raw.clone(),
                    };
                    self.open_detail(detail, TxOrigin::Standalone);
                }
            }
            View::Results => {
                if let Some(hit) = self.taddr_hits.get(self.selected_hit).cloned() {
                    self.taddr_idx = self.selected_hit;
                    self.open_detail(hit.detail, TxOrigin::Results);
                }
            }
            View::Blocks => {
                if let Some(b) = self
                    .selected_index()
                    .and_then(|i| self.blocks.get(i))
                    .cloned()
                {
                    self.block_detail = Some(b);
                    self.block_tx_sel = 0;
                    self.focus = Focus::Block;
                    self.footer_msg = None;
                }
            }
        }
    }

    /// `n`/`N` inside the tx detail: walk whatever opened it.
    fn detail_step(&mut self, dir: i32) {
        match self.detail_origin {
            TxOrigin::Results => self.walk_hits(dir),
            TxOrigin::Block => self.walk_block_tx(dir),
            TxOrigin::Standalone => {}
        }
    }

    /// Step to the block's next/prev tx and open it in place. A step that lands
    /// on the current row (a single-tx block, or already at a boundary) fetches
    /// nothing: re-requesting the tx already shown is wasted work.
    fn walk_block_tx(&mut self, dir: i32) {
        let count = self
            .block_detail
            .as_ref()
            .map(|b| b.tx_rows.len())
            .unwrap_or(0);
        if let Some(next) = self.step(Some(self.block_tx_sel), count, dir as isize)
            && next != self.block_tx_sel
        {
            self.block_tx_sel = next;
            self.open_block_tx();
        }
    }

    fn walk_hits(&mut self, dir: i32) {
        if self.taddr_hits.is_empty() {
            return;
        }
        let last = self.taddr_hits.len() as i32 - 1;
        let next = (self.taddr_idx as i32 + dir).clamp(0, last) as usize;
        self.taddr_idx = next;
        self.selected_hit = next;
        if let Some(hit) = self.taddr_hits.get(next).cloned() {
            self.open_detail(hit.detail, TxOrigin::Results);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.view {
            View::Mempool => {
                let next = self.step(self.selected_index(), self.rows.len(), delta);
                self.selected_row = next.and_then(|i| self.rows.get(i)).map(|r| r.seq);
            }
            View::Blocks => {
                let next = self.step(self.selected_index(), self.blocks.len(), delta);
                self.selected_block_seq = next.and_then(|i| self.blocks.get(i)).map(|b| b.seq);
            }
            View::Results => {
                if let Some(i) = self.step(Some(self.selected_hit), self.taddr_hits.len(), delta) {
                    self.selected_hit = i;
                }
            }
        }
    }

    fn step(&self, current: Option<usize>, len: usize, delta: isize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let i = match current {
            None => 0,
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
        };
        Some(i)
    }
}

/// A 64-hex txid (with optional `0x`) → its `hash`-order hex. Zcash displays
/// txids byte-reversed, so the net side reverses this to internal order before
/// filling `TxFilter.hash`.
fn as_txid_hex(q: &str) -> Option<String> {
    let s = q.strip_prefix("0x").unwrap_or(q);
    (s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())).then(|| s.to_string())
}

/// A transparent-address prefix for any canonical network. Over-accepting is
/// harmless: a wrong-network address simply returns no rows.
fn is_transparent_addr(q: &str) -> bool {
    ["t1", "t3", "tm", "t2"].iter().any(|p| q.starts_with(p)) && q.len() > 20
}

/// A shielded or unified address, rejected: the indexer has no lookup for one.
fn is_shielded_addr(q: &str) -> bool {
    ["zs", "zo", "zt", "u1", "utest"]
        .iter()
        .any(|p| q.starts_with(p))
}

impl App {
    /// The default target spacing, exposed for `main` when no flag is given.
    pub const DEFAULT_TARGET: u64 = DEFAULT_TARGET_SPACING;
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightwallet_txview::{InputTotal, OutPoint, ParsedTx, Value};
    use tokio::sync::mpsc;

    /// An app under test with the receiving end of its request channel.
    struct TestApp {
        app: App,
        rx: mpsc::UnboundedReceiver<Request>,
    }

    fn app() -> TestApp {
        let (tx, rx) = mpsc::unbounded_channel();
        TestApp {
            app: App::new(tx, App::DEFAULT_TARGET, None),
            rx,
        }
    }

    /// A shielded row with no transparent inputs, addressed by `seq`.
    fn tx_row(seq: u64) -> Box<Row> {
        Box::new(mempool_row_seq(seq))
    }

    /// A row whose transparent input total is awaiting resolution.
    fn pending_row(seq: u64) -> Box<Row> {
        let mut row = mempool_row_seq(seq);
        row.tx.prevouts = vec![OutPoint {
            hash: [1u8; 32],
            index: 0,
        }];
        row.tx.input_total = InputTotal::Pending;
        Box::new(row)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(s: &str, app: &mut App) {
        for c in s.chars() {
            app.on_key(press(KeyCode::Char(c)));
        }
    }

    fn parsed() -> ParsedTx {
        ParsedTx {
            txid: Some("a".repeat(64)),
            version: Some("V6".into()),
            size: 100,
            consensus_branch_id: Some("nu6_3".into()),
            lock_time: Some(0),
            expiry_height: Some(0),
            inputs: Vec::new(),
            outputs: Vec::new(),
            staking: None,
            value: Value::Shielded,
            vout_values: Vec::new(),
            prevouts: Vec::new(),
            input_total: InputTotal::None,
            fee_base: Some(1_000),
            error: None,
        }
    }

    fn mempool_row_seq(seq: u64) -> Row {
        Row {
            seq,
            first_seen: Instant::now(),
            tx: parsed(),
            raw: vec![0u8; 8],
        }
    }

    fn mempool_row() -> Box<Row> {
        Box::new(mempool_row_seq(0))
    }

    fn block(height: u64, time: u32) -> BlockRow {
        BlockRow {
            seq: 0,
            height,
            time,
            hash: vec![height as u8; 32],
            prev_hash: vec![(height - 1) as u8; 32],
            txs: 1,
            inputs: 1,
            outputs: 2,
            tx_rows: vec![BlockTx {
                txid: "a".repeat(64),
                inputs: 1,
                outputs: 2,
            }],
            reorged: false,
        }
    }

    /// A block carrying two transactions, so a walk has somewhere to step to.
    fn block_two_txs(height: u64, time: u32) -> BlockRow {
        let mut b = block(height, time);
        b.txs = 2;
        b.tx_rows.push(BlockTx {
            txid: "b".repeat(64),
            inputs: 2,
            outputs: 3,
        });
        b
    }

    /// Select the first (index 0) block row in the Blocks view.
    fn select_first_block(app: &mut App) {
        app.on_key(press(KeyCode::Char('j')));
    }

    #[test]
    fn newest_row_is_front_carrying_its_producer_seq() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::Tx(tx_row(0)));
        app.apply(Update::Tx(tx_row(1)));
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.rows[0].seq, 1);
        assert_eq!(app.rows[1].seq, 0);
    }

    #[test]
    fn a_resolved_total_lands_on_its_row_and_a_stale_one_is_dropped() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::Tx(pending_row(7)));
        app.apply(Update::ValueResolved {
            seq: 7,
            total: Some(410_000),
        });
        assert!(matches!(
            app.rows[0].tx.input_total,
            InputTotal::Known(410_000)
        ));
        // A patch for a seq no longer present is a no-op, not a panic.
        app.apply(Update::ValueResolved {
            seq: 999,
            total: Some(1),
        });
    }

    #[test]
    fn detail_input_resolution_lands_by_txid_and_tracks_progress() {
        let TestApp { mut app, .. } = app();
        let mut parsed = parsed();
        parsed.txid = Some("a".repeat(64));
        parsed.input_total = InputTotal::Pending;
        parsed.prevouts = vec![OutPoint {
            hash: [1u8; 32],
            index: 0,
        }];
        app.tx_detail = Some(TxDetail {
            parsed,
            raw: Vec::new(),
        });

        // A progress report for the open tx says how many funders have read.
        app.apply(Update::ResolveProgress {
            txid: "a".repeat(64),
            progress: Some(ResolveProgress { done: 0, of: 1 }),
        });
        assert_eq!(
            app.resolve_progress,
            Some(ResolveProgress { done: 0, of: 1 })
        );
        // A report for a different tx is dropped, leaving the current one.
        app.apply(Update::ResolveProgress {
            txid: "b".repeat(64),
            progress: Some(ResolveProgress { done: 3, of: 4 }),
        });
        assert_eq!(
            app.resolve_progress,
            Some(ResolveProgress { done: 0, of: 1 })
        );

        // Resolution for the open tx lands and clears the progress.
        app.apply(Update::DetailResolved {
            txid: "a".repeat(64),
            total: Some(500_000),
            values: vec![Some(500_000)],
        });
        assert!(matches!(
            app.tx_detail.as_ref().unwrap().parsed.input_total,
            InputTotal::Known(500_000)
        ));
        assert_eq!(app.detail_input_values, vec![Some(500_000)]);
        assert!(app.resolve_progress.is_none());

        // A stale resolution for a tx no longer in the tx detail is a no-op.
        app.apply(Update::DetailResolved {
            txid: "z".repeat(64),
            total: Some(1),
            values: vec![Some(1)],
        });
        assert!(matches!(
            app.tx_detail.as_ref().unwrap().parsed.input_total,
            InputTotal::Known(500_000)
        ));
    }

    #[test]
    fn mined_blocks_keep_the_newest_at_the_front() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        app.apply(Update::MinedBlock(block(101, 1_075)));
        assert_eq!(app.blocks[0].height, 101);
        assert_eq!(app.blocks[1].height, 100);
    }

    #[test]
    fn selection_navigates_past_reorged_duplicate_heights() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        app.apply(Update::MinedBlock(block(101, 1_075)));
        app.apply(Update::Reorg { at_height: 101 }); // orphans the old 101
        app.apply(Update::MinedBlock(block(101, 1_080))); // a fresh 101 at the same height
        assert_eq!(
            app.blocks.len(),
            3,
            "the orphan is kept beside the fresh row"
        );
        // Walking down must reach every row, not stall on the duplicate height.
        select_first_block(&mut app);
        assert_eq!(app.selected_index(), Some(0));
        app.on_key(press(KeyCode::Char('j')));
        assert_eq!(app.selected_index(), Some(1));
        app.on_key(press(KeyCode::Char('j')));
        assert_eq!(app.selected_index(), Some(2));
    }

    #[test]
    fn a_reorg_marks_rows_at_or_above_the_height() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        app.apply(Update::MinedBlock(block(101, 1_075)));
        app.apply(Update::Reorg { at_height: 101 });
        assert!(!app.blocks.iter().find(|b| b.height == 100).unwrap().reorged);
        assert!(app.blocks.iter().find(|b| b.height == 101).unwrap().reorged);
    }

    #[test]
    fn tab_toggles_the_view() {
        let TestApp { mut app, .. } = app();
        assert_eq!(app.view, View::Blocks);
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.view, View::Mempool);
        app.on_key(press(KeyCode::Tab));
        assert_eq!(app.view, View::Blocks);
    }

    #[test]
    fn a_height_query_sends_a_block_request() {
        let TestApp { mut app, mut rx } = app();
        app.tip = 500;
        app.on_key(press(KeyCode::Char('/')));
        assert_eq!(app.focus, Focus::Search);
        typed("321", &mut app);
        app.on_key(press(KeyCode::Enter));
        assert!(matches!(rx.try_recv(), Ok(Request::Block(321))));
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn a_txid_query_sends_a_transaction_request() {
        let TestApp { mut app, mut rx } = app();
        app.on_key(press(KeyCode::Char('/')));
        typed(&"a".repeat(64), &mut app);
        app.on_key(press(KeyCode::Enter));
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
    }

    #[test]
    fn a_shielded_address_is_rejected_without_a_request() {
        let TestApp { mut app, mut rx } = app();
        app.on_key(press(KeyCode::Char('/')));
        typed("zs1qqqqqqqqqqqqqqqqqqqqqqqqq", &mut app);
        app.on_key(press(KeyCode::Enter));
        assert!(rx.try_recv().is_err());
        assert!(
            app.footer_msg
                .as_deref()
                .unwrap()
                .contains("transparent-only")
        );
    }

    #[test]
    fn search_captures_letters_as_literal_text() {
        let TestApp { mut app, .. } = app();
        app.on_key(press(KeyCode::Char('/')));
        // 'q' would quit at List focus, but Search captures it as input.
        let quit = app.on_key(press(KeyCode::Char('q')));
        assert!(!quit);
        assert_eq!(app.search_input, "q");
    }

    #[test]
    fn enter_opens_and_esc_closes_the_detail() {
        let TestApp { mut app, .. } = app();
        app.view = View::Mempool;
        app.apply(Update::Tx(mempool_row()));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Tx);
        assert!(app.tx_detail.is_some());
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.focus, Focus::List);
        assert!(app.tx_detail.is_none());
    }

    #[test]
    fn enter_on_a_block_opens_detail_then_a_tx_request() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // open block detail
        assert_eq!(app.focus, Focus::Block);
        assert!(app.block_detail.is_some());
        app.on_key(press(KeyCode::Enter)); // open the selected tx
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
    }

    fn detail() -> TxDetail {
        TxDetail {
            parsed: parsed(),
            raw: vec![0u8; 4],
        }
    }

    #[test]
    fn a_block_opened_tx_returns_to_its_tx_list_not_the_block_list() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // block detail (Focus::Block)
        app.on_key(press(KeyCode::Enter)); // request the selected tx
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        }); // the search task answers
        assert_eq!(app.focus, Focus::Tx);
        assert_eq!(app.detail_origin, TxOrigin::Block);
        assert!(
            app.block_detail.is_some(),
            "block context is kept for the 3rd column"
        );
        app.on_key(press(KeyCode::Esc));
        assert_eq!(
            app.focus,
            Focus::Block,
            "back to the tx list, not the block list"
        );
    }

    #[test]
    fn a_late_tx_response_after_close_does_not_reopen_the_detail() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // block detail
        app.on_key(press(KeyCode::Enter)); // request the selected tx
        let _ = rx.try_recv();
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        }); // first response opens the tx detail
        assert_eq!(app.focus, Focus::Tx);
        app.on_key(press(KeyCode::Esc)); // back to the tx list
        assert_eq!(app.focus, Focus::Block);
        // The tx stays shown in the (now dimmed) pane on the way back.
        assert!(app.tx_detail.is_some(), "the tx keeps showing in the pane");
        // A stale response from an earlier walk must not jump focus into the
        // tx detail.
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        });
        assert_eq!(app.focus, Focus::Block);
    }

    #[test]
    fn a_tx_search_pulls_in_its_block_for_context() {
        let TestApp { mut app, mut rx } = app();
        app.tip = 600;
        app.on_key(press(KeyCode::Char('/')));
        typed(&"a".repeat(64), &mut app);
        app.on_key(press(KeyCode::Enter));
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
        // The tx resolves with its mining height and opens the tx detail.
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 500,
        });
        assert_eq!(app.focus, Focus::Tx);
        // Which triggers a fetch of that block for the side panes.
        assert!(matches!(rx.try_recv(), Ok(Request::Block(500))));
        app.apply(Update::SearchBlock(block(500, 1_000)));
        assert!(app.block_detail.is_some(), "the block shows as context");
        assert_eq!(app.detail_origin, TxOrigin::Block);
        assert_eq!(app.view, View::Blocks);
        assert_eq!(app.focus, Focus::Tx, "the tx stays open");
    }

    #[test]
    fn a_block_search_older_than_the_live_window_still_opens() {
        let TestApp { mut app, .. } = app();
        for h in 1_000..1_000 + MAX_BLOCKS as u64 {
            app.apply(Update::MinedBlock(block(h, h as u32)));
        }
        app.apply(Update::SearchBlock(block(500, 1_000)));
        assert_eq!(app.block_detail.as_ref().map(|b| b.height), Some(500));
        assert_eq!(app.focus, Focus::Block);
        assert_eq!(app.view, View::Blocks);
        assert!(app.footer_msg.is_none());
    }

    #[test]
    fn closing_the_detail_tells_the_search_task_to_stop_resolving() {
        let TestApp { mut app, mut rx } = app();
        app.awaiting_tx = true;
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        });
        assert_eq!(app.focus, Focus::Tx);
        app.on_key(press(KeyCode::Esc));
        assert!(app.tx_detail.is_none());
        assert!(matches!(rx.try_recv(), Ok(Request::CloseTx)));
    }

    #[test]
    fn leaving_the_block_detail_clears_the_kept_tx() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // block detail
        app.on_key(press(KeyCode::Enter)); // request the tx
        let _ = rx.try_recv();
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        }); // tx detail opens
        app.on_key(press(KeyCode::Esc)); // back to tx list, tx kept
        assert!(app.tx_detail.is_some());
        app.on_key(press(KeyCode::Esc)); // leave the block context
        assert_eq!(app.focus, Focus::List);
        assert!(
            app.tx_detail.is_none(),
            "the kept tx doesn't leak onto the list"
        );
    }

    #[test]
    fn re_entering_the_open_tx_focuses_it_without_refetching() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // block detail (Focus::Block)
        app.on_key(press(KeyCode::Enter)); // request the selected tx
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        }); // tx detail opens
        app.on_key(press(KeyCode::Esc)); // back to the tx list, tx kept
        assert_eq!(app.focus, Focus::Block);
        assert!(app.tx_detail.is_some());
        // Right re-enters the same tx: it's already in the pane, so it re-focuses
        // without a second fetch.
        app.on_key(press(KeyCode::Right));
        assert_eq!(app.focus, Focus::Tx);
        assert!(
            rx.try_recv().is_err(),
            "the already-open tx must not be refetched"
        );
    }

    #[test]
    fn a_standalone_tx_returns_straight_to_the_list() {
        let TestApp { mut app, .. } = app();
        app.view = View::Mempool;
        app.apply(Update::Tx(mempool_row()));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.detail_origin, TxOrigin::Standalone);
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn down_in_a_focused_detail_scrolls_instead_of_walking() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block_two_txs(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        let _ = rx.try_recv();
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        });
        assert_eq!(app.focus, Focus::Tx);
        // Focused in the tx detail, Down scrolls the pane and fetches nothing; n walks.
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.detail_scroll, 1);
        assert!(rx.try_recv().is_err(), "arrows scroll, they don't walk");
        app.on_key(press(KeyCode::Char('n')));
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
    }

    #[test]
    fn n_walks_the_block_tx_list_from_the_unfocused_pane() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block_two_txs(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // block detail (Focus::Block)
        assert_eq!(app.focus, Focus::Block);
        // From the tx list, n opens the next tx into the detail preview.
        app.on_key(press(KeyCode::Char('n')));
        assert_eq!(app.block_tx_sel, 1);
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
    }

    #[test]
    fn walking_a_single_tx_block_does_not_refetch() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block(100, 1_000))); // one tx
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter)); // block detail
        app.on_key(press(KeyCode::Enter)); // fetch the only tx
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        }); // tx detail opens
        // Walking within a one-tx block stays put and sends nothing.
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Char('n')));
        assert!(rx.try_recv().is_err(), "no redundant fetch for a single tx");
    }

    #[test]
    fn n_in_a_block_tx_requests_another_tx() {
        let TestApp { mut app, mut rx } = app();
        app.apply(Update::MinedBlock(block_two_txs(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        let _ = rx.try_recv();
        app.apply(Update::SearchTx {
            detail: detail(),
            height: 0,
        });
        app.on_key(press(KeyCode::Char('n')));
        assert!(matches!(rx.try_recv(), Ok(Request::Transaction(_))));
    }

    #[test]
    fn esc_closes_the_block_detail() {
        let TestApp { mut app, .. } = app();
        app.apply(Update::MinedBlock(block(100, 1_000)));
        select_first_block(&mut app);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Esc));
        assert_eq!(app.focus, Focus::List);
        assert!(app.block_detail.is_none());
    }

    #[test]
    fn y_queues_the_tx_json_for_the_clipboard() {
        let TestApp { mut app, .. } = app();
        app.view = View::Mempool;
        app.apply(Update::Tx(mempool_row()));
        app.on_key(press(KeyCode::Enter)); // open the tx detail
        app.on_key(press(KeyCode::Char('y')));
        let payload = app.take_copy().expect("a payload was queued");
        assert!(payload.contains("\"txid\""));
        assert!(app.take_copy().is_none(), "taking clears it");
        assert!(app.footer_msg.as_deref().unwrap().contains("JSON"));
    }

    #[test]
    fn help_opens_and_any_key_dismisses() {
        let TestApp { mut app, .. } = app();
        app.on_key(press(KeyCode::Char('?')));
        assert_eq!(app.focus, Focus::Help);
        app.on_key(press(KeyCode::Char('j')));
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn a_disabled_gradient_never_advances_its_clock() {
        let TestApp { mut app, .. } = app();
        assert!(app.dither.is_none());
        for _ in 0..5 {
            app.tick_dither();
        }
        assert_eq!(app.dither_time(), 0.0);
    }

    #[test]
    fn the_field_clock_quantizes_to_the_repaint_step() {
        let TestApp { mut app, .. } = app();
        app.dither = Some(Charset::Blocks);
        // Two sub-step advances land in the same 90ms bucket: the frame is frozen.
        app.dither_secs = 0.04;
        let a = app.dither_time();
        app.dither_secs = 0.08;
        assert_eq!(app.dither_time(), a);
        // Crossing the step boundary advances t by one 90ms tick scaled by 0.5.
        app.dither_secs = 0.10;
        assert!(app.dither_time() > a);
    }

    #[test]
    fn space_pauses_and_q_quits_at_list_focus() {
        let TestApp { mut app, .. } = app();
        assert!(!app.paused);
        assert!(!app.on_key(press(KeyCode::Char(' '))));
        assert!(app.paused);
        assert!(app.on_key(press(KeyCode::Char('q'))));
    }
}
