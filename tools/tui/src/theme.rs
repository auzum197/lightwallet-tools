//! Colors and the shimmer math: a cosine pulse whose phase drifts per glyph, so
//! the bright point travels across the title string.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub type Rgb = (u8, u8, u8);

// Accent (brand) — warm gold. The title shimmer sweeps DIM -> HI. Gold marks
// values, active and focus.
pub const ACCENT_DIM: Rgb = (0x7a, 0x5c, 0x2e);
pub const ACCENT_HI: Rgb = (0xd3, 0xa0, 0x50);
// Cyan marks section headers, tags and column titles. Olive marks code and
// literals.
pub const ACCENT_CYAN: Rgb = (0xa8, 0xd5, 0xd5);
pub const OLIVE: Rgb = (0xa0, 0xa3, 0x44);

// Ground and chrome, the warm palette. `BG` is painted on every cell.
pub const BG: Rgb = (0x18, 0x18, 0x18);
// Structural: pane borders (rule) and the selected-row fill (surface). Dark
// enough to sit under content without competing with it.
pub const BORDER: Rgb = (0x2c, 0x2c, 0x28);
// A warm lifted bar for the selected row, clearly brighter than the ground so
// the cursor reads at a glance.
pub const SEL_FILL: Rgb = (0x33, 0x2e, 0x24);
pub const FAINT: Rgb = (0x8f, 0x8a, 0x7c);
pub const TEXT: Rgb = (0xe8, 0xe3, 0xd3);
pub const WARN: Rgb = (0xc9, 0x84, 0x43);
// Health indicators: a settled green and an alarm red. Kept as semantics; the
// warm palette has no substitute that reads as healthy/stalled.
pub const GOOD: Rgb = (0x5f, 0xd0, 0x8a);
pub const BAD: Rgb = (0xf2, 0x60, 0x6e);

// Per-pool hues drawn from the warm accents (cyan/olive/gold/coral), so a pool
// reads the same everywhere and no longer leans blue.
pub const POOL_TRANSPARENT: Rgb = (0xa8, 0xd5, 0xd5);
pub const POOL_SAPLING: Rgb = (0xa0, 0xa3, 0x44);
pub const POOL_ORCHARD: Rgb = (0xd3, 0xa0, 0x50);
pub const POOL_IRONWOOD: Rgb = (0xc9, 0x8a, 0x72);

pub fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

/// Linear blend between two colors, `t` clamped to `0..=1`.
pub fn lerp(a: Rgb, b: Rgb, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Braille spinner frames. A syncing indicator animates by cycling these on the
/// frame clock, so the motion carries the "in progress" read without pulsing the
/// color.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The spinner frame for `secs` of elapsed time, advancing about 12 frames a
/// second.
pub fn spinner(secs: f32) -> char {
    SPINNER[(secs * 12.0) as usize % SPINNER.len()]
}

/// A smooth 0->1->0 pulse. `secs` is the animation clock, `period` the full cycle.
pub fn pulse(secs: f32, period: f32) -> f32 {
    let phase = (secs / period) * std::f32::consts::TAU;
    0.5 - 0.5 * phase.cos()
}

/// A pulse whose phase is offset per index, so a run of glyphs ripples like a
/// wave instead of blinking in unison.
pub fn wave(secs: f32, period: f32, index: usize) -> f32 {
    pulse(secs + index as f32 * 0.13, period)
}

/// Each character of `text` colored by a wave that drifts sideways over `e`
/// seconds of elapsed time.
pub fn shimmer(text: &str, e: f32) -> Vec<Span<'static>> {
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let c = lerp(ACCENT_DIM, ACCENT_HI, wave(e * 1.1, 1.6, i));
            Span::styled(
                ch.to_string(),
                Style::default().fg(c).add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}
