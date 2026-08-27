//! The about pane's centerpiece: a shaded sphere under an orbiting light,
//! drawn on the classic ASCII luminance ramp. The sphere itself is smooth, so
//! the motion comes from the light circling it, sweeping the bright face
//! around and leaving a dark limb behind.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::theme::{self, color};

/// Luminance ramp, dark to bright: the classic 15-level ASCII grayscale
/// ramp, chosen because its ink density climbs monotonically, so no level
/// reads darker than the one below it. Index 0 (space) never lands inside
/// the disc: the floor is `.` so the silhouette holds against the sea.
const RAMP: [char; 15] = [
    ' ', '.', ',', ':', ';', 'i', '1', 't', 'f', 'L', 'C', 'G', '0', '8', '@',
];

/// A warm white the hotspot leans toward, gently.
const GLARE: theme::Rgb = theme::rgb(0xf6, 0xf1, 0xe4);

/// The color ramp: the base gold sweep, with only a slight whitening at the
/// very top so the hotspot pops a step or two without leaving the accent.
fn shade(lum: f32) -> ratatui::style::Color {
    let base = theme::Rgb {
        r: toward(theme::ACCENT_DIM.r, theme::ACCENT_HI.r, lum),
        g: toward(theme::ACCENT_DIM.g, theme::ACCENT_HI.g, lum),
        b: toward(theme::ACCENT_DIM.b, theme::ACCENT_HI.b, lum),
    };
    let lift = ((lum - 0.85) / 0.15).clamp(0.0, 1.0) * 0.2;
    Color::Rgb(
        toward(base.r, GLARE.r, lift),
        toward(base.g, GLARE.g, lift),
        toward(base.b, GLARE.b, lift),
    )
}

fn norm3(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let len = (x * x + y * y + z * z).sqrt();
    (x / len, y / len, z / len)
}

/// The disc's placement in its area plus the orbiting light at time `t`.
struct Geom {
    cx0: f32,
    cy0: f32,
    rx: f32,
    ry: f32,
    lx: f32,
    ly: f32,
    lz: f32,
}

/// `None` when the area is too small to hold a disc. The light orbits the
/// vertical axis once every ~7s, held a little above the equator. In front
/// (+z) it fills the face; behind, only the ambient floor and a grazing rim
/// survive, which is what makes the orbit read.
fn geom(area: Rect, t: f32) -> Option<Geom> {
    let ry = (area.height as f32 / 2.0).min(area.width as f32 / 4.0);
    if ry < 1.5 {
        return None;
    }
    let th = 0.9 * t;
    let (lx, ly, lz) = norm3(th.sin(), -0.45, th.cos());
    Some(Geom {
        cx0: area.x as f32 + area.width as f32 / 2.0,
        cy0: area.y as f32 + area.height as f32 / 2.0,
        rx: ry * 2.0,
        ry,
        lx,
        ly,
        lz,
    })
}

/// Surface luminance at normalized disc coords, `None` outside the disc.
/// Half-Lambert diffuse plus a tight specular pop at the hotspot. The
/// half-Lambert term is monotonic in the angle to the light over the whole
/// sphere, so shading grades across the face along the light direction and
/// the darkest point sits antipodal to the hotspot, never parked mid-disc.
fn lum_at(g: &Geom, nx: f32, ny: f32) -> Option<f32> {
    let d2 = nx * nx + ny * ny;
    if d2 > 1.0 {
        return None;
    }
    let nz = (1.0 - d2).sqrt();
    let dot = nx * g.lx + ny * g.ly + nz * g.lz;
    let half = (dot + 1.0) / 2.0;
    let spec = dot.max(0.0).powi(8);
    Some((0.02 + 0.88 * half * half + 0.35 * spec).clamp(0.0, 1.0))
}

/// Paint the sphere centered in `area` at animation time `t` (seconds of
/// active animation). Cells are about twice as tall as wide, so the x radius
/// doubles to draw a round disc. Cells outside the disc are left untouched,
/// letting the sea show through around it.
pub fn render(buf: &mut Buffer, area: Rect, t: f32) {
    let Some(g) = geom(area, t) else { return };
    for cy in area.top()..area.bottom() {
        for cx in area.left()..area.right() {
            let nx = (cx as f32 + 0.5 - g.cx0) / g.rx;
            let ny = (cy as f32 + 0.5 - g.cy0) / g.ry;
            let Some(lum) = lum_at(&g, nx, ny) else {
                continue;
            };
            let idx = ((1.0 + lum * 13.0).round() as usize).clamp(1, 14);
            if let Some(cell) = buf.cell_mut((cx, cy)) {
                cell.set_char(RAMP[idx]);
                cell.set_style(Style::default().fg(shade(lum)).bg(color(theme::BG)));
            }
        }
    }
}

fn toward(c: u8, target: u8, a: f32) -> u8 {
    (c as f32 + (target as f32 - c as f32) * a).round() as u8
}

/// The sphere's shadow on the water: each sea cell samples the sphere at its
/// mirror image across the waterline (the sea's top edge) and leans the sea's
/// foreground toward the sphere's gold by that luminance, fading with depth.
/// Glyphs stay the sea's own, so the water keeps its texture under the tint.
pub fn shadow(buf: &mut Buffer, sea: Rect, sky: Rect, t: f32) {
    let Some(g) = geom(sky, t) else { return };
    if sea.height == 0 {
        return;
    }
    let waterline = sea.y as f32;
    for cy in sea.top()..sea.bottom() {
        let depth = cy as f32 + 0.5 - waterline;
        let fade = 0.6 * (1.0 - depth / sea.height as f32);
        for cx in sea.left()..sea.right() {
            let nx = (cx as f32 + 0.5 - g.cx0) / g.rx;
            let ny = (2.0 * waterline - (cy as f32 + 0.5) - g.cy0) / g.ry;
            let Some(lum) = lum_at(&g, nx, ny) else {
                continue;
            };
            let a = fade * (0.3 + 0.7 * lum);
            if let Some(cell) = buf.cell_mut((cx, cy))
                && let Color::Rgb(r, gr, b) = cell.fg
            {
                cell.set_fg(Color::Rgb(
                    toward(r, toward(theme::ACCENT_DIM.r, theme::ACCENT_HI.r, lum), a),
                    toward(gr, toward(theme::ACCENT_DIM.g, theme::ACCENT_HI.g, lum), a),
                    toward(b, toward(theme::ACCENT_DIM.b, theme::ACCENT_HI.b, lum), a),
                ));
            }
        }
    }
}

/// Spark glyphs by depth, far to near. Depth also scales brightness.
const SPARKS: [char; 3] = ['.', '+', '*'];

/// A cheap unit-interval hash.
fn hash01(seed: u32) -> f32 {
    let mut x = seed.wrapping_mul(0x9e37_79b9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Sparks in orbit around the sphere: each slot loops unhurried (1.8-3.5s),
/// popping in at full brightness at a fresh spot every cycle and fading to
/// the ground. Positions land in an annulus just off the disc's limb. A hash
/// of the slot and its cycle index drives angle, distance and depth. Only
/// blank cells take a spark.
pub fn particles(buf: &mut Buffer, sky: Rect, t: f32) {
    let Some(g) = geom(sky, t) else { return };
    let slots = (sky.width as u32 * sky.height as u32 / 60).clamp(3, 8);
    for i in 0..slots {
        let period = 1.8 + 1.7 * hash01(i.wrapping_mul(31) ^ 0xa11ce);
        let phase = period * hash01(i.wrapping_mul(101) ^ 0x5eed);
        let cycles = (t + phase) / period;
        let cycle = cycles as u32;
        let life = cycles.fract();
        let seed = i.wrapping_mul(0x01f1) ^ cycle.wrapping_mul(0x9d2c);
        let ang = hash01(seed ^ 1) * std::f32::consts::TAU;
        let orbit = 1.15 + 0.45 * hash01(seed ^ 2);
        let x = (g.cx0 + ang.cos() * orbit * g.rx).round();
        let y = (g.cy0 + ang.sin() * orbit * g.ry).round();
        if x < sky.left() as f32
            || x >= sky.right() as f32
            || y < sky.top() as f32
            || y >= sky.bottom() as f32
        {
            continue;
        }
        let (x, y) = (x as u16, y as u16);
        let depth = hash01(seed ^ 3);
        let fade = (1.0 - life) * (1.0 - life);
        let glow = fade * (0.35 + 0.65 * depth);
        if let Some(cell) = buf.cell_mut((x, y))
            && cell.symbol() == " "
        {
            let spark = theme::Rgb {
                r: toward(theme::ACCENT_DIM.r, theme::ACCENT_HI.r, depth),
                g: toward(theme::ACCENT_DIM.g, theme::ACCENT_HI.g, depth),
                b: toward(theme::ACCENT_DIM.b, theme::ACCENT_HI.b, depth),
            };
            cell.set_char(SPARKS[((depth * 3.0) as usize).min(2)]);
            cell.set_fg(Color::Rgb(
                toward(theme::BG.r, spark.r, glow),
                toward(theme::BG.g, spark.g, glow),
                toward(theme::BG.b, spark.b, glow),
            ));
            cell.set_bg(color(theme::BG));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: f32) -> Buffer {
        let area = Rect::new(0, 0, 32, 14);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, t);
        buf
    }

    #[test]
    fn the_dark_side_still_shades_when_the_light_is_behind() {
        // t chosen so the orbit puts the light straight behind (θ = π).
        let buf = frame(std::f32::consts::PI / 0.9);
        let mut glyphs = std::collections::HashSet::new();
        for y in 0..14u16 {
            for x in 0..32u16 {
                let ch = buf.cell((x, y)).unwrap().symbol().chars().next().unwrap();
                if ch != ' ' {
                    glyphs.insert(ch);
                }
            }
        }
        assert!(
            glyphs.len() >= 3,
            "the unlit face steps through shades, got {glyphs:?}"
        );
    }

    #[test]
    fn the_shadow_tints_the_sea_without_touching_its_glyphs() {
        let sky = Rect::new(0, 0, 32, 14);
        let sea = Rect::new(0, 14, 32, 5);
        let mut buf = Buffer::empty(Rect::new(0, 0, 32, 19));
        let water = Color::Rgb(0x39, 0x55, 0x4e);
        for y in sea.top()..sea.bottom() {
            for x in sea.left()..sea.right() {
                let cell = buf.cell_mut((x, y)).unwrap();
                cell.set_char('░');
                cell.set_fg(water);
            }
        }
        shadow(&mut buf, sea, sky, 0.0);
        let mut tinted = 0;
        for y in sea.top()..sea.bottom() {
            for x in sea.left()..sea.right() {
                let cell = buf.cell((x, y)).unwrap();
                assert_eq!(cell.symbol(), "░", "the water keeps its texture");
                if cell.fg != water {
                    tinted += 1;
                }
            }
        }
        assert!(
            tinted > 10,
            "the reflection recolors a real area, got {tinted}"
        );
        // Far corners sit outside the mirrored disc and keep the sea's hue.
        assert_eq!(buf.cell((0, 18)).unwrap().fg, water);
        assert_eq!(buf.cell((31, 18)).unwrap().fg, water);
    }

    #[test]
    fn the_disc_stays_on_the_ramp_and_outside_stays_untouched() {
        let buf = frame(3.0);
        let mut inside = 0;
        for y in 0..14u16 {
            for x in 0..32u16 {
                let ch = buf.cell((x, y)).unwrap().symbol().chars().next().unwrap();
                assert!(RAMP.contains(&ch), "glyph {ch:?} is off the ramp");
                if ch != ' ' {
                    inside += 1;
                }
            }
        }
        assert!(inside > 50, "the disc covers a real area, got {inside}");
        // Corners sit outside the disc and keep their empty cells.
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((31, 13)).unwrap().symbol(), " ");
    }

    #[test]
    fn the_same_t_reproduces_the_frame_and_the_light_moves_between_ts() {
        assert_eq!(frame(2.0), frame(2.0));
        assert_ne!(frame(0.0), frame(2.0), "the orbiting light changes shading");
    }

    #[test]
    fn particles_dust_blank_sky_and_leave_the_disc_alone() {
        let area = Rect::new(0, 0, 32, 14);
        let mut sparks = 0;
        for step in 0..8 {
            let t = step as f32 * 0.7;
            let mut buf = Buffer::empty(area);
            render(&mut buf, area, t);
            let disc = buf.clone();
            particles(&mut buf, area, t);
            for y in 0..14u16 {
                for x in 0..32u16 {
                    let before = disc.cell((x, y)).unwrap().symbol();
                    let after = buf.cell((x, y)).unwrap().symbol();
                    if before != " " {
                        assert_eq!(before, after, "the disc keeps its shading");
                    } else if after != " " {
                        assert!(SPARKS.contains(&after.chars().next().unwrap()));
                        sparks += 1;
                    }
                }
            }
            let mut again = disc.clone();
            particles(&mut again, area, t);
            assert_eq!(buf, again);
        }
        assert!(sparks > 0, "some sparks land in the open sky");
    }

    #[test]
    fn a_sliver_area_is_a_no_op() {
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, 1.0);
        assert_eq!(buf, Buffer::empty(area));
    }
}
