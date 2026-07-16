# lightwallet-cli (`lwcli`)

A one-shot gRPC client for Zcash lightwallet indexers. Every RPC on both
protocol variants is a typed subcommand calling the `lightwallet-core`
indexers, so the binary doubles as a dogfood of the crate's public API.

```
lwcli --url https://zec.rocks:443 get-latest-height
lwcli --url https://zec.rocks:443 get-block 2000000
lwcli --url https://zec.rocks:443 get-block-range 2000000 2000010   # NDJSON
lwcli --url https://testnet.zec.rocks:443 --transport tor get-lightd-info
lwcli --url http://127.0.0.1:9067 get-roster                        # implies --variant crosslink
```

Flags: `--variant` picks canonical (default) or crosslink, with
crosslink-only commands implying crosslink. `--transport` picks direct
(default), `tor` (in-process arti, bootstraps on first use), or `nym`
(through a running `nym-socks5-client`, address via `--nym-socks5`).
`--output` is `json` (default) or `debug`. `completions <shell>` prints a
completion script.

Output is JSON with every field emitted, defaults included, and `bytes`
fields as hex. Txids and block hashes cross the CLI boundary in display
order (as explorers show them), both as arguments and in output. Other byte
fields are wire-order hex. Streaming RPCs print NDJSON and survive a closed
pipe, so `lwcli ... | head` works mid-stream.

The `tor` and `nym` cargo features are default-on and cut the corresponding
`--transport` values when disabled (arti roughly doubles the build, so
`--no-default-features` gives a lean direct-only binary). Design decisions
are recorded in `cli-plan.md` at the workspace root.
