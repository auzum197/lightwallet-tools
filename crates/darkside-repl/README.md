# darkside-repl

An interactive console for a running darkside. It owns no chain and serves no
RPC: every line typed here becomes the same HTTP request a script would send,
so nothing can work at this prompt and fail over the wire.

Start a server, then drive it:

```
darkside serve
darkside-repl
```

It connects to `http://127.0.0.1:9068` by default, the address `darkside serve`
binds its control surface to. `--control <url>` points it elsewhere. An
unreachable server is an error at startup rather than a mystery on the first
command.

## Commands

```
mine [N]                  mine N blocks now (default 1)
to <height>               mine forward until the tip reaches <height>
next                      mine forward to the next network upgrade
advance <upgrade>         mine forward to a named upgrade's activation
fund <addr> <zec> [tsoi]  fabricate value to any address
pause | resume            stop or start auto-mining
tick <seconds|low..high>  set the auto-mining cadence
reorg <depth>             fork <depth> blocks back and serve the fork
reset                     rebuild the boot chain and serve it
withhold on|off           accept submissions but hold them out of blocks
status                    tip, pools, next upgrade, mempool, miner state
help                      command list
quit                      leave (Ctrl-D also works)
```

`advance` names the upgrade to stop at, where `next` takes whichever comes
first. The names are `overwinter`, `sapling`, `blossom`, `heartwood`,
`canopy`, `nu5`, `nu6`, `nu6.1`, `nu6.2`, and `ironwood`, which also answers to
`nu6.3` so the spelling `--activation-heights` uses works here too.

```
darkside> advance ironwood
mined to ironwood activation at 500
darkside> advance ironwood
ironwood activated at 500 and the tip is already 500
```

Advancing to an upgrade the chain never schedules, or one already passed,
fails rather than quietly doing nothing.

A target further away than `--max-blocks` is **jumped** rather than mined. On
mainnet, ironwood sits 1.7M blocks above the NU5 boot height, and storing a
block per height would cost about an hour and several gigabytes for blocks
carrying nothing. A jump mines the target block alone and leaves the heights
beneath it unmined, computed on request the way prehistory is (ADR 0002).

```
darkside> fund u1abc… 10
funded u1abc… at height 1687105: 10 ZEC o, tip now 1687105
darkside> advance ironwood
jumped to ironwood activation at 3428143
darkside> fund u1abc… 10 i
funded u1abc… at height 3428144: 10 ZEC i, tip now 3428144
```

Nothing below the skipped span moves. Blocks keep their heights, their hashes,
and their transactions, so the fund at 1,687,105 is still there and a wallet
already syncing sees new empty blocks rather than a rewrite. A jump is refused
only when work is scheduled inside the span it would skip, since those events
would never fire.

A target inside `--max-blocks` is mined normally.

`fund` takes any address valid for the served network. Nothing needs declaring
first: open a wallet, copy the address it gives you, and fund it.

The trailing letters pick which receivers of that address get paid, `t`
transparent, `s` sapling, `o` orchard, `i` ironwood. Several letters split the
amount into equal parts, with the remainder going to the letter typed first.
Omit them and the whole amount goes to the newest receiver the address carries
and the chain has active.

```
darkside> fund u1abc… 12 tsoi
funded u1abc… at height 2: 3 ZEC t, 3 ZEC s, 3 ZEC o, 3 ZEC i, tip now 2

darkside> fund u1abc… 12 osi
warning: i is not active at height 1687105, skipped
funded u1abc… at height 1687105: 6 ZEC o, 6 ZEC s, tip now 1687105
```

A receiver the address does not carry, or a pool the chain has not activated,
is skipped and its share re-split over the rest, so the amount asked for is
the amount that lands. A fund where no requested receiver survives fails
instead.
