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

use crate::app::{App, BlockRow, Focus, Phase, ResolveProgress, Row, TaddrHit, TxOrigin, View};
use crate::dither;
use crate::health::Health;
use crate::sphere;
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
    // Help and about are modals over the current view, not replacements for it.
    if app.focus == Focus::Help {
        render_help(f, body);
    }
    render_footer(f, footer, app);
    // The gradient rides the empty space at the bottom of the focused pane, so
    // it renders last, over whatever is already drawn (skipping filled cells).
    gradient_band(f, body, footer, app);
    if app.focus == Focus::About {
        dim_area(f, f.area());
        darken_area(f, f.area(), BACKDROP_DROP);
        render_about(f, body, app);
    }
}

/// The column range the gradient may fill: the focused tx list (mempool) or the
/// focused tx view (an open tx detail), else nothing. Mirrors the body layout so the
/// band lines up with the pane that holds focus.
fn focused_pane(body: Rect, app: &App) -> Option<Rect> {
    let block_ctx = app.block_detail.is_some()
        && (app.focus == Focus::Block
            || (app.tx_detail.is_some() && app.detail_origin == TxOrigin::Block));
    match app.focus {
        Focus::Tx if block_ctx && body.width >= 120 => {
            let [_, _, right] = Layout::horizontal([
                Constraint::Length(36),
                Constraint::Fill(3),
                Constraint::Fill(4),
            ])
            .areas(body);
            Some(right)
        }
        Focus::Tx if block_ctx && body.width >= 84 => {
            let [_, right] =
                Layout::horizontal([Constraint::Fill(3), Constraint::Fill(4)]).areas(body);
            Some(right)
        }
        Focus::Tx if block_ctx => Some(body),
        Focus::Tx => {
            let [_, detail] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(DETAIL_RIGHT)])
                    .areas(body);
            Some(detail)
        }
        // The mempool is the tx list, so its tail carries the gradient (empty or
        // not). The blocks and results lists don't, keeping the first-load
        // "loading blocks…" screen flat.
        Focus::List if app.view == View::Mempool => Some(body),
        _ => None,
    }
}

/// Paint the gradient into the focused pane's empty tail, rising from the
/// screen bottom to halfway between it and the pane's last content line. Fills
/// only blank cells, so it never covers a row, and its background is the app
/// ground, so it blends into the pane.
fn gradient_band(f: &mut Frame, body: Rect, footer: Rect, app: &App) {
    let Some(charset) = app.dither else { return };
    let Some(pane) = focused_pane(body, app) else {
        return;
    };
    let buf = f.buffer_mut();

    // The last row in the pane (body only) that carries content. Skip the first
    // column: a pane's left border runs the full height and would otherwise mark
    // every row as filled, hiding the empty tail below a short tx detail.
    let mut last = pane.top();
    for y in pane.top()..pane.bottom() {
        let filled = (pane.left().saturating_add(1)..pane.right())
            .any(|x| buf.cell((x, y)).is_some_and(|c| c.symbol() != " "));
        if filled {
            last = y;
        }
    }
    let bottom = footer.bottom().saturating_sub(1);
    // The band fills the empty tail from just below the last content line to the
    // screen bottom. The field is sampled over this whole height, so the bright
    // lobes sit near the middle and fade up toward the content and down into the
    // footer, rather than floating as a short strip.
    let top = last + 1;
    if bottom <= top {
        return;
    }
    let region = Rect {
        x: pane.x,
        y: top,
        width: pane.width,
        height: bottom + 1 - top,
    };
    dither::render(buf, region, app.dither_time(), charset, true);
}

/// How far an unfocused panel's colors slide toward the background. Enough to
/// read as inactive, not so much that it stops being legible.
const DIM_AMOUNT: f32 = 0.45;

/// Extra darkening on the about backdrop, past the ground.
const BACKDROP_DROP: f32 = 0.18;

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

/// Pull a region's colors toward black by `amount`, background included.
fn darken_area(f: &mut Frame, area: Rect, amount: f32) {
    let black = theme::rgb(0, 0, 0);
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                if let Color::Rgb(r, g, b) = cell.fg {
                    cell.set_fg(theme::lerp(theme::rgb(r, g, b), black, amount));
                }
                if let Color::Rgb(r, g, b) = cell.bg {
                    cell.set_bg(theme::lerp(theme::rgb(r, g, b), black, amount));
                }
            }
        }
    }
}

fn fade(c: Color, reset_to: Option<theme::Rgb>) -> Color {
    let rgb = match c {
        Color::Rgb(r, g, b) => theme::rgb(r, g, b),
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
        None => spans.append(&mut status_line(app)),
    }
    if app.focus == Focus::Tx {
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

fn status_line(app: &App) -> Vec<Span<'static>> {
    match &app.phase {
        Phase::Connecting => vec![Span::styled("connecting…", fg(theme::FAINT))],
        Phase::Reconnecting(err) => {
            vec![Span::styled(
                format!("reconnecting… ({err})"),
                fg(theme::WARN),
            )]
        }
        Phase::Live => {
            let sep = || Span::styled("  ·  ", fg(theme::FAINT));
            // tip is a value (gold), the pending count a live readout (orange),
            // paused an alarm (red). The rest stays quiet.
            let mut spans = vec![
                Span::styled(format!("tip {}", app.tip), fg(theme::ACCENT_HI)),
                sep(),
                Span::styled(since_block(app), fg(theme::FAINT)),
                sep(),
                Span::styled(format!("{} pending", app.rows.len()), fg(theme::WARN)),
            ];
            if app.paused {
                spans.push(sep());
                spans.push(Span::styled(
                    "PAUSED",
                    fg(theme::BAD).add_modifier(Modifier::BOLD),
                ));
            }
            spans
        }
    }
}

fn since_block(app: &App) -> String {
    let Some(mined) = app.tip_mined_unix else {
        return "waiting for block".to_string();
    };
    format!("{}s since block", now_unix().saturating_sub(mined as u64))
}

/// The detail path, shown once a block or tx is in context: `blocks › #height ›
/// tx id`, with the last segment (where you are) brightened. Returns `None` on
/// the plain list, where the tip status reads in its place.
fn breadcrumb(app: &App) -> Option<Vec<Span<'static>>> {
    if app.block_detail.is_none() && app.tx_detail.is_none() {
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
    if let Some(detail) = &app.tx_detail {
        let id = detail
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
/// layout. The block-context view uses equal thirds instead.
const DETAIL_RIGHT: u16 = 52;

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    // A block is "in context" from the moment its detail opens until it closes,
    // whether or not a tx detail is open. The tx column is reserved throughout, so
    // opening or closing a tx fills or empties that column without moving the
    // block list or the block detail beside it.
    let block_ctx = app.block_detail.is_some()
        && (app.focus == Focus::Block
            || (app.tx_detail.is_some() && app.detail_origin == TxOrigin::Block));
    if block_ctx {
        render_block_columns(f, area, app);
    } else if app.tx_detail.is_some() {
        let [list, detail] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(DETAIL_RIGHT)]).areas(area);
        render_active_list(f, list, app);
        render_tx_detail(f, detail, app);
        // The tx detail holds focus here; fade the list behind it.
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
/// then collapse to a single pane that follows focus, so backing out of a tx
/// lands on the block detail it came from rather than on an invisible pane.
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
        // focused whenever a tx is open, mirroring the Tx focus.
        for (rect, focused) in [
            (list, app.focus == Focus::List),
            (mid, app.focus == Focus::Block),
            (right, app.focus == Focus::Tx),
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
        if app.focus != Focus::Tx {
            dim_area(f, right);
        }
    } else if app.focus == Focus::Tx && app.tx_detail.is_some() {
        render_tx_detail(f, area, app);
    } else {
        render_block_detail(f, area, app);
    }
}

/// The rightmost column: the open tx, or a reserved placeholder that holds
/// A column's left divider in the structural slate, titled. The border reads as
/// a seam between panes, not a frame competing with the content.
fn left_pane(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::LEFT)
        .border_style(fg(theme::BORDER))
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, fg(theme::ACCENT_CYAN)))
}

/// the column's width so opening a tx doesn't shift the layout.
fn render_tx_pane(f: &mut Frame, area: Rect, app: &App) {
    if app.tx_detail.is_some() {
        render_tx_detail(f, area, app);
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
        // Age ticks up live, so it reads as an orange live readout.
        Span::styled(age, Style::default().fg(color(theme::WARN))),
        Span::raw("  "),
        Span::styled(format!("{id:<18}"), Style::default().fg(color(theme::TEXT))),
        Span::raw("  "),
        value_span(&tx.value, value),
        Span::raw("  "),
        Span::styled(pools, Style::default().fg(color(theme::ACCENT_CYAN))),
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
            .fg(color(theme::ACCENT_CYAN))
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
    // Height is the spine of the list: gold on a live row, faded when reorged
    // or disconnected.
    let height = if b.reorged || dim {
        faint
    } else {
        Style::default().fg(color(theme::ACCENT_HI))
    };
    let age = now.saturating_sub(b.time as u64);
    let mut spans = vec![
        Span::styled(format!("{:>9}", b.height), height),
        Span::raw("  "),
        // The hash is a literal, so it reads olive on a live row.
        Span::styled(
            format!("{:<13}", short_block_hash(&b.hash)),
            if b.reorged || dim {
                faint
            } else {
                Style::default().fg(color(theme::OLIVE))
            },
        ),
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
            .fg(color(theme::ACCENT_CYAN))
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
            Style::default().fg(color(theme::ACCENT_HI)),
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
    // A warm lifted fill plus a bold row marks the selection and leaves each
    // column its own hue, rather than a full reverse bar that flattens the row
    // to two tones. The gold caret carries the cursor.
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(color(theme::SEL_FILL))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▍ ")
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
    // Values read gold; a shielded amount is coral, marking it redacted.
    let rgb = match value {
        Value::Shielded => theme::POOL_IRONWOOD,
        Value::Clear(_) => theme::ACCENT_HI,
        Value::Unknown => theme::FAINT,
    };
    Span::styled(text, Style::default().fg(color(rgb)))
}

fn render_tx_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(detail) = &app.tx_detail else {
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
            app.resolve_progress,
            &app.detail_input_values,
        )
    };
    // Clamp the scroll to the content so the last line can reach the top edge
    // but no further, never scrolling into blank below.
    let view_h = block.inner(area).height;
    let max = (lines.len() as u16).saturating_sub(view_h);
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.detail_scroll.min(max), 0))
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
    progress: Option<ResolveProgress>,
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

    // Value and fee lead, so they read near the top of a long input/output
    // list before the reader scrolls down.
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
        // While the total resolves, count the funders read so far, so the wait
        // reads as progress rather than a stuck spinner.
        if let Some(ResolveProgress { done, of }) =
            progress.filter(|_| matches!(tx.input_total, InputTotal::Pending))
        {
            lines.push(indented(vec![
                Span::styled("↳ resolving inputs ", fg(theme::FAINT)),
                Span::styled(format!("{done}/{of}"), fg(theme::ACCENT_HI)),
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
    // the tx detail and nowhere in the list. The section shows only when the tx
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
        Focus::Tx => "r raw⇄human · j/k scroll · n/N next · y/Y copy json/raw · esc close",
        Focus::Block => "j/k select · n/N walk tx · enter open · y copy txid · esc close",
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
        ("a", "about lwtui"),
        ("Tab", "switch mempool ⇄ blocks"),
        ("/", "search: height, txid, or t-address"),
        (
            "j / k  ·  ↑ / ↓",
            "move selection (scroll a focused tx detail)",
        ),
        ("Enter", "open detail: a block, then a tx; a mempool tx"),
        ("Esc", "close the detail or results view"),
        ("r", "tx detail: raw ⇄ human"),
        ("y / Y", "copy tx JSON / raw hex to clipboard"),
        ("n / N", "walk txs: block list or t-address results"),
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

/// The about modal: identity lines over an animated centerpiece, a shaded
/// sphere under an orbiting light hanging in the sky above a brighter cut of
/// the dither sea. Esc, `q`, or `a` closes the modal. The animation runs on the wall
/// clock, so it keeps spinning while the feed is paused. When the stage is
/// too small for the sphere, the sea fills it alone.
fn render_about(f: &mut Frame, area: Rect, app: &App) {
    // The dialog holds 3/5 of the width and 3/4 of the height, at any
    // terminal size; `centered` clamps the floors on a screen too small to
    // honor them.
    let w = (area.width * 3 / 5).max(24);
    let h = (area.height * 3 / 4).max(9);
    let modal = centered(area, w, h);
    f.render_widget(Clear, modal);
    let dialog = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(color(theme::ACCENT_DIM)))
        .title(Span::styled(
            "─about",
            Style::default().fg(color(theme::ACCENT_DIM)),
        ))
        .style(Style::default().bg(color(theme::BG)));
    let inner = dialog.inner(modal);
    f.render_widget(dialog, modal);

    let idlines = vec![
        Line::from(Span::styled(
            format!("lwtui {}", env!("CARGO_PKG_VERSION")),
            Style::default()
                .fg(color(theme::ACCENT_HI))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "a live terminal monitor for Zcash lightwallet indexers",
            Style::default().fg(color(theme::TEXT)),
        )),
        Line::from(Span::styled(
            "github.com/auzum197/lightwallet-tools",
            Style::default().fg(color(theme::FAINT)),
        )),
    ];
    let text_h = (idlines.len() as u16 + 1).min(inner.height);
    f.render_widget(
        Paragraph::new(idlines).alignment(Alignment::Center),
        Rect {
            height: text_h,
            ..inner
        },
    );

    let mut credit_at = None;
    if inner.height > text_h {
        let full = Rect {
            y: inner.y + text_h,
            height: inner.height - text_h,
            ..inner
        };
        let sea_h = (full.height / 3).max(2).min(full.height);
        let sky_h = full.height - sea_h;
        if sky_h >= 6 && full.width >= 14 {
            let ry = (sky_h as f32 / 2.0).min(full.width as f32 / 4.0);
            let cx = full.x as f32 + full.width as f32 / 2.0;
            let cy = full.y as f32 + sky_h as f32 / 2.0;
            let x = (cx + ry * 2.0 + 2.0) as u16;
            let y = (cy - ry * 0.5) as u16;
            if x + CREDIT.len() as u16 <= inner.right() {
                credit_at = Some((x, y));
            }
        }
        let stage = if credit_at.is_some() {
            full
        } else {
            Rect {
                height: full.height - 1,
                ..full
            }
        };
        if stage.height > 1 {
            let e = app.elapsed();
            // The sea steps at the dither field's 90ms cadence; the sphere runs
            // at the frame loop's full ~30fps so the orbit sweeps smoothly.
            let sea_t = (e / 0.09).floor() * 0.09 * 0.5;
            let sphere_t = (e / 0.033).floor() * 0.033;
            let charset = app.dither.unwrap_or(dither::Charset::Blocks);
            let buf = f.buffer_mut();
            // The bottom third is water, the sky above holds the sphere, so the
            // two never overlap. Too small a sky, and the water floods the stage.
            let sea_h = (stage.height / 3).max(2).min(stage.height);
            let sky_h = stage.height - sea_h;
            if sky_h >= 6 && stage.width >= 14 {
                let sea = Rect {
                    y: stage.bottom() - sea_h,
                    height: sea_h,
                    ..stage
                };
                dither::render_bright(buf, sea, sea_t, charset);
                let sky = Rect {
                    height: sky_h,
                    ..stage
                };
                sphere::render(buf, sky, sphere_t);
                sphere::shadow(buf, sea, sky, sphere_t);
                sphere::particles(buf, sky, sphere_t);
            } else {
                dither::render_bright(buf, stage, sea_t, charset);
            }
        }
    }
    let credit = Line::from(credit_spans(app.elapsed()));
    match credit_at {
        Some((x, y)) => f.render_widget(
            Paragraph::new(credit),
            Rect {
                x,
                y,
                width: CREDIT.len() as u16,
                height: 1,
            },
        ),
        None if inner.height > text_h => f.render_widget(
            Paragraph::new(credit).alignment(Alignment::Center),
            Rect {
                y: inner.bottom() - 1,
                height: 1,
                ..inner
            },
        ),
        None => {}
    }
}

/// The about pane's sign-off, drawn in the sphere's gold.
const CREDIT: &str = "Made with <3 by Auzum";

/// A cheap unit-interval hash for the credit shimmer's per-cycle jitter.
fn hash01(seed: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9e37_79b9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// The credit in its gold, with a single highlight pass sweeping across it
/// once per 8-second slot, at a per-slot random offset up to 3s. The
/// highlight spans six characters, brightest at its center, lifting the gold
/// toward warm white.
fn credit_spans(e: f32) -> Vec<Span<'static>> {
    const SLOT: f32 = 8.0;
    const JITTER: f32 = 3.0;
    const SWEEP: f32 = 0.9;
    const RADIUS: f32 = 3.0;
    let slot = (e / SLOT) as u32;
    let start = slot as f32 * SLOT + JITTER * hash01(slot);
    let local = e - start;
    let center = (local / SWEEP) * (CREDIT.len() as f32 + 2.0 * RADIUS) - RADIUS;
    CREDIT
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let w = if (0.0..SWEEP).contains(&local) {
                let d = (i as f32 - center).abs() / RADIUS;
                (1.0 - d * d).max(0.0)
            } else {
                0.0
            };
            Span::styled(
                ch.to_string(),
                Style::default().fg(theme::lerp(theme::ACCENT_HI, theme::TEXT, w)),
            )
        })
        .collect()
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
    use crate::app::{App, BlockRow, ChainHeights, Update};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc;

    fn app() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        App::new(tx, App::DEFAULT_TARGET, None)
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
    fn about_is_a_modal_over_the_current_view() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::MinedBlock(block(100, 1_700_000_000)));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('a'),
        ));
        let out = render(&app);
        assert!(
            out.contains("a live terminal monitor"),
            "the dialog is shown"
        );
        assert!(out.contains(concat!("lwtui ", env!("CARGO_PKG_VERSION"))));
        assert!(out.contains("Made with <3 by Auzum"));
        // The block list stays visible around the dialog: it's an overlay.
        assert!(out.contains("height"), "the view behind stays visible");
    }

    #[test]
    fn the_about_stage_carries_the_sphere_on_a_tall_terminal() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('a'),
        ));
        let out = render_sized(&app, 120, 60);
        // The lit face lands on the bright end of the ramp somewhere.
        assert!(
            out.contains('@') || out.contains('8') || out.contains('G'),
            "the sphere's bright face renders"
        );
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
    fn staking_action_renders_in_tx_detail() {
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
        app.apply(Update::SearchTx {
            detail: TxDetail {
                parsed,
                raw: Vec::new(),
            },
            height: 0,
        });
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
    fn narrow_single_pane_follows_focus_back_to_the_block_detail() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        let mut b = block(100, now_unix() as u32);
        b.tx_rows.push(crate::app::BlockTx {
            txid: "a".repeat(64),
            inputs: 1,
            outputs: 1,
        });
        app.apply(Update::MinedBlock(b));
        let enter = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter);
        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char('j'),
        ));
        app.on_key(enter); // block detail
        app.on_key(enter); // request the tx
        use crate::app::TxDetail;
        use lightwallet_txview::{InputTotal, ParsedTx, Value};
        let parsed = ParsedTx {
            txid: Some("a".repeat(64)),
            version: Some("V5".into()),
            size: 100,
            consensus_branch_id: Some("nu6".into()),
            lock_time: Some(0),
            expiry_height: Some(0),
            inputs: Vec::new(),
            outputs: Vec::new(),
            staking: None,
            value: Value::Shielded,
            vout_values: Vec::new(),
            prevouts: Vec::new(),
            input_total: InputTotal::None,
            fee_base: None,
            error: None,
        };
        app.apply(Update::SearchTx {
            detail: TxDetail {
                parsed,
                raw: Vec::new(),
            },
            height: 100,
        });
        assert_eq!(app.focus, Focus::Tx);
        let out = render_sized(&app, 70, 20);
        assert!(
            out.contains("transaction"),
            "under 84 cols the tx pane shows"
        );
        assert!(!out.contains("┤ block ├"), "and the block detail does not");

        app.on_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Esc,
        ));
        assert_eq!(app.focus, Focus::Block);
        assert!(
            app.tx_detail.is_some(),
            "the tx stays open behind the block pane"
        );
        let out = render_sized(&app, 70, 20);
        assert!(
            out.contains("block") && !out.contains("transaction"),
            "back lands on the block detail, not an invisible pane:\n{out}"
        );
    }

    #[test]
    fn healthy_indicator_shows_when_live() {
        let mut app = app();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::MinedBlock(block(100, now_unix() as u32)));
        app.apply(Update::Info(ChainHeights {
            block_height: 100,
            estimated_height: 100,
        }));
        let out = render(&app);
        assert!(out.contains("healthy"));
    }
}
