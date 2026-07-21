# darkside-serve

The darkside's server tier. One `darkside-chain` state machine behind both
variants' generated `CompactTxStreamer` server traits, emitted by a single
macro with an `extra_rpcs` block for the Crosslink additions (roster, bond
info, faucet), the same structure `lightwallet-test-support` uses.

Also here:

- the two drivers: live (wall-clock ticks, `--instamine`, `--withhold`) and
  scenario (barrier-driven, no clock anywhere). The live one reads its pause
  flag and cadence from a watch channel, so a control command retimes it
  mid-wait,
- the command vocabulary and its dispatcher: one `Command` enum, one
  `Dispatcher` over `Darkside`, so every frontend shares one brain and no
  command can exist in one and not another. Results and failures are values,
  never prints, which is what lets a caller reached over a socket see what a
  caller at a terminal sees,
- the HTTP control surface: axum routes over that dispatcher, one per
  command. Loopback only until there is a token to gate it,
- RPC-observable barriers: darkside can only see what the wallet asks
  for, so barriers reference requests and nothing else,
- a direct `lightwallet_core::IndexerClient` impl over darkside, so
  trait-generic consumers get a darkside target with no socket,
- in-process serving over a tokio duplex pipe, reusing the test-support
  pattern. TCP binding exists only in `darkside`.
