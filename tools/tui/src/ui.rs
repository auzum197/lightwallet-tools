//! Rendering. Every frame is a pure function of `App` plus the animation clock,
//! recomputed from `app.elapsed()` so the title shimmer and the syncing
//! indicator animate on the frame timer even when no update has arrived.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::{App, BlockRow, DrillOrigin, Focus, Phase, Row, TaddrHit, View};
use crate::health::Health;
use crate::theme::{self, color};
use lightwallet_txview::{
    Blob, Fee, InputTotal, PoolInput, PoolOutput, Staking, StakingKind, Value, format_zats,
    input_pools, output_pools,
};

pub fn draw(f: &mut Frame, app: &App) {
    // Paint the whole frame with the theme background so every pane sits on the
    // same dark fill instead of showing through to the terminal's own color.
    f.render_widget(
        Block::default().style(Style::default().bg(color(theme::BG))),
        f.area(),
    );
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

/// How far an unfocused panel's colors slide toward the background. Enough to
/// read as inactive, not so much that it stops being legible.
const DIM_AMOUNT: f32 = 0.45;

/// Fade an already-rendered region toward the background, marking it unfocused.
/// Cells left at the terminal default foreground (`Reset`) resolve to the theme
/// text color before fading, so plain text dims with everything else. The frame
/// is painted `theme::BG`, so fading a background cell toward BG is a no-op and
/// the dim is carried by the foreground.
fn dim_area(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_fg(fade(cell.fg, Some(theme::TEXT)));
                cell.set_bg(fade(cell.bg, None));
            }
        }
    }
}

fn fade(c: Color, reset_to: Option<theme::Rgb>) -> Color {
    let rgb = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => match reset_to {
            Some(rgb) => rgb,
            None => return c,
        },
        other => return other,
    };
    theme::lerp(rgb, theme::BG, DIM_AMOUNT)
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
    match breadcrumb(app) {
        Some(mut crumb) => spans.append(&mut crumb),
        None => spans.push(status_span(app)),
    }
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
        Phase::Connecting => Span::styled(
            format!("{} connecting", theme::spinner(app.elapsed())),
            Style::default().fg(color(theme::FAINT)),
        ),
        Phase::Reconnecting(_) => {
            Span::styled("⊘ reconnecting", Style::default().fg(color(theme::WARN)))
        }
        Phase::Live => match app.health(now_unix()) {
            Health::Healthy => Span::styled("● healthy", Style::default().fg(color(theme::GOOD))),
            Health::Stalled => Span::styled("○ stalled", Style::default().fg(color(theme::BAD))),
            // Syncing is the one in-motion state; a cycling spinner carries the
            // motion so the color can hold steady.
            Health::Syncing => Span::styled(
                format!("{} syncing", theme::spinner(app.elapsed())),
                Style::default()
                    .fg(color(theme::ACCENT_HI))
                    .add_modifier(Modifier::BOLD),
            ),
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

/// The drill path, shown once a block or tx is in context: `blocks › #height ›
/// tx id`, with the last segment (where you are) brightened. Returns `None` on
/// the plain list, where the tip status reads in its place.
fn breadcrumb(app: &App) -> Option<Vec<Span<'static>>> {
    if app.block_detail.is_none() && app.drill.is_none() {
        return None;
    }
    let mut labels = vec![
        match app.view {
            View::Mempool => "mempool",
            View::Blocks => "blocks",
            View::Results => "results",
        }
        .to_string(),
    ];
    if let Some(block) = &app.block_detail {
        labels.push(format!("#{}", block.height));
    }
    if let Some(drill) = &app.drill {
        let id = drill
            .parsed
            .txid
            .as_deref()
            .map(truncate_txid)
            .unwrap_or_else(|| "unparsed".to_string());
        labels.push(format!("tx {id}"));
    }
    let last = labels.len() - 1;
    let mut spans = Vec::new();
    for (i, label) in labels.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" › ", fg(theme::FAINT)));
        }
        let rgb = if i == last { theme::TEXT } else { theme::FAINT };
        spans.push(Span::styled(label, fg(rgb)));
    }
    Some(spans)
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
        // The drill holds focus here; fade the list behind it.
        dim_area(f, list);
    } else {
        render_active_list(f, area, app);
    }
    if let Some(alpha) = flash_alpha(app) {
        render_flash(f, area, app, alpha);
    }
}

/// The block-context columns: a fixed-width block list, then the block detail and
/// tx pane sharing the rest 3:4, so the deepest pane (the tx being read) gets the
/// most room and its hexdump can widen. The tx pane is reserved even before a tx
/// opens (a placeholder), so filling it never reflows the columns to its left. As
/// width drops, shed the block list first (it's the index you navigated from),
/// then collapse to a single pane.
fn render_block_columns(f: &mut Frame, area: Rect, app: &App) {
    if area.width >= 120 {
        let [list, mid, right] = Layout::horizontal([
            Constraint::Length(36),
            Constraint::Fill(3),
            Constraint::Fill(4),
        ])
        .areas(area);
        render_active_list(f, list, app);
        render_block_detail(f, mid, app);
        render_tx_pane(f, right, app);
        // Fade the two columns that don't hold focus. The tx pane counts as
        // focused whenever a tx is open, mirroring the Drill focus.
        for (rect, focused) in [
            (list, app.focus == Focus::List),
            (mid, app.focus == Focus::Block),
            (right, app.focus == Focus::Drill),
        ] {
            if !focused {
                dim_area(f, rect);
            }
        }
    } else if area.width >= 84 {
        let [mid, right] =
            Layout::horizontal([Constraint::Fill(3), Constraint::Fill(4)]).areas(area);
        render_block_detail(f, mid, app);
        render_tx_pane(f, right, app);
        if app.focus != Focus::Block {
            dim_area(f, mid);
        }
        if app.focus != Focus::Drill {
            dim_area(f, right);
        }
    } else if app.drill.is_some() {
        render_drill(f, area, app);
    } else {
        render_block_detail(f, area, app);
    }
}

/// The rightmost column: the drilled tx, or a reserved placeholder that holds
/// A column's left divider in the structural slate, titled. The border reads as
/// a seam between panes, not a frame competing with the content.
fn left_pane(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::LEFT)
        .border_style(fg(theme::BORDER))
        .padding(Padding::horizontal(1))
        .title(title)
}

/// the column's width so opening a tx doesn't shift the layout.
fn render_tx_pane(f: &mut Frame, area: Rect, app: &App) {
    if app.drill.is_some() {
        render_drill(f, area, app);
        return;
    }
    let outer = left_pane("transaction");
    let inner = outer.inner(area);
    f.render_widget(outer, area);
    placeholder(f, inner, "select a tx · enter");
}

fn render_block_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(block) = &app.block_detail else {
        return;
    };
    let outer = left_pane("block");
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
            "  {:>9}  {:<13}  {:>5}  {:>4}  {:>5}  {:>5}",
            "height", "hash", "age", "txs", "in", "out"
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
        Span::styled(format!("{:<13}", short_block_hash(&b.hash)), faint),
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
    // A soft slate fill marks the selection and leaves each column its own hue,
    // rather than a full reverse bar that flattens the row to two tones.
    let list = List::new(items)
        .highlight_style(Style::default().bg(color(theme::SEL_BG)))
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

/// The fee, with a word for each state that has no number yet: still resolving
/// its transparent inputs, or a lookup that failed. Never a wrong amount.
fn fee_str(fee: &Fee) -> String {
    match fee {
        Fee::Known(zats) => format_zats(*zats),
        Fee::Pending => "resolving…".to_string(),
        Fee::Unresolvable => "?".to_string(),
        Fee::Unknown => "—".to_string(),
    }
}

/// The transparent input total, or `None` when the tx has no transparent inputs
/// and there is nothing to show.
fn input_total_str(total: &InputTotal) -> Option<String> {
    match total {
        InputTotal::None => None,
        InputTotal::Pending => Some("resolving…".to_string()),
        InputTotal::Known(zats) => Some(format_zats(*zats)),
        InputTotal::Unresolvable => Some("?".to_string()),
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
    let block = left_pane("transaction");
    let inner_width = block.inner(area).width;
    let lines = if app.raw_mode || degraded {
        raw_lines(detail, degraded, inner_width)
    } else {
        human_lines(
            detail,
            theme::spinner(app.elapsed()),
            app.drill_probe.as_deref(),
            &app.drill_input_values,
        )
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

fn human_lines(
    detail: &crate::app::TxDetail,
    spin: char,
    probe: Option<&str>,
    input_values: &[Option<i64>],
) -> Vec<Line<'static>> {
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
        lines.push(meta_line("version", version.clone()));
        lines.push(meta_line(
            "branch",
            tx.consensus_branch_id.as_deref().unwrap_or("?").to_string(),
        ));
        lines.push(meta_line("size", format!("{} bytes", tx.size)));
    }
    if let Some(lock) = tx.lock_time {
        lines.push(meta_line("locktime", lock.to_string()));
    }
    if let Some(expiry) = tx.expiry_height {
        lines.push(meta_line("expiry", expiry.to_string()));
    }
    lines.push(Line::from(""));

    // Value and fee lead, so they stay visible above a long input/output list
    // (a block-opened drill can't scroll the pane).
    lines.push(Line::from(vec![
        Span::styled("value    ", fg(theme::FAINT)),
        value_span(&tx.value, value_str(&tx.value)),
    ]));
    // The transparent input total, shown only when there are inputs to resolve,
    // so a fully-shielded tx's pane stays uncluttered. A still-resolving total
    // carries the spinner, marking it as in-flight rather than a settled value.
    if let Some(text) = input_total_str(&tx.input_total) {
        let value = match tx.input_total {
            InputTotal::Pending => format!("{spin} {text}"),
            _ => text,
        };
        lines.push(Line::from(vec![
            Span::styled("t-inputs ", fg(theme::FAINT)),
            Span::styled(value, fg(theme::TEXT)),
        ]));
        // While the total resolves, name the funding tx being fetched right now,
        // so the wait reads as progress rather than a stuck spinner.
        if let Some(funder) = probe.filter(|_| matches!(tx.input_total, InputTotal::Pending)) {
            lines.push(indented(vec![
                Span::styled("↳ reading ", fg(theme::FAINT)),
                Span::styled(truncate_txid(funder), fg(theme::ACCENT_HI)),
            ]));
        }
    }
    let fee = tx.fee();
    let fee_rgb = match fee {
        Fee::Known(_) => theme::GOOD,
        _ => theme::FAINT,
    };
    let fee_text = match fee {
        Fee::Pending => format!("{spin} {}", fee_str(&fee)),
        _ => fee_str(&fee),
    };
    lines.push(Line::from(vec![
        Span::styled("fee      ", fg(theme::FAINT)),
        Span::styled(fee_text, fg(fee_rgb)),
    ]));
    lines.push(Line::from(""));

    lines.push(section_line("inputs", input_pools(&tx.inputs)));
    let inputs_pending = matches!(tx.input_total, InputTotal::Pending);
    for pi in &tx.inputs {
        lines.extend(input_detail(pi, input_values, spin, inputs_pending));
    }
    // A blank line keeps the two sides from running together.
    lines.push(Line::from(""));
    lines.push(section_line("outputs", output_pools(&tx.outputs)));
    for po in &tx.outputs {
        lines.extend(output_detail(po));
    }
    // A V7 staking action only rides in the full-tx bytes, so it renders here in
    // the drill-down and nowhere in the list. The section shows only when the tx
    // carries one; the bytes of every blob are already in the raw hexdump.
    if let Some(staking) = tx.staking.as_deref() {
        lines.push(Line::from(""));
        lines.extend(staking_lines(staking));
    }
    lines
}

fn staking_lines(s: &Staking) -> Vec<Line<'static>> {
    let mut lines = vec![section_line("staking", s.kind.title().to_string())];
    lines.push(staking_field("bond", short_hex(&s.bond_id)));
    if let Some(finalizer) = &s.target_finalizer {
        lines.push(staking_field("finalizer", short_hex(finalizer)));
    }
    if let Some(zat) = s.amount_zat {
        lines.push(indented(vec![
            Span::styled(format!("{:<10} ", "amount"), fg(theme::FAINT)),
            Span::styled(staking_amount(s.kind, zat), fg(theme::GOOD)),
        ]));
    }
    lines.push(staking_blob("challenge", s.challenge));
    lines.push(staking_blob("signature", s.signature));
    if let Some(blob) = s.second_challenge {
        lines.push(staking_blob("2nd chal", blob));
    }
    if let Some(blob) = s.finalizer_signature {
        lines.push(staking_blob("fin sig", blob));
    }
    lines
}

/// An indented `label   value` pair inside the staking section.
fn staking_field(label: &str, value: String) -> Line<'static> {
    indented(vec![
        Span::styled(format!("{label:<10} "), fg(theme::FAINT)),
        Span::styled(value, fg(theme::TEXT)),
    ])
}

/// A challenge/signature blob shown as its length, matching the JSON's
/// `{bytes:N}` presence: the bytes say nothing to a viewer and live in the raw
/// hexdump.
fn staking_blob(label: &str, blob: Blob) -> Line<'static> {
    indented(vec![
        Span::styled(format!("{label:<10} "), fg(theme::FAINT)),
        Span::styled(format!("{{bytes:{}}}", blob.bytes), fg(theme::FAINT)),
    ])
}

/// The staking amount signed by its value-balance effect, so the create/withdraw
/// contribution ties to the top-line value/fee math at a glance. A create locks
/// value into the bond (out); a withdraw returns it. Unbonding sits between them
/// and moves nothing, so the five zero-value actions never reach here.
fn staking_amount(kind: StakingKind, zat: i64) -> String {
    match kind {
        StakingKind::CreateNewDelegationBond => format!("-{} bonded", format_zats(zat)),
        StakingKind::WithdrawDelegationBond => format!("+{} withdrawn", format_zats(zat)),
        _ => format_zats(zat),
    }
}

fn input_detail(
    pi: &PoolInput,
    values: &[Option<i64>],
    spin: char,
    pending: bool,
) -> Vec<Line<'static>> {
    match pi {
        PoolInput::Transparent { vin } => vin
            .iter()
            .enumerate()
            .map(|(i, tin)| {
                let mut spans = vec![Span::styled("t-in  ", fg(theme::POOL_TRANSPARENT))];
                // The input's value: the amount once its funder is read, a
                // spinner placeholder while still resolving, or `?` when the
                // funder could not be read at all.
                match values.get(i).copied() {
                    Some(Some(value)) => spans.push(Span::styled(
                        format!("{}  ", format_zats(value)),
                        fg(theme::GOOD),
                    )),
                    Some(None) => {
                        spans.push(Span::styled("? ZEC  ", fg(theme::FAINT)));
                    }
                    None if pending => {
                        spans.push(Span::styled(format!("{spin} ZEC  "), fg(theme::FAINT)));
                    }
                    None => {}
                }
                // Where it came from, as `txid:vout`: the funding tx and which of
                // its outputs this input consumes.
                spans.push(Span::styled(
                    format!("{}:{}", short_hex(&tin.prevout_txid), tin.prevout_index),
                    fg(theme::FAINT),
                ));
                indented(spans)
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

fn raw_lines(detail: &crate::app::TxDetail, degraded: bool, width: u16) -> Vec<Line<'static>> {
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
    let per = hex_cols(width);
    for (i, chunk) in detail.raw.chunks(per).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        lines.push(Line::from(format!("{:08x}  {}", i * per, hex.join(" "))));
    }
    lines
}

/// Bytes per hexdump row that fit a `width`-col content area, in 8-byte groups
/// (min 8). Ten cols go to the `08x  ` offset, three per byte (two hex digits
/// and a separating space).
fn hex_cols(width: u16) -> usize {
    let avail = width.saturating_sub(10);
    ((avail / 3) as usize / 8 * 8).max(8)
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
        Focus::Drill => "r raw⇄human · j/k scroll · n/N next · y/Y copy json/raw · esc close",
        Focus::Block => "j/k select tx · enter open tx · y copy txid · esc close",
        _ => {
            "tab view · / search · enter detail · y/Y copy json/raw · space pause · ? help · q quit"
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

/// A block hash shortened for the list, in display (reversed) byte order.
fn short_block_hash(hash: &[u8]) -> String {
    let mut h = hash.to_vec();
    h.reverse();
    short_hex(&hex::encode(h))
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

    fn render_sized(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
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

    #[test]
    fn staking_action_renders_in_drill_down() {
        use crate::app::TxDetail;
        use lightwallet_txview::{Blob, InputTotal, ParsedTx, Staking, StakingKind, Value};

        let staking = Staking {
            kind: StakingKind::CreateNewDelegationBond,
            bond_id: "ab".repeat(32),
            challenge: Blob { bytes: 32 },
            signature: Blob { bytes: 64 },
            target_finalizer: Some("cd".repeat(32)),
            amount_zat: Some(150_000_000),
            second_challenge: None,
            finalizer_signature: None,
        };
        let parsed = ParsedTx {
            txid: Some("a".repeat(64)),
            version: Some("V7".into()),
            size: 200,
            consensus_branch_id: Some("crosslink".into()),
            lock_time: Some(0),
            expiry_height: Some(0),
            inputs: Vec::new(),
            outputs: Vec::new(),
            staking: Some(Box::new(staking)),
            value: Value::Shielded,
            vout_values: Vec::new(),
            prevouts: Vec::new(),
            input_total: InputTotal::None,
            fee_base: Some(1_000),
            error: None,
        };
        let mut app = app();
        app.awaiting_tx = true;
        app.apply(Update::SearchTx(TxDetail {
            parsed,
            raw: Vec::new(),
        }));
        let out = render_sized(&app, 100, 30);
        assert!(out.contains("staking"), "the staking section renders");
        assert!(
            out.contains("Create new delegation bond"),
            "the kind titles it"
        );
        assert!(
            out.contains("-1.5 ZEC bonded"),
            "the amount shows its signed direction"
        );
        assert!(
            out.contains("{bytes:64}"),
            "the signature shows as presence"
        );
    }

    #[test]
    fn block_context_shows_a_breadcrumb_and_keeps_the_list_when_wide() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::MinedBlock(block(100, now_unix() as u32)));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('j'),
        )); // select the block
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Enter,
        )); // open its detail
        let out = render_sized(&app, 130, 20);
        // The breadcrumb replaces the tip status once a block is in context.
        assert!(out.contains("blocks"), "the breadcrumb roots at the view");
        assert!(out.contains("#100"), "and names the open block");
        // At 130 cols all three columns show: the block list keeps its header.
        assert!(out.contains("height"), "the fixed list column survives");
        assert!(out.contains("transaction"), "the reserved tx pane shows");
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
