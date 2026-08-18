//! Colors and the shimmer math: a cosine pulse whose phase drifts per glyph, so
//! the bright point travels across the title string.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub type Rgb = (u8, u8, u8);

// Accent (brand) — teal. The title shimmer sweeps DIM -> HI.
pub const ACCENT_DIM: Rgb = (0x0f, 0x4c, 0x47);
pub const ACCENT_HI: Rgb = (0x9c, 0xf6, 0xe9);

pub const BG: Rgb = (0x0d, 0x11, 0x17);
pub const FAINT: Rgb = (0x55, 0x60, 0x6b);
pub const TEXT: Rgb = (0xc8, 0xd2, 0xdc);
pub const WARN: Rgb = (0xf2, 0xb0, 0x5e);
// Health indicators: a settled green and an alarm red.
pub const GOOD: Rgb = (0x5f, 0xd0, 0x8a);
pub const BAD: Rgb = (0xf2, 0x60, 0x6e);

// Per-pool hues, so a pool reads the same everywhere it appears.
pub const POOL_TRANSPARENT: Rgb = (0x7f, 0xb0, 0xff);
pub const POOL_SAPLING: Rgb = (0x5f, 0xd0, 0x8a);
pub const POOL_ORCHARD: Rgb = (0xf2, 0xb0, 0x5e);
pub const POOL_IRONWOOD: Rgb = (0xc7, 0x92, 0xea);

pub fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

/// Linear blend between two colors, `t` clamped to `0..=1`.
pub fn lerp(a: Rgb, b: Rgb, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
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
