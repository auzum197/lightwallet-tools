# lwtui design notes

## Color tokens

Chrome (foreground/UI). Never ANSI 0-15.

| token       | rgb     | role                           |
|-------------|---------|--------------------------------|
| bg          | #181818 | app ground, painted every cell |
| surface     | #1f1f1d | content panes, flat            |
| ink         | #e8e3d3 | body text                      |
| dim         | #8f8a7c | labels, secondary              |
| rule        | #2c2c28 | borders                        |
| accent-cyan | #a8d5d5 | tags, section markers          |
| accent-gold | #d3a050 | values, active, focus          |
| orange      | #c98443 | live readouts                  |
| olive       | #a0a344 | code, literals                 |
| coral       | #c98a72 | warnings                       |

Gradient body, dither fields only. These values are the source hues already
dimmed 40% toward `bg`; the dim factor is the single brightness knob, held at
its default and applied at the palette level, not at runtime.

| token      | rgb     |
|------------|---------|
| tan-dim    | #624e34 |
| tan-faint  | #413625 |
| sand       | #8b6e52 |
| rust       | #7b4331 |
| grad-olive | #676638 |
| greige     | #776b58 |
| teal-deep  | #39554e |
| teal       | #435c54 |

## Dither gradient (`--experimental-dither`)

An idle animation for empty panes: the reserved tx column and the empty
mempool/blocks/results states. One animated surface per screen. Gradients are
empty-space material, never under text, lists, tables, or code.

`src/dither.rs` renders it, `App` owns the clock, `ui::empty_surface` places
it under the one-line hint.

### Value field

Coords normalized `0..1` over the pane, `t` the field clock in seconds. Three
blobs orbit on independent cosine/sine paths:

```
b1 = (0.50 + 0.30 cos(0.40 t),        0.42 + 0.26 sin(0.31 t))         w = 1.00
b2 = (0.50 + 0.34 cos(2.1 - 0.23 t),  0.55 + 0.30 sin(0.19 t + 1.0))   w = 0.85
b3 = (0.50 + 0.26 cos(0.17 t + 4.2),  0.50 + 0.34 sin(3.3 - 0.27 t))   w = 0.70

falloff(d²) = 1 / (1 + 4.5 d²)²        // rational stand-in for exp(-9 d²)
v  = Σ wᵢ · falloff(|p - bᵢ|²)
v *= 0.72 + 0.28 sin(6x - 4y + 1.3 t)  // diagonal wave
v  = clamp(v - 0.06, 0, 1)
```

Blob centers computed once per frame, never per cell. Field sampled once per
cell. No allocation in the render path.

### Color

Value drives glyph density, blob position drives hue. Per cell the hue is the
blob colors (`sand`←b1, `rust`←b2, `grad-olive`←b3) blended by the same
falloff weights, over a `tan-faint` floor where every blob is far, then washed
`greige` at the top and `teal-deep` at the bottom. A cell near b2 reads rust,
its shade coverage says how bright.

### Dither

Bayer 8×8, threshold `(M[y%8][x%8] + 0.5) / 64`. Shade ramp
`[' ','░','▒','▓','█']`. Two-color cell: `bg` = hue sunk halfway to ground,
`fg` = hue, glyph = `SHADES[floor(v·4 + bayer)]`, the shade interpolating
between the two stops. Every cell paints its own `bg`.

### Motion

`t` advances 0.5 units per second of active animation, quantized to a 90ms
step so the frame rate caps near 11fps and frozen frames diff to nothing. The
loop never repeats, it drifts.

Pauses (freezes on the last frame) on input focus (`/` search, `?` help),
while a tx is open, on manual pause, and after 10s idle.

### Degrade

Needs truecolor (`COLORTERM = truecolor` or `24bit`). Flag off, `NO_COLOR`, or
a 256-only terminal: flat `surface`, no gradient.
