//! UI-owned state and the input/update handling. The app is the single owner
//! of the row list; the network task only feeds it `Update`s over a channel.

use std::collections::VecDeque;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lightwallet_txview::ParsedTx;
use tokio::sync::mpsc::UnboundedReceiver;

/// A mempool transaction as the monitor tracks it: the shared projection plus
/// the two view-local facts txview doesn't carry. `seq` is assigned by the
/// producer and is what both a selection and a later value patch bind to, so
/// incoming rows never move the cursor off the tx being read and a resolved
/// input total lands on the right row.
pub struct Row {
    pub seq: u64,
    pub first_seen: Instant,
    pub tx: ParsedTx,
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
}

/// The whole monitor state. Rendered as a pure function of this plus the clock.
pub struct App {
    pub rows: VecDeque<Row>,
    pub phase: Phase,
    pub tip: u64,
    /// The tip block's miner timestamp (Unix seconds), the source for the
    /// "since block" clock. `None` when the server reported no time.
    pub tip_mined_unix: Option<u32>,
    /// When the tip last advanced, driving the "mempool freed" flash. Set only
    /// on a genuine new block, not the first connect or a same-height reconnect.
    pub freed_at: Option<Instant>,
    pub paused: bool,
    pub detail: bool,
    /// The `seq` of the selected row. Stable across prepends and clears.
    selected: Option<u64>,
    start: Instant,
}

const MAX_ROWS: usize = 5_000;

impl App {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            rows: VecDeque::new(),
            phase: Phase::Connecting,
            tip: 0,
            tip_mined_unix: None,
            freed_at: None,
            paused: false,
            detail: false,
            selected: None,
            start: now,
        }
    }

    /// Seconds since start, the shimmer's animation clock.
    pub fn elapsed(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Drain everything the network task has queued. Called only while running
    /// (not paused), so a paused list holds still and the channel buffers.
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
                // Flash only on a real new block, not the first connect (tip 0)
                // or a reconnect at the same height.
                if self.tip != 0 && tip > self.tip {
                    self.freed_at = Some(Instant::now());
                }
                self.rows.clear();
                self.selected = None;
                self.tip = tip;
                self.tip_mined_unix = mined_unix;
            }
            Update::Phase(phase) => self.phase = phase,
        }
    }

    /// The index of the selected row in current display order, if any.
    pub fn selected_index(&self) -> Option<usize> {
        let seq = self.selected?;
        self.rows.iter().position(|r| r.seq == seq)
    }

    /// Handle a keypress. Returns true to quit.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => return true,
            _ if ctrl_c => return true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            // Enter toggles: open the detail, or close it if already open.
            KeyCode::Enter if self.detail => self.detail = false,
            KeyCode::Enter | KeyCode::Right => {
                if self.selected.is_none() {
                    self.select_first();
                }
                self.detail = self.selected.is_some();
            }
            KeyCode::Esc | KeyCode::Left => self.detail = false,
            KeyCode::Char(' ') => self.paused = !self.paused,
            _ => {}
        }
        false
    }

    fn select_first(&mut self) {
        self.selected = self.rows.front().map(|r| r.seq);
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let next = match self.selected_index() {
            None => 0,
            Some(i) => (i as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
        };
        self.selected = self.rows.get(next).map(|r| r.seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightwallet_txview::{InputTotal, OutPoint, parse};

    /// A shielded row with no transparent inputs, addressed by `seq`.
    fn tx_row(seq: u64) -> Box<Row> {
        Box::new(Row {
            seq,
            first_seen: Instant::now(),
            // Real bytes are hard to synthesize; a degraded parse gives a valid
            // ParsedTx and these tests only exercise the row list, not values.
            tx: parse(&[0xff, 0x00], 0),
        })
    }

    /// A row whose transparent input total is awaiting resolution.
    fn pending_row(seq: u64) -> Box<Row> {
        let mut tx = parse(&[0xff, 0x00], 0);
        tx.prevouts = vec![OutPoint {
            hash: [1u8; 32],
            index: 0,
        }];
        tx.input_total = InputTotal::Pending;
        Box::new(Row {
            seq,
            first_seen: Instant::now(),
            tx,
        })
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn newest_row_is_front_carrying_its_producer_seq() {
        let mut app = App::new();
        app.apply(Update::Tx(tx_row(0)));
        app.apply(Update::Tx(tx_row(1)));
        assert_eq!(app.rows.len(), 2);
        assert_eq!(app.rows[0].seq, 1);
        assert_eq!(app.rows[1].seq, 0);
    }

    #[test]
    fn a_resolved_total_lands_on_its_row_and_a_stale_one_is_dropped() {
        let mut app = App::new();
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
    fn selection_stays_on_its_tx_across_a_prepend() {
        let mut app = App::new();
        app.apply(Update::Tx(tx_row(0)));
        app.select_first();
        assert_eq!(app.selected_index(), Some(0));
        app.apply(Update::Tx(tx_row(1)));
        // The same tx is now at index 1, and the cursor followed it there.
        assert_eq!(app.selected_index(), Some(1));
    }

    #[test]
    fn a_block_clears_the_pending_set_and_selection() {
        let mut app = App::new();
        app.apply(Update::Tx(tx_row(0)));
        app.select_first();
        app.apply(Update::Block {
            tip: 42,
            mined_unix: None,
        });
        assert!(app.rows.is_empty());
        assert_eq!(app.tip, 42);
        assert_eq!(app.selected_index(), None);
    }

    #[test]
    fn j_and_k_walk_the_selection() {
        let mut app = App::new();
        app.apply(Update::Tx(tx_row(0)));
        app.apply(Update::Tx(tx_row(1)));
        app.on_key(press(KeyCode::Char('j')));
        assert_eq!(app.selected_index(), Some(0));
        app.on_key(press(KeyCode::Char('j')));
        assert_eq!(app.selected_index(), Some(1));
        app.on_key(press(KeyCode::Char('k')));
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn block_time_feeds_the_since_block_clock() {
        let mut app = App::new();
        assert!(app.tip_mined_unix.is_none());
        app.apply(Update::Block {
            tip: 100,
            mined_unix: Some(1_700_000_000),
        });
        assert_eq!(app.tip, 100);
        assert_eq!(app.tip_mined_unix, Some(1_700_000_000));
    }

    #[test]
    fn a_new_block_arms_the_flash() {
        let mut app = App::new();
        app.apply(Update::Block {
            tip: 100,
            mined_unix: None,
        });
        assert!(app.freed_at.is_none(), "first connect does not flash");
        app.apply(Update::Block {
            tip: 100,
            mined_unix: None,
        });
        assert!(app.freed_at.is_none(), "a same-height reconnect does not");
        app.apply(Update::Block {
            tip: 101,
            mined_unix: None,
        });
        assert!(app.freed_at.is_some(), "a higher tip arms the flash");
    }

    #[test]
    fn enter_toggles_the_detail_pane() {
        let mut app = App::new();
        app.apply(Update::Tx(tx_row(0)));
        app.on_key(press(KeyCode::Enter));
        assert!(app.detail, "enter opens the detail");
        app.on_key(press(KeyCode::Enter));
        assert!(!app.detail, "enter again closes it");
    }

    #[test]
    fn space_toggles_pause_and_q_quits() {
        let mut app = App::new();
        assert!(!app.paused);
        assert!(!app.on_key(press(KeyCode::Char(' '))));
        assert!(app.paused);
        assert!(app.on_key(press(KeyCode::Char('q'))));
    }
}
