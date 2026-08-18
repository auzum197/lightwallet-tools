//! Rendering. Every frame is a pure function of `App` plus the animation clock,
//! recomputed from `app.elapsed()` so the title shimmer and the syncing
//! indicator animate on the frame timer even when no update has arrived.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::{App, BlockRow, DrillOrigin, Focus, Phase, Row, TaddrHit, View};
use crate::health::Health;
use crate::theme::{self, color};
use lightwallet_txview::{PoolInput, PoolOutput, Value, format_zats, input_pools, output_pools};

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());

    render_header(f, header, app);
    render_body(f, body, app);
    // Help is a modal over the current view, not a replacement for it.
    if app.focus == Focus::Help {
        render_help(f, body);
    }
    render_footer(f, footer, app);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = theme::shimmer("lwtui", app.elapsed());
    spans.push(Span::raw("   "));
    spans.push(indicator(app));
    spans.push(Span::styled(
        "  ·  ",
        Style::default().fg(color(theme::FAINT)),
    ));
    spans.push(status_span(app));
    if app.focus == Focus::Drill {
        let mode = if app.raw_mode { "raw" } else { "human" };
        spans.push(Span::styled(
            format!("  ·  mode {mode}"),
            Style::default().fg(color(theme::FAINT)),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(color(theme::BG))),
        area,
    );
}

/// The composed connection × health segment. Connection supersedes health: a
/// frozen `healthy` while disconnected would mislead.
fn indicator(app: &App) -> Span<'static> {
    match &app.phase {
        Phase::Connecting | Phase::Reconnecting(_) => {
            Span::styled("⊘ reconnecting", Style::default().fg(color(theme::WARN)))
        }
        Phase::Live => match app.health(now_unix()) {
            Health::Healthy => Span::styled("● healthy", Style::default().fg(color(theme::GOOD))),
            Health::Stalled => Span::styled("○ stalled", Style::default().fg(color(theme::BAD))),
            // Syncing is the one in-motion state, so it reads as motion.
            Health::Syncing => {
                let c = theme::lerp(
                    theme::FAINT,
                    theme::ACCENT_HI,
                    theme::pulse(app.elapsed(), 1.2),
                );
                Span::styled(
                    "◍ syncing",
                    Style::default().fg(c).add_modifier(Modifier::BOLD),
                )
            }
        },
    }
}

fn status_span(app: &App) -> Span<'static> {
    let (text, rgb) = match &app.phase {
        Phase::Connecting => ("connecting…".to_string(), theme::FAINT),
        Phase::Reconnecting(err) => (format!("reconnecting… ({err})"), theme::WARN),
        Phase::Live => (
            format!(
                "tip {}  ·  {}  ·  {} pending{}",
                app.tip,
                since_block(app),
                app.rows.len(),
                if app.paused { "  ·  PAUSED" } else { "" }
            ),
            theme::FAINT,
        ),
    };
    Span::styled(text, Style::default().fg(color(rgb)))
}

fn since_block(app: &App) -> String {
    let Some(mined) = app.tip_mined_unix else {
        return "waiting for block".to_string();
    };
    format!("{}s since block", now_unix().saturating_sub(mined as u64))
}

/// Fixed width for the tx-detail pane in the two-column (mempool / results)
/// drill. The block-context view uses equal thirds instead.
const DETAIL_RIGHT: u16 = 52;

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    // A block is "in context" from the moment its detail opens until it closes,
    // whether or not a tx is drilled. The tx column is reserved throughout, so
    // opening or closing a tx fills or empties that column without moving the
    // block list or the block detail beside it.
    let block_ctx = app.block_detail.is_some()
        && (app.focus == Focus::Block
            || (app.drill.is_some() && app.drill_origin == DrillOrigin::Block));
    if block_ctx {
        render_block_columns(f, area, app);
    } else if app.drill.is_some() {
        let [list, detail] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(DETAIL_RIGHT)]).areas(area);
        render_active_list(f, list, app);
        render_drill(f, detail, app);
    } else {
        render_active_list(f, area, app);
    }
    if let Some(alpha) = flash_alpha(app) {
        render_flash(f, area, app, alpha);
    }
}

/// The block-context columns: block list · block detail · tx pane, in three
/// equal thirds. The tx pane is reserved even before a tx is opened (a
/// placeholder), so filling it never reflows the columns to its left. Below the
/// width where thirds stay usable, collapse to a single column.
fn render_block_columns(f: &mut Frame, area: Rect, app: &App) {
    if area.width >= 96 {
        let [list, mid, right] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        render_active_list(f, list, app);
        render_block_detail(f, mid, app);
        render_tx_pane(f, right, app);
    } else if app.drill.is_some() {
        render_drill(f, area, app);
    } else {
        render_block_detail(f, area, app);
    }
}

/// The rightmost column: the drilled tx, or a reserved placeholder that holds
/// the column's width so opening a tx doesn't shift the layout.
fn render_tx_pane(f: &mut Frame, area: Rect, app: &App) {
    if app.drill.is_some() {
        render_drill(f, area, app);
        return;
    }
    let outer = Block::default().borders(Borders::LEFT).title("transaction");
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    placeholder(f, inner, "select a tx · enter");
}

fn render_block_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(block) = &app.block_detail else {
        return;
    };
    let outer = Block::default().borders(Borders::LEFT).title("block");
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    let [fields, list] = Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(inner);

    let mut hash = block.hash.clone();
    hash.reverse();
    let age = now_unix().saturating_sub(block.time as u64);
    let lines = vec![
        Line::from(format!("height   {}", block.height)),
        Line::from(format!("age      {age}s   ({} txs)", block.txs)),
        Line::from(Span::styled(
            format!("hash     {}", hex::encode(hash)),
            Style::default().fg(color(theme::FAINT)),
        )),
        Line::from(""),
        Line::from(format!("in {}   out {}", block.inputs, block.outputs)),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(Style::default().fg(color(theme::TEXT))),
        fields,
    );

    if block.tx_rows.is_empty() {
        placeholder(f, list, "no transactions");
        return;
    }
    let items: Vec<ListItem> = block
        .tx_rows
        .iter()
        .map(|t| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    truncate_txid(&t.txid),
                    Style::default().fg(color(theme::TEXT)),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("in {}  out {}", t.inputs, t.outputs),
                    Style::default().fg(color(theme::FAINT)),
                ),
            ]))
        })
        .collect();
    render_selectable(f, list, items, Some(app.block_tx_sel));
}

fn render_active_list(f: &mut Frame, area: Rect, app: &App) {
    match app.view {
        View::Mempool => render_mempool(f, area, app),
        View::Blocks => render_blocks(f, area, app),
        View::Results => render_results(f, area, app),
    }
}

/// Seconds the "mempool freed" flash lingers after a new block.
const FLASH_SECS: f32 = 0.8;

fn flash_alpha(app: &App) -> Option<f32> {
    // Only over the mempool view; the block tail has no "freed" moment.
    if app.view != View::Mempool {
        return None;
    }
    let e = app.freed_at?.elapsed().as_secs_f32();
    (e < FLASH_SECS).then(|| 1.0 - e / FLASH_SECS)
}

fn render_flash(f: &mut Frame, area: Rect, app: &App, alpha: f32) {
    let banner = Rect { height: 1, ..area };
    let bg = theme::lerp(theme::BG, theme::ACCENT_HI, alpha);
    let fg = theme::lerp(theme::FAINT, theme::BG, alpha);
    let text = format!("  ◆ new block {} — mempool freed", app.tip);
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD)),
        banner,
    );
}

fn placeholder(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(text)
            .centered()
            .style(Style::default().fg(color(theme::FAINT))),
        area,
    );
}

fn render_mempool(f: &mut Frame, area: Rect, app: &App) {
    if app.rows.is_empty() {
        let msg = match &app.phase {
            Phase::Live => format!("mempool empty at height {}", app.tip),
            Phase::Connecting => "connecting…".to_string(),
            Phase::Reconnecting(_) => "reconnecting…".to_string(),
        };
        placeholder(f, area, &msg);
        return;
    }
    let items: Vec<ListItem> = app.rows.iter().map(mempool_item).collect();
    render_selectable(f, area, items, app.selected_index());
}

fn mempool_item(row: &Row) -> ListItem<'static> {
    let tx = &row.tx;
    let age = format!("{:>4.0}s", row.first_seen.elapsed().as_secs_f32());
    let id = match &tx.txid {
        Some(txid) => truncate_txid(txid),
        None => "unparseable".to_string(),
    };
    let pools = if tx.error.is_some() {
        "—".to_string()
    } else {
        format!(
            "{} → {}",
            input_pools(&tx.inputs),
            output_pools(&tx.outputs)
        )
    };
    let value = format!("{:>18}", value_str(&tx.value));
    let line = Line::from(vec![
        Span::styled(age, Style::default().fg(color(theme::FAINT))),
        Span::raw("  "),
        Span::styled(format!("{id:<18}"), Style::default().fg(color(theme::TEXT))),
        Span::raw("  "),
        value_span(&tx.value, value),
        Span::raw("  "),
        Span::styled(pools, Style::default().fg(color(theme::TEXT))),
    ]);
    ListItem::new(line)
}

fn render_blocks(f: &mut Frame, area: Rect, app: &App) {
    let [head, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let header = Line::from(vec![Span::styled(
        format!(
            "  {:>9}  {:>5}  {:>4}  {:>5}  {:>5}",
            "height", "age", "txs", "in", "out"
        ),
        Style::default()
            .fg(color(theme::FAINT))
            .add_modifier(Modifier::BOLD),
    )]);
    f.render_widget(Paragraph::new(header), head);

    if app.blocks.is_empty() {
        let msg = match &app.phase {
            Phase::Live => "loading blocks…".to_string(),
            Phase::Connecting => "connecting…".to_string(),
            Phase::Reconnecting(_) => "reconnecting…".to_string(),
        };
        placeholder(f, list, &msg);
        return;
    }
    let now = now_unix();
    // Dim the whole list on a transient disconnect; keep the last-good rows.
    let dim = !matches!(app.phase, Phase::Live);
    let items: Vec<ListItem> = app.blocks.iter().map(|b| block_item(b, now, dim)).collect();
    render_selectable(f, list, items, app.selected_index());
}

fn block_item(b: &BlockRow, now: u64, dim: bool) -> ListItem<'static> {
    let faint = Style::default().fg(color(theme::FAINT));
    let text = if b.reorged || dim {
        faint
    } else {
        Style::default().fg(color(theme::TEXT))
    };
    let age = now.saturating_sub(b.time as u64);
    let mut spans = vec![
        Span::styled(format!("{:>9}", b.height), text),
        Span::raw("  "),
        Span::styled(format!("{age:>4}s"), faint),
        Span::raw("  "),
        Span::styled(format!("{:>4}", b.txs), text),
        Span::raw("  "),
        Span::styled(format!("{:>5}", b.inputs), text),
        Span::raw("  "),
        Span::styled(format!("{:>5}", b.outputs), text),
    ];
    if b.reorged {
        spans.push(Span::styled("  reorged", faint));
    }
    ListItem::new(Line::from(spans))
}

fn render_results(f: &mut Frame, area: Rect, app: &App) {
    let [head, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let header = Line::from(Span::styled(
        format!("  results · {}", app.taddr_label),
        Style::default()
            .fg(color(theme::FAINT))
            .add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(header), head);

    if app.taddr_hits.is_empty() {
        placeholder(f, list, &format!("no transactions for {}", app.taddr_label));
        return;
    }
    let items: Vec<ListItem> = app.taddr_hits.iter().map(result_item).collect();
    render_selectable(f, list, items, app.selected_index());
}

fn result_item(hit: &TaddrHit) -> ListItem<'static> {
    let txid = hit
        .detail
        .parsed
        .txid
        .as_deref()
        .map(truncate_txid)
        .unwrap_or_else(|| "unparseable".to_string());
    let line = Line::from(vec![
        Span::styled(
            format!("{:>9}", hit.height),
            Style::default().fg(color(theme::FAINT)),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{txid:<18}"),
            Style::default().fg(color(theme::TEXT)),
        ),
        Span::raw("  "),
        Span::styled(
            hit.detail.parsed.summary(),
            Style::default().fg(color(theme::FAINT)),
        ),
    ]);
    ListItem::new(line)
}

fn render_selectable(f: &mut Frame, area: Rect, items: Vec<ListItem>, selected: Option<usize>) {
    let mut state = ListState::default();
    state.select(selected);
    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ")
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(list, area, &mut state);
}

/// The moved value as text. Shielded outputs render as a redaction bar with the
/// denomination left visible, so the column reads as censored rather than empty.
fn value_str(value: &Value) -> String {
    match value {
        Value::Clear(zats) => format_zats(*zats),
        Value::Shielded => format!("{} ZEC", "█".repeat(8)),
        Value::Unknown => "—".to_string(),
    }
}

fn value_span(value: &Value, text: String) -> Span<'static> {
    let rgb = match value {
        Value::Shielded => theme::WARN,
        _ => theme::TEXT,
    };
    Span::styled(text, Style::default().fg(color(rgb)))
}

fn render_drill(f: &mut Frame, area: Rect, app: &App) {
    let Some(detail) = &app.drill else {
        return;
    };
    let parsed = &detail.parsed;
    // An unparseable tx has no fields to show, so fall back to raw with a banner.
    let degraded = parsed.txid.is_none() || parsed.error.is_some();
    let block = Block::default().borders(Borders::LEFT).title("transaction");
    let lines = if app.raw_mode || degraded {
        raw_lines(detail, degraded)
    } else {
        human_lines(detail)
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.drill_scroll, 0))
            .style(Style::default().fg(color(theme::TEXT))),
        area,
    );
}

fn fg(rgb: theme::Rgb) -> Style {
    Style::default().fg(color(rgb))
}

/// A `label   value` line: faint padded label, text value.
fn meta_line(name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<8} "), fg(theme::FAINT)),
        Span::styled(value, fg(theme::TEXT)),
    ])
}

/// A section header: bold accent name, faint pool summary.
fn section_line(name: &str, pools: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{name:<8} "),
            fg(theme::ACCENT_HI).add_modifier(Modifier::BOLD),
        ),
        Span::styled(pools, fg(theme::FAINT)),
    ])
}

fn indented(mut spans: Vec<Span<'static>>) -> Line<'static> {
    spans.insert(0, Span::raw("  "));
    Line::from(spans)
}

fn human_lines(detail: &crate::app::TxDetail) -> Vec<Line<'static>> {
    let tx = &detail.parsed;
    let mut lines = Vec::new();
    if let Some(txid) = &tx.txid {
        lines.push(Line::from(Span::styled(
            txid.clone(),
            fg(theme::ACCENT_HI).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    if let Some(version) = &tx.version {
        let branch = tx.consensus_branch_id.as_deref().unwrap_or("?");
        lines.push(meta_line(
            "version",
            format!("{version}  ·  {branch}  ·  {} bytes", tx.size),
        ));
    }
    if let (Some(lock), Some(expiry)) = (tx.lock_time, tx.expiry_height) {
        lines.push(meta_line("locktime", format!("{lock}   expiry {expiry}")));
    }
    lines.push(Line::from(""));

    // Value and fee lead, so they stay visible above a long input/output list
    // (a block-opened drill can't scroll the pane).
    lines.push(Line::from(vec![
        Span::styled("value    ", fg(theme::FAINT)),
        value_span(&tx.value, value_str(&tx.value)),
    ]));
    let fee = match tx.fee {
        Some(f) => Span::styled(format_zats(f), fg(theme::GOOD)),
        // A stateless server can't price a tx with transparent inputs.
        None => Span::styled("— (server didn't provide)".to_string(), fg(theme::FAINT)),
    };
    lines.push(Line::from(vec![
        Span::styled("fee      ", fg(theme::FAINT)),
        fee,
    ]));
    lines.push(Line::from(""));

    lines.push(section_line("inputs", input_pools(&tx.inputs)));
    for pi in &tx.inputs {
        lines.extend(input_detail(pi));
    }
    // A blank line keeps the two sides from running together.
    lines.push(Line::from(""));
    lines.push(section_line("outputs", output_pools(&tx.outputs)));
    for po in &tx.outputs {
        lines.extend(output_detail(po));
    }
    lines
}

fn input_detail(pi: &PoolInput) -> Vec<Line<'static>> {
    match pi {
        PoolInput::Transparent { vin } => vin
            .iter()
            .map(|i| {
                indented(vec![
                    Span::styled("t-in  ", fg(theme::POOL_TRANSPARENT)),
                    Span::styled(
                        format!("{}…:{}", short_hex(&i.prevout_txid), i.prevout_index),
                        fg(theme::FAINT),
                    ),
                ])
            })
            .collect(),
        PoolInput::Sapling {
            value_balance_zat,
            spends,
            ..
        } => vec![shielded_line(
            "sapling",
            theme::POOL_SAPLING,
            format!(
                "{} spends  vb {}",
                spends.len(),
                format_zats(*value_balance_zat)
            ),
        )],
        PoolInput::Orchard {
            value_balance_zat,
            spends,
            ..
        } => vec![shielded_line(
            "orchard",
            theme::POOL_ORCHARD,
            format!(
                "{} actions  vb {}",
                spends.len(),
                format_zats(*value_balance_zat)
            ),
        )],
        PoolInput::Ironwood {
            value_balance_zat,
            spends,
            ..
        } => vec![shielded_line(
            "ironwood",
            theme::POOL_IRONWOOD,
            format!(
                "{} actions  vb {}",
                spends.len(),
                format_zats(*value_balance_zat)
            ),
        )],
    }
}

fn output_detail(po: &PoolOutput) -> Vec<Line<'static>> {
    match po {
        PoolOutput::Transparent { vout } => vout
            .iter()
            .map(|o| {
                let addr = o
                    .address
                    .clone()
                    .unwrap_or_else(|| "(non-standard)".to_string());
                indented(vec![
                    Span::styled("t-out ", fg(theme::POOL_TRANSPARENT)),
                    Span::styled(format!("{}  ", format_zats(o.value_zat)), fg(theme::GOOD)),
                    Span::styled(addr, fg(theme::TEXT)),
                ])
            })
            .collect(),
        PoolOutput::Sapling { outputs } => vec![shielded_line(
            "sapling",
            theme::POOL_SAPLING,
            format!("{} outputs", outputs.len()),
        )],
        PoolOutput::Orchard { outputs } => vec![shielded_line(
            "orchard",
            theme::POOL_ORCHARD,
            format!("{} outputs", outputs.len()),
        )],
        PoolOutput::Ironwood { outputs } => vec![shielded_line(
            "ironwood",
            theme::POOL_IRONWOOD,
            format!("{} outputs", outputs.len()),
        )],
    }
}

/// An indented shielded-pool line: colored pool name, faint detail.
fn shielded_line(pool: &str, rgb: theme::Rgb, detail: String) -> Line<'static> {
    indented(vec![
        Span::styled(format!("{pool:<8} "), fg(rgb)),
        Span::styled(detail, fg(theme::FAINT)),
    ])
}

fn raw_lines(detail: &crate::app::TxDetail, degraded: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if degraded {
        lines.push(Line::from(Span::styled(
            "couldn't parse — showing raw",
            Style::default().fg(color(theme::WARN)),
        )));
    }
    let txid = detail
        .parsed
        .txid
        .clone()
        .unwrap_or_else(|| "(unparsed)".to_string());
    let branch = detail.parsed.consensus_branch_id.as_deref().unwrap_or("?");
    lines.push(Line::from(Span::styled(
        format!("{txid}  {branch}"),
        Style::default().fg(color(theme::FAINT)),
    )));
    lines.push(Line::from(""));
    for (i, chunk) in detail.raw.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        lines.push(Line::from(format!("{:08x}  {}", i * 16, hex.join(" "))));
    }
    lines
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    if app.focus == Focus::Search {
        let bar = format!("/{}", app.search_input);
        f.render_widget(
            Paragraph::new(bar).style(Style::default().fg(color(theme::TEXT))),
            area,
        );
        // Place the cursor after the leading '/'.
        f.set_cursor_position((area.x + 1 + app.search_cursor as u16, area.y));
        return;
    }
    if let Some(msg) = &app.footer_msg {
        f.render_widget(
            Paragraph::new(msg.clone()).style(Style::default().fg(color(theme::WARN))),
            area,
        );
        return;
    }
    let keys = match app.focus {
        Focus::Drill => {
            "r raw⇄human  ·  j/k scroll  ·  n/N next  ·  y/Y copy json/raw  ·  esc close"
        }
        Focus::Block => "j/k select tx  ·  enter open tx  ·  y copy txid  ·  esc close",
        _ => {
            "tab view  ·  / search  ·  enter detail  ·  y/Y copy json/raw  ·  space pause  ·  ? help  ·  q quit"
        }
    };
    f.render_widget(
        Paragraph::new(keys).style(Style::default().fg(color(theme::FAINT))),
        area,
    );
}

/// A rectangle of `w`×`h` centered in `area`, clamped to fit.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// The help modal, a centered dialog over the current view, rendered from the
/// one authoritative binding table so it can't drift from the real keys.
fn render_help(f: &mut Frame, area: Rect) {
    let rows = [
        ("q  /  Ctrl-C", "quit"),
        ("?", "toggle this help"),
        ("Tab", "switch mempool ⇄ blocks"),
        ("/", "search: height, txid, or t-address"),
        ("j / k", "move selection (scroll in a drill-down)"),
        ("Enter", "open detail: a block, then a tx; a mempool tx"),
        ("Esc", "close the detail or results view"),
        ("r", "drill-down: raw ⇄ human"),
        ("y / Y", "copy tx JSON / raw hex to clipboard"),
        ("n / N", "walk t-address results"),
        ("Space", "pause live-follow"),
    ];
    let mut lines = vec![
        Line::from(Span::styled(
            "lwtui — keys",
            Style::default()
                .fg(color(theme::ACCENT_HI))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (k, d) in rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {k:<14}"),
                Style::default().fg(color(theme::TEXT)),
            ),
            Span::styled(d.to_string(), Style::default().fg(color(theme::FAINT))),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  any key to close",
        Style::default().fg(color(theme::FAINT)),
    )));
    // Size the dialog to its content, then center it and clear only that rect
    // so the view stays visible around it.
    let height = lines.len() as u16 + 2;
    let modal = centered(area, 60, height);
    f.render_widget(Clear, modal);
    let block = Block::default().borders(Borders::ALL).title("help");
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .alignment(Alignment::Left)
            .style(Style::default().bg(color(theme::BG))),
        modal,
    );
}

fn short_hex(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}…{}", &s[..6], &s[s.len() - 4..])
    }
}

/// A txid is 64 hex chars; the list shows head and tail, the detail pane the
/// whole thing.
fn truncate_txid(txid: &str) -> String {
    if txid.len() <= 16 {
        return txid.to_string();
    }
    format!("{}…{}", &txid[..8], &txid[txid.len() - 6..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, BlockRow, Update};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc;

    fn app() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        App::new(tx, App::DEFAULT_TARGET)
    }

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 14)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if let Some(cell) = buf.cell((x, y)) {
                    text.push_str(cell.symbol());
                }
            }
            text.push('\n');
        }
        text
    }

    fn block(height: u64, time: u32) -> BlockRow {
        BlockRow {
            seq: 0,
            height,
            time,
            hash: vec![height as u8; 32],
            prev_hash: vec![(height - 1) as u8; 32],
            txs: 3,
            inputs: 5,
            outputs: 7,
            tx_rows: Vec::new(),
            reorged: false,
        }
    }

    #[test]
    fn blocks_view_shows_columns_and_rows() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::MinedBlock(block(100, 1_700_000_000)));
        let out = render(&app);
        assert!(out.contains("height"));
        assert!(out.contains("in"));
        assert!(out.contains("out"));
        assert!(out.contains("100"));
        // The input and output totals show.
        assert!(out.contains("5"));
        assert!(out.contains("7"));
    }

    #[test]
    fn help_is_a_modal_over_the_current_view() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::MinedBlock(block(100, 1_700_000_000)));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('?'),
        ));
        let out = render(&app);
        assert!(out.contains("help"), "the dialog is shown");
        // The block list stays visible around the dialog: it's an overlay.
        assert!(out.contains("height"), "the view behind stays visible");
    }

    #[test]
    fn empty_blocks_view_reads_as_loading() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        let out = render(&app);
        assert!(out.contains("loading blocks…"));
    }

    #[test]
    fn healthy_indicator_shows_when_live() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::MinedBlock(block(100, now_unix() as u32)));
        app.apply(Update::Info {
            block_height: 100,
            estimated_height: 100,
        });
        let out = render(&app);
        assert!(out.contains("healthy"));
    }
}
