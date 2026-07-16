# Embedded Nym for lwcli: a sketch, not a plan

NymVPN's mobile apps prove the embedding works: a Rust core carries the whole
mixnet client in-process, and the "external client" disappears into the app.
This sketch describes what the same move would mean for `lwcli`, so the
decision to wait is recorded next to what it is waiting for. It is the
in-process refinement nym-plan.md deferred, seen from the CLI's side.

## What it would mean

`--transport nym` works with nothing else running. The connector inside
`lightwallet-transport-nym` (or a sibling constructor) builds a mixnet client
in-process, opens a reliable byte stream through the mixnet, and hands tonic
the same plain `Channel` it hands today. The `GrpcTransport` seam, the flag
surface, and everything downstream of `connect()` are untouched. The
`--nym-socks5` flag would remain as an escape hatch for operators who already
run the external client.

Reaching a clearnet lightwalletd still involves a network requester: the
embedded client dials one over the mixnet and speaks the egress protocol to
it, which is the work `nym-socks5-client` does today, relocated into the
process. Only a Nym-native lightwalletd would remove that hop, and none
exists.

## What mobile pays that a one-shot CLI feels harder

- Dependencies. The reliable-stream layer (`stream` in the SDK) is git-only.
  NymVPN pins its own monorepo, which is fine for them and unacceptable here.
  Nothing moves before that layer is published on crates.io.
- Startup. A VPN registers with a gateway once and keeps the connection warm
  for days. `lwcli` exits after one RPC, so every invocation would pay
  gateway connection and registration, seconds each time. The mitigation is
  the arti pattern already used for Tor: persistent client state in the
  platform state dir, announced on stderr, excluded from `--timeout`.
- State and credentials. Embedding moves gateway keys, and zk-nym bandwidth
  credentials where the network demands them, into the CLI's care. Key
  storage can follow the arti precedent. Credential acquisition should not:
  funding and topping up credentials is a product concern (NymVPN sells it),
  and the most a debug tool should do is fail with a message that says what
  the operator must provide.

## What would unpark it

The `stream` module (or an equivalent published client library covering the
network-requester egress protocol) landing on crates.io with some stability
promise. That is the same release nym-plan.md's deferred direct-stream path
waits for, so one upstream event unparks both. When it happens, the work is:
an embedded constructor in the transport crate, a state-dir and bootstrap-UX
decision mirroring the Tor answers in cli-plan.md, and a credentials-are-not-
our-problem boundary written down before the first line of code.

## What does not change

The promotion gate. An embedded client rides the same mixnet with the same
latency, so it inherits nym's experimental marking until the live sync rate
clears the §3.6 bar, however the bytes get into the mixnet.
