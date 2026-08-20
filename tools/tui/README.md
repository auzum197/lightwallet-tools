# lightwallet-tui (`lwtui`)

A live terminal monitor and read-only block explorer for Zcash lightwallet
indexers. It holds one connection open and tails the chain: a mempool view that
repopulates each block, a block-explorer tail with tx drill-down, and `/`
search over heights, txids, and transparent addresses. The interactive sibling
to `lwcli`, on the same `lightwallet-core` calls.

```
lwtui --url https://zec.rocks:443
lwtui --url http://127.0.0.1:9067 --variant crosslink
lwtui --url https://zec.rocks:443 --output ndjson        # machine-readable feed
lwtui --url https://zec.rocks:443 --experimental-dither-braille
```

## Keys

Tab switches mempool ⇄ blocks. `j`/`k` (or `↑`/`↓`) move the selection; Enter
opens a detail (a block, then a tx within it; a mempool tx directly), Esc or
`←` steps back. In a focused tx view the arrows scroll and `n`/`N` walk between
txs (a block's list or a search result set). `r` toggles raw ⇄ human, `y`/`Y`
copy the tx JSON or raw hex, `/` searches, Space pauses live-follow, `?` shows
the full key list, `q` quits.

## Flags

`--variant` picks canonical (default) or crosslink. `--target-spacing`
overrides the health heuristic's reference block time (default suits canonical
main/test; pass a featurenet's own spacing). `--output` is `tui` (default) or
`ndjson`, a one-event-per-line feed on stdout with block-boundary markers, for
piping.

### Dither gradient

An idle animation can back the empty tail of the focused pane. Prefer
`--experimental-dither-braille` for the finer look (2×4 braille dots per cell);
fall back to `--experimental-dither` (universal shade blocks) where a terminal
can't render braille glyphs. The braille flag wins when both are passed. Both
need a truecolor terminal (`COLORTERM=truecolor`) and degrade to a flat pane
otherwise or under `NO_COLOR`. [DESIGN.md](DESIGN.md) covers the field math,
color tokens, and motion.
