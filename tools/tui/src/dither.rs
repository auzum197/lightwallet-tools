//! An ordered-dither gradient for empty panes: three orbiting blobs summed into
//! a value field, thresholded against an 8x8 Bayer matrix into shade glyphs. The
//! field and its colors are a direct port of the `dither-gradient.html` hero (see
//! `DESIGN.md`). Value drives glyph density, position drives hue: a cell near
//! blob two reads rust, the dot coverage there says how bright.
//!
//! No allocation in the render path. Blob centers are computed once per call,
//! never per cell, and the field is sampled once per cell.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Shade ramp, empty to full. `floor(v*4 + bayer)` indexes it, so inter-cell
/// dithering carries the levels between the five glyphs.
const SHADES: [char; 5] = [' ', '░', '▒', '▓', '█'];

/// The ground the region paints on every cell, so it never shows the terminal
/// default through the gradient.
const BG: (u8, u8, u8) = (0x18, 0x18, 0x18);
/// The gradient body, already dimmed 40% toward `BG` at the palette level (the
/// single brightness knob, held at its default). Used directly, as the HTML does.
const TAN_FAINT: (u8, u8, u8) = (0x41, 0x36, 0x25);
const SAND: (u8, u8, u8) = (0x8b, 0x6e, 0x52);
const RUST: (u8, u8, u8) = (0x7b, 0x43, 0x31);
const GRAD_OLIVE: (u8, u8, u8) = (0x67, 0x66, 0x38);
const GREIGE: (u8, u8, u8) = (0x77, 0x6b, 0x58);
const TEAL_DEEP: (u8, u8, u8) = (0x39, 0x55, 0x4e);

/// Standard 8x8 ordered-dither threshold matrix, values 0..63.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// The three blob centers at field time `t`, in normalized coords.
struct Blobs {
    b1: (f32, f32),
    b2: (f32, f32),
    b3: (f32, f32),
}

/// Blob centers orbit on independent cosine/sine paths. Computed once per frame.
fn blobs(t: f32) -> Blobs {
    Blobs {
        b1: (
            0.50 + 0.30 * (0.40 * t).cos(),
            0.42 + 0.26 * (0.31 * t).sin(),
        ),
        b2: (
            0.50 + 0.34 * (2.1 - 0.23 * t).cos(),
            0.55 + 0.30 * (0.19 * t + 1.0).sin(),
        ),
        b3: (
            0.50 + 0.26 * (0.17 * t + 4.2).cos(),
            0.50 + 0.34 * (3.3 - 0.27 * t).sin(),
        ),
    }
}

/// Rational stand-in for `exp(-9·d²)`: no transcendental per blob per cell.
fn falloff(dx: f32, dy: f32) -> f32 {
    let q = 1.0 + 4.5 * (dx * dx + dy * dy);
    1.0 / (q * q)
}

fn mix(a: (f32, f32, f32), b: (u8, u8, u8), t: f32) -> (f32, f32, f32) {
    (
        a.0 + (b.0 as f32 - a.0) * t,
        a.1 + (b.1 as f32 - a.1) * t,
        a.2 + (b.2 as f32 - a.2) * t,
    )
}

/// Paint the dither field over `area` at field time `t`, one glyph per cell.
pub fn render(buf: &mut Buffer, area: Rect, t: f32) {
    let (w, h) = (area.width, area.height);
    if w == 0 || h == 0 {
        return;
    }
    let Blobs { b1, b2, b3 } = blobs(t);
    let (wf, hf) = (w as f32, h as f32);

    for cy in 0..h {
        let y = (cy as f32 + 0.5) / hf;
        for cx in 0..w {
            let x = (cx as f32 + 0.5) / wf;

            let f1 = 1.00 * falloff(x - b1.0, y - b1.1);
            let f2 = 0.85 * falloff(x - b2.0, y - b2.1);
            let f3 = 0.70 * falloff(x - b3.0, y - b3.1);
            let mut v = f1 + f2 + f3;
            v *= 0.72 + 0.28 * (6.0 * x - 4.0 * y + 1.3 * t).sin();
            v = (v - 0.06).clamp(0.0, 1.0);

            // Hue is the blob colors blended by the same falloff weights, over a
            // tan-faint floor where every blob is far, then washed greige at the
            // top and teal at the bottom.
            let wsum = f1 + f2 + f3;
            let hue = if wsum > 1e-4 {
                let inv = 1.0 / wsum;
                (
                    (SAND.0 as f32 * f1 + RUST.0 as f32 * f2 + GRAD_OLIVE.0 as f32 * f3) * inv,
                    (SAND.1 as f32 * f1 + RUST.1 as f32 * f2 + GRAD_OLIVE.1 as f32 * f3) * inv,
                    (SAND.2 as f32 * f1 + RUST.2 as f32 * f2 + GRAD_OLIVE.2 as f32 * f3) * inv,
                )
            } else {
                (TAN_FAINT.0 as f32, TAN_FAINT.1 as f32, TAN_FAINT.2 as f32)
            };
            let hue = mix(hue, GREIGE, 0.22 * (1.0 - y));
            let hue = mix(hue, TEAL_DEEP, 0.18 * y);

            // Two-color cell: bg is the hue sunk halfway to ground, fg the hue
            // itself; the shade glyph blends between them, dithered by Bayer.
            let bg = mix((BG.0 as f32, BG.1 as f32, BG.2 as f32), rgb(hue), 0.5);
            let bayer = (BAYER[(cy % 8) as usize][(cx % 8) as usize] as f32 + 0.5) / 64.0;
            let level = (v * 4.0 + bayer).floor().clamp(0.0, 4.0) as usize;

            if let Some(cell) = buf.cell_mut((area.x + cx, area.y + cy)) {
                cell.set_bg(to_color(bg));
                cell.set_fg(to_color(hue));
                cell.set_char(SHADES[level]);
            }
        }
    }
}

fn rgb(c: (f32, f32, f32)) -> (u8, u8, u8) {
    (c.0 as u8, c.1 as u8, c.2 as u8)
}

fn to_color(c: (f32, f32, f32)) -> Color {
    Color::Rgb(
        c.0.round().clamp(0.0, 255.0) as u8,
        c.1.round().clamp(0.0, 255.0) as u8,
        c.2.round().clamp(0.0, 255.0) as u8,
    )
}

/// Whether the terminal advertises truecolor and color is not suppressed. The
/// gradient needs 24-bit output; a 256-only terminal or `NO_COLOR` paints flat.
pub fn supported() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cell_gets_a_shade_glyph_and_an_rgb_pair() {
        let area = Rect::new(2, 1, 20, 8);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
        render(&mut buf, area, 3.5);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = buf.cell((x, y)).unwrap();
                let ch = cell.symbol().chars().next().unwrap();
                assert!(SHADES.contains(&ch), "glyph {ch:?} is off the ramp");
                assert!(matches!(cell.bg, Color::Rgb(..)));
                assert!(matches!(cell.fg, Color::Rgb(..)));
            }
        }
    }

    #[test]
    fn a_zero_area_is_a_no_op() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        render(&mut buf, Rect::new(0, 0, 0, 0), 1.0);
    }

    #[test]
    fn the_field_is_bounded_and_stable() {
        // The clamp keeps v in range and the same t reproduces the same frame,
        // so a frozen clock diffs to nothing.
        let mut a = Buffer::empty(Rect::new(0, 0, 24, 10));
        let mut b = Buffer::empty(Rect::new(0, 0, 24, 10));
        let area = Rect::new(0, 0, 24, 10);
        render(&mut a, area, 7.0);
        render(&mut b, area, 7.0);
        assert_eq!(a, b);
    }
}
