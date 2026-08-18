//! Rendering. Every frame is a pure function of `App` plus the animation clock,
//! recomputed from `app.elapsed()` so the title shimmer animates on the frame
//! timer even when no transaction has arrived.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::{App, Phase, Row};
use crate::theme::{self, color};
use lightwallet_txview::{Pool, Value, format_zats};

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());

    render_header(f, header, app);
    render_body(f, body, app);
    render_footer(f, footer, app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = theme::shimmer("lwtui", app.elapsed());
    spans.push(Span::styled("   ", Style::default()));
    spans.push(status_span(app));
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(color(theme::BG))),
        area,
    );
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Block time is miner-reported and may sit slightly ahead of the local
    // clock; saturate rather than show a negative age.
    format!("{}s since block", now.saturating_sub(mined as u64))
}

fn render_body(f: &mut Frame, area: Rect, app: &App) {
    if app.detail && app.selected_index().is_some() {
        let [list, detail] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(48)]).areas(area);
        render_list(f, list, app);
        render_detail(f, detail, app);
    } else {
        render_list(f, area, app);
    }
    if let Some(alpha) = flash_alpha(app) {
        render_flash(f, area, app, alpha);
    }
}

/// Seconds the "mempool freed" flash lingers after a new block.
const FLASH_SECS: f32 = 0.8;

fn flash_alpha(app: &App) -> Option<f32> {
    let e = app.freed_at?.elapsed().as_secs_f32();
    (e < FLASH_SECS).then(|| 1.0 - e / FLASH_SECS)
}

/// A one-line banner across the top of the body that fades from the accent
/// color back to the background, so a new block reads as a visible pulse.
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

fn render_list(f: &mut Frame, area: Rect, app: &App) {
    if app.rows.is_empty() {
        let msg = match &app.phase {
            Phase::Live => format!("mempool empty at height {}", app.tip),
            Phase::Connecting => "connecting…".to_string(),
            Phase::Reconnecting(_) => "reconnecting…".to_string(),
        };
        f.render_widget(
            Paragraph::new(msg)
                .centered()
                .style(Style::default().fg(color(theme::FAINT))),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app.rows.iter().map(row_item).collect();
    let mut state = ListState::default();
    state.select(app.selected_index());
    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ")
        // Reserve the gutter always, so selecting a row never shifts the
        // columns to its right.
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(list, area, &mut state);
}

fn row_item(row: &Row) -> ListItem<'static> {
    let tx = &row.tx;
    let age = format!("{:>4.0}s", row.first_seen.elapsed().as_secs_f32());
    let id = match &tx.txid {
        Some(txid) => truncate_txid(txid),
        None => "unparseable".to_string(),
    };
    let pools = if tx.error.is_some() {
        "—".to_string()
    } else {
        format!("{} → {}", pool_list(&tx.inputs), pool_list(&tx.outputs))
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

fn render_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(row) = app.selected_index().and_then(|i| app.rows.get(i)) else {
        return;
    };
    let tx = &row.tx;
    let mut lines = Vec::new();
    match (&tx.txid, &tx.error) {
        (Some(txid), _) => lines.push(Line::from(txid.clone())),
        (None, Some(err)) => lines.push(Line::from(err.clone())),
        (None, None) => {}
    }
    lines.push(Line::from(""));
    if let Some(version) = &tx.version {
        lines.push(Line::from(format!(
            "version  {version}   {} bytes",
            tx.size
        )));
    }
    lines.push(Line::from(format!("inputs   {}", pool_list(&tx.inputs))));
    lines.push(Line::from(format!("outputs  {}", pool_list(&tx.outputs))));
    lines.push(Line::from(format!("value    {}", value_str(&tx.value))));
    lines.push(Line::from(format!(
        "fee      {}",
        tx.fee.map(format_zats).unwrap_or_else(|| "—".to_string())
    )));
    lines.push(Line::from(format!(
        "seen     {:.0}s ago",
        row.first_seen.elapsed().as_secs_f32()
    )));
    let block = Block::default().borders(Borders::LEFT).title("transaction");
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(color(theme::TEXT))),
        area,
    );
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let keys = if app.detail {
        "↑/↓ select  ·  enter/esc close  ·  space pause  ·  q quit"
    } else {
        "↑/↓ select  ·  enter detail  ·  space pause  ·  q quit"
    };
    f.render_widget(
        Paragraph::new(keys).style(Style::default().fg(color(theme::FAINT))),
        area,
    );
}

fn pool_list(pools: &[Pool]) -> String {
    if pools.is_empty() {
        return "none".to_string();
    }
    pools
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("+")
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
    use crate::app::{App, Row, Update};
    use lightwallet_txview::ParsedTx;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Instant;

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
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
    fn header_and_empty_state_render() {
        let mut app = App::new();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::Block {
            tip: 100,
            mined_unix: None,
        });
        let out = render(&app);
        assert!(out.contains("lwtui"));
        assert!(out.contains("mempool empty at height 100"));
    }

    #[test]
    fn a_row_shows_its_pools() {
        let mut app = App::new();
        app.apply(Update::Phase(Phase::Live));
        app.apply(Update::Tx(Row {
            seq: 0,
            first_seen: Instant::now(),
            tx: ParsedTx {
                txid: Some("ab".repeat(32)),
                version: Some("V5".into()),
                size: 500,
                inputs: vec![Pool::Sapling],
                outputs: vec![Pool::Orchard],
                value: Value::Shielded,
                fee: Some(10_000),
                error: None,
            },
        }));
        let out = render(&app);
        assert!(out.contains("sapling"));
        assert!(out.contains("orchard"));
        assert!(out.contains("1 pending"));
        // Shielded value renders as a redaction bar, not a number.
        assert!(out.contains("█"));
    }
}
