# lwtui design notes

## Dither gradient (`--experimental-dither`)

An **idle animation** for empty panes: the reserved tx column and the
empty mempool/blocks/results states. One animated surface per screen.
Ported from `dither-gradient.html`; behavior, not DOM.

`src/dither.rs` renders it, `App` owns the clock, `ui::empty_surface`
places it under the one-line hint.

### Field

Three blobs **orbit** on independent cosine/sine paths. Their weighted
falloffs sum to a value `v ∈ 0..1`, modulated by a diagonal wave. `v`
drives glyph density, blob position drives hue: near blob two a cell
reads rust, the shade coverage says how bright.

- `falloff(d²) = 1/(1 + 4.5·d²)²` — no `exp`, no transcendental per blob.
- Blob centers computed once per frame, never per cell. Field sampled
  once per cell. No allocation in the render path.

### Dither

Bayer 8×8 threshold, shade ramp `[' ','░','▒','▓','█']`. Two-color cell:
`bg` is the hue sunk halfway to ground, `fg` the hue, the glyph
**interpolates** between them. Every cell paints its own `bg`.

### Motion

`t` advances 0.5 units per second of active animation, quantized to a
90ms step so the **frame rate** caps at ~11fps and frozen frames diff to
nothing. The **loop** never repeats; it drifts.

Pauses (freezes on the last frame) on input focus (`/` search, `?`
help), while a tx is open, on manual pause, and after 10s idle.

### Degrade

Needs truecolor (`COLORTERM=truecolor|24bit`). Flag off, `NO_COLOR`, or a
256-only terminal: flat pane, no gradient. A 256-color Oklab-quantized
ramp is deferred (`.wayfinder/tickets/13-oklab-256-quantization.md`).

Palette values are the gradient body pre-dimmed 40% toward `bg`; the dim
factor is the single brightness knob, held at its default.
