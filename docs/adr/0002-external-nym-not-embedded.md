# Nym transport stays external, not embedded

We considered embedding a Nym mixnet client in the binary (nym-sdk's
`Socks5MixnetClient`) so `--transport nym` would need no separately-run
`nym-socks5-client`, mirroring how the Tor transport embeds arti. It cannot be
done. nym-sdk and arti both link the sqlite native library (`links =
"sqlite3"`) at incompatible versions, and cargo forbids two packages linking the
same native library in one resolve, so a single binary cannot embed both Tor and
Nym. Nym stays external: `lightwallet-transport-nym` is a SOCKS5 connector to an
operator-run `nym-socks5-client`.

The collision:

- arti → tor-dirmgr → rusqlite → `libsqlite3-sys 0.34`
- nym-sdk → `nym-bandwidth-fetcher` and `nym-credential-storage` → sqlx-sqlite →
  `libsqlite3-sys 0.30`

Both declare `links = "sqlite3"`. The constraint bites at resolution, not build:
one `Cargo.lock` must be valid for every feature combination, so even declaring
the two as mutually-exclusive optional dependencies fails to resolve. A build
with `tor` off and only the embed feature on still fails. Neither side exposes a
knob to drop sqlite: nym-sdk has no feature gates, `nym-credential-storage`
pulls sqlx+sqlite unconditionally on non-wasm targets, and arti pulls rusqlite
through tor-dirmgr even with `default-features = false`.

## Considered Options

- **Embed nym-sdk as the default nym realization** (the original plan). Rejected:
  it cannot coexist with the embedded arti Tor transport, per the conflict above.
- **Depend on the lighter `nym-socks5-client-core` instead of nym-sdk.** Rejected:
  it pulls sqlite through `nym-credential-storage` one hop away, the same
  conflict.
- **Two mutually-exclusive binaries**, an arti-flavored `lwcli` and an
  nym-sdk-flavored `lwcli-nym` over a shared `cli-core` lib, in separate
  workspaces. Rejected: a debug CLI uses one transport per invocation and never
  needs both embedded at once. A binary split, a second workspace, and a second
  artifact to distribute is more structure than the value, and the external path
  already delivers single-binary Nym behind one operator step.
- **Drop the embedded arti Tor transport and make Tor external too, then embed
  Nym.** Rejected: it discards a working embedding to move the same friction onto
  Tor.

## Consequences

`--transport nym` dials a running `nym-socks5-client`
([nymtech/nym](https://github.com/nymtech/nym), `clients/socks5`) at
`--nym-socks5` (default `127.0.0.1:1080`). Running and funding it is operator
setup, as `lightwallet-transport-nym`'s docs already state.

If a future consumer, a wallet, genuinely needs both Tor and Nym embedded and
runtime-selectable in one artifact, that is an arti-vs-nym-sdk problem for the
ecosystem, best raised upstream with nym (the unconditional sqlite in
`nym-credential-storage` is the blocker), not worked around here. The
gateway-selection and socks5-embeddability research under `scratchpad/` captured
the nym-sdk 1.21.4 API and remain the reference if embedding is revisited.
