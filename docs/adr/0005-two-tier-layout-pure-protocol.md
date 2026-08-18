# Two-tier workspace: pure protocol layer, separate tools

The workspace splits in two. `crates/` is the protocol layer, the library other
consumers build on: `core`, the generated proto crates, the transports,
`nym-embed`, `ffi`, and `test-support`. `tools/` is what this repo is named for,
the binaries and their shared support: `lwcli`, `lwtui`, and `txview` (the
shared transaction projection). The invariant is one-directional: **`crates/*`
must never depend on `tools/*`**, so the protocol layer stays pure and a
consumer that vendors it inherits none of the tools' weight, above all not
`zcash_primitives` (orchard/halo2), which transaction parsing drags in.

The interpreting work `lwcli` and `lwtui` share (a serialized transaction into
txid, pools, value, fee) lives in `tools/txview`, not `core`. It is genuinely
shared, so it wants a crate; it is not pure, so that crate sits on the tools
side. Putting it in `core` would push orchard onto every downstream consumer of
the protocol layer for a concern none of them asked for.

## Considered Options

- **Keep one flat `crates/` for everything.** The prior layout: libraries and
  binaries side by side. Rejected because it left the one boundary that carries
  a real guarantee, pure versus not-pure, invisible and unenforced. The shared
  projection crate would have had no obvious home, and `core` was the tempting
  wrong one.
- **Three tiers (`crates/`, `support/`, `tools/`).** A middle tier for shared
  app libraries. Rejected as a distinction without a difference: `txview` and
  the binaries are all free to pull `zcash_primitives`/`ratatui`/`clap`, so they
  share one purity level. A `support/` tier would imply a third level that does
  not exist.

## Consequences

`crates/*` and `tools/*` are both workspace member globs; the binaries' path
dependencies reach back with `../../crates/...`. Package names are unchanged, so
`lightwallet-cli`/`lightwallet-tui` and every `-p` reference (the `feature-check`
recipe included) keep working across the move.

The invariant is upheld by convention and review, not a build gate: the folder
split makes it legible, and the tempting violation, putting shared parsing in
`core`, is exactly what `tools/txview` exists to prevent. It can be spot-checked
with `cargo tree -e normal -p lightwallet-core`, whose output should carry no
`zcash_primitives`/`zcash_protocol`/`ratatui`/`crossterm`. A one-directional
folder dependency is the kind of rule a reviewer can hold in their head.
