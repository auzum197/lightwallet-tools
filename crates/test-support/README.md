# lightwallet-test-support

In-memory mock indexer endpoints for both protocol variants, plus a SOCKS5
test server. A dev-dependency for the workspace's offline suite.

Each variant module (`canonical`, `crosslink`) provides a `MockStreamer`
implementing that variant's generated `CompactTxStreamer` server trait, and
a `serve` function that runs it over a tokio duplex pipe and hands back a
connected `Channel`. Real prost encode/decode, real HTTP/2, real gRPC status
mapping, no ports and no child processes.

```rust
let mock = canonical::MockStreamer::new().with_blocks(canonical::linked_blocks(100, 50));
let handle = mock.clone(); // keep for post-hoc assertions
let indexer = CanonicalIndexerClient::new(canonical::serve(mock).await, params);
```

Fault injection is endpoint-level: `with_fault(rpc, status)` fails one RPC
outright, `with_stream_fault(rpc, after, status)` drops a stream after N
items, and `replace_chain` swaps the served chain mid-test through any clone
(the reorg lever). `linked_blocks` builds a hash-linked run with
deterministic hashes from `mock_hash`, and the `sent()` /
`balance_queries()` accessors expose what the mock received.

`socks5` is an in-process RFC 1928 server standing in for
`nym-socks5-client`: it records each CONNECT target and splices to a fixed
upstream, which is what lets tests prove a connector sends hostnames to the
proxy instead of resolving them locally.

Passing mock tests prove self-consistency: both ends of the pipe share the
generated types. Conformance against servers that were not co-designed with
these types is the live suite's job (`crates/core/tests/live.rs`).
