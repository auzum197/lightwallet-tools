# Close the test coverage blind spots

## Context

The suite today proves the happy path well (generic sync over both indexers, one injected fault, one mid-stream fault, one NotFound, the CLI's rendering and tunnel paths) but leaves failure cases and regression traps uncovered. The first audit covered `core`, `transport-tor`, and the mock; a second sweep now covers the code that landed since (`crates/cli`, `crates/transport-nym`, `test-support/src/socks5.rs`) and adds a measured baseline.

From the first audit:

- `assert_continuity` has zero failure-case tests (height gap, repeated height, prev_hash mismatch, i.e. the reorg signal).
- `Error::retryable` is untested for `DeadlineExceeded`. A regression dropping it from the retryable set would pass the suite silently. `Display`/`source()` are untested too.
- `prev_block_hash()` is never called by an offline test. `hash32` edges (empty slice, oversized) untested.
- Most of the mock-modeled RPC surface is never exercised through an indexer: `get_transaction`, `get_lightd_info`, `ping`, `get_tree_state` happy path, `get_latest_tree_state` (highest-wins and empty→Unavailable), empty-chain tip, deprecated nullifier methods.
- `get_block_range` edges untested: single-block range (inclusive ends), descending range, range past tip, stream fault at position 0.
- Fault injection on crosslink-only RPCs untested; failure-path tests run against the canonical variant only.
- `authority()` in transport-tor: "no host" branch and IPv6 literals untested.
- Everything builds `--all-features` only, so a broken feature gate (canonical-only, crosslink-only, or no-features build) ships unnoticed.
- The mock serves a frozen chain, so reorg detection is never proven end-to-end over real wire round-trips.

From the second sweep:

- `core/src/streamer.rs` sits at 13.53% line coverage, the worst file in the workspace. The mock models 9 of the 19 shared RPCs; the other 10 (mempool ×2, taddress ×4, subtree roots, utxos ×2, plus the deliberate nullifier pair) answer UNIMPLEMENTED, so the macro-emitted client methods for them never run past the first `?`. The CLI subcommands for the same RPCs are equally unreachable end-to-end.
- `network_params()` is never called by any test.
- CLI `parse_tx_hex` has no unit tests (non-hex, empty, whitespace trim; only the stdin happy path runs, via e2e).
- `render.rs` has no unit tests. Through e2e only two behaviors are pinned (camelCase fields, hash/prevHash reversal). Untested: defaults emitted rather than dropped, null for absent singular messages, enum rendering by name with integer fallback for unknown numbers, and every `DISPLAY_ORDER` entry beyond `CompactBlock`.
- `emit`'s broken-pipe exit (`lwcli ... | head` must end quietly, render.rs:80-90) untested.
- `rpc_err`'s "(retryable)" stderr suffix untested.
- CLI subcommands never run e2e even though the mock models them: `get-transaction` (display-order txid plumbing!), `get-tree-state`, `get-latest-tree-state`, `get-bond-info` (hex arg parsing), `request-faucet-donation`, `completions` (its early return runs before the `--url` check), plus the missing-`--url` error and bad hex in `--exclude`/bond keys.
- The `--variant crosslink` dispatch arm for shared commands never runs; only the implied-crosslink path does.
- `transport-nym`'s `authority()` is a copy of tor's with the same two untested branches (no host, IPv6 literal). Its proxy addressing is unpinned for IP-literal endpoints: tokio-socks parses `127.0.0.1` into atyp 0x01, and bracketed `[::1]` fails that parse and leaves as a *domain* address, both unobserved by any test.
- The CLI's `tor`/`nym` features are absent from the feature matrix, so a broken gate (say, a `#[cfg]` slip on the `Transport` enum) only surfaces when someone builds without defaults.
- `test-support/src/socks5.rs` is test infrastructure, exercised through the tunnel and e2e suites; it is not itself a coverage target.

Decisions made: include the mutable-chain mock enhancement for a reorg test, add a cargo-hack feature-matrix recipe (cargo-hack is installed), extend the mock to model the remaining shared RPCs (step 12), and measure with cargo-llvm-cov (chosen below).

## Coverage loop

Tool: `cargo-llvm-cov` driving nextest (`cargo llvm-cov nextest`). Both it and tarpaulin are installed; llvm-cov wins because it uses rustc's own source-based instrumentation (exact region counts, no ptrace), runs natively on aarch64 macOS, and instruments spawned binaries, so the e2e tests' `lwcli` subprocess runs are counted (verified: `cli/src/main.rs` reports 71% today and is reached almost exclusively through `CARGO_BIN_EXE_lwcli`). Generated proto bindings live in `OUT_DIR` and never enter the report, so no filtering dance is needed.

Step 0 adds the recipe:

```
# Line/region coverage over the offline suite. Extra args pass through,
# e.g. `just coverage --html --open` for annotated source.
coverage *args='':
    cargo llvm-cov nextest --workspace --all-features {{args}}
```

The loop: `just coverage`, pick the file with the worst region column, close one blind spot from the steps below, rerun, and expect the number to move. If it didn't move, the test isn't exercising what it claims to. Regions are the finer signal than lines (the branch column needs nightly; skip it). For a product-only number, append `--ignore-filename-regex 'crates/test-support/'` to drop test infrastructure from the denominator.

Baseline, 2026-07-16, `--all-features` offline suite (lines / regions), with the steps that close each file:

| file | lines | regions | closed by |
|---|---|---|---|
| cli/src/main.rs | 71.24% | 43.90% | 10, 11, 12 |
| cli/src/render.rs | 63.37% | 59.31% | 10 |
| core/src/canonical.rs | 84.13% | 75.64% | 4 |
| core/src/crosslink.rs | 69.77% | 61.74% | 4, 5, 12 |
| core/src/error.rs | 86.36% | 84.29% | 2 |
| core/src/header.rs | 53.85% | 60.00% | 1 |
| core/src/indexer.rs | 100% | 100% | done |
| core/src/lib.rs | 100% | 100% | done |
| core/src/streamer.rs | 13.53% | 15.87% | 12 |
| test-support/src/mock.rs | 74.81% | 78.24% | 8, 12 |
| test-support/src/socks5.rs | 89.71% | 87.14% | 9 |
| transport-nym/src/lib.rs | 98.36% | 96.47% | 9 |
| transport-tor/src/lib.rs | 50.00% | 54.35% | 6 (partial, see floor) |
| TOTAL | 66.19% | 58.45% | |

Known floor: transport-tor's `connector`/`channel`/`channel_lazy` bodies need a live Tor client, so roughly half of that file stays red offline (the ignored live test owns it). The CLI's Tor arm and the https/TLS branch of `connect()` are in the same bucket. 100% is not the target; the target is every reachable branch of the offline surface, with the leftovers named under "explicitly not testing". Once the steps land, pin the floor in CI with `--fail-under-regions` (choose the number from the post-landing report, not now).

## Steps

Each step passes `just test` on its own and lands independently. Match the existing style in `crates/core/tests/mock_endpoint.rs` (descriptive snake_case sentence names, unwrap/expect fine in tests, no narrating comments).

### 0. Coverage recipe — `justfile`

The `coverage` recipe above. Land it first so every later step can show its delta. Not wired into `check` (it reruns the whole suite instrumented; that's a loop tool and a future CI job, not a per-commit gate yet).

### 1. Header unit tests — `crates/core/src/header.rs`

New `#[cfg(test)] mod tests` (feature-free, runs under every combo) with a minimal local `CompactBlockHeader` impl (`struct Hdr { hash: Vec<u8>, prev: Vec<u8> }`, height 0):

- `empty_hash_reports_zero_length` — `block_hash()` on empty → `Err(HashLen { len: 0 })`
- `oversized_hash_reports_its_length` — 33 bytes → `Err(HashLen { len: 33 })`; 32 bytes round-trips in the same test
- `prev_block_hash_converts_like_block_hash` — first offline exercise of `prev_block_hash()`: 32-byte ok, 20-byte → `Err(HashLen { len: 20 })`

### 2. Error unit tests — `crates/core/src/error.rs` (existing tests module)

- `deadline_exceeded_is_retryable` — closes the silent-regression hole in the retryable matrix
- `display_and_source_expose_the_underlying_failure` — `Rpc` variant: display starts with "indexer rpc failed: ", `source()` downcasts to `tonic::Status`; `HashLen` variant: display matches, `source()` downcasts to `HashLen`

### 3. Continuity failure tests — `crates/core/src/lib.rs` (existing gated tests module)

Build `CompactBlock` literals like `continuity_holds_across_both_variants` does:

- `continuity_rejects_a_skipped_height` — prev 100, cur 102, matching hashes → false
- `continuity_rejects_a_repeated_height` — both 100 → false
- `continuity_rejects_a_prev_hash_mismatch_at_the_next_height` — the reorg signal; assert on both variants' block types

### 4. Shared-surface and range-edge tests — `crates/core/tests/mock_endpoint.rs`

All use the existing serve-then-assert pattern; canonical variant unless noted. Mock builders already support everything here (verified in `crates/test-support/src/mock.rs`), no test-support changes.

- `get_transaction_round_trips_and_unknown_txid_is_not_found` — `with_transaction`, fetch back, unknown txid → NotFound, not retryable
- `lightd_info_and_ping_answer_from_the_mock` — `with_lightd_info`, version round-trips, `ping(0)` succeeds
- `tree_state_lookups_serve_registered_heights_and_latest_picks_the_highest` — `with_tree_state` at heights 100 and 200: `get_tree_state(100)` right one, `get_latest_tree_state()` → 200 (BTreeMap `next_back()`), `get_tree_state(150)` → NotFound
- `empty_mock_reports_unavailable_for_tip_and_latest_tree_state` — bare mock: both err Unavailable and retryable
- `deprecated_nullifier_methods_surface_unimplemented` — `#[allow(deprecated)]`; both nullifier methods err Unimplemented (tonic surfaces the streaming one's status at call time)
- `single_block_range_yields_exactly_one_block` — range 6..6 on `linked_blocks(5, 3)` → one item, pins inclusive ends
- `descending_range_streams_blocks_in_reverse` — range 4..0 → heights [4,3,2,1,0] (mock reverses at mock.rs:187)
- `range_past_the_tip_returns_the_existing_prefix` — range 0..10 on 3 blocks → 3 items, no error item
- `stream_fault_at_position_zero_fails_before_any_block` — `with_stream_fault(Rpc::GetBlockRange, 0, ...)` → first item Err, then None
- `genesis_block_carries_an_all_zero_prev_hash` — `linked_blocks(0, 2)`: `get_block(0).prev_block_hash().unwrap() == [0u8; 32]`, continuity holds 0→1 over the wire
- `network_params_return_the_carried_values` — the one accessor no test calls; construct with a non-default `NetworkParams`, read it back through the trait

### 5. Crosslink parity and crosslink fault injection — `mock_endpoint.rs`

Add two generic helpers next to `sync_to_tip` (mock setup stays per-variant since the two `MockStreamer` types are distinct):

```rust
async fn assert_tip_unavailable<I: TestnetIndexer>(indexer: &I)
async fn assert_mid_stream_fault<I: TestnetIndexer>(indexer: &I, start: u64, end: u64, ok_before_fault: usize)
```

- Rework the step-4 empty-mock test onto `assert_tip_unavailable`; add `crosslink_empty_mock_reports_unavailable_at_the_tip`
- `crosslink_mid_stream_fault_arrives_as_an_error_item` via `assert_mid_stream_fault`; refactor the existing canonical mid-stream test onto the helper
- `faults_on_crosslink_only_rpcs_surface_with_their_codes` — one crosslink mock, three distinct faults: `GetRoster`→Unavailable, `GetBondInfo`→PermissionDenied, `RequestFaucetDonation`→ResourceExhausted. Distinct codes prove the fault map keys per-RPC. Only the roster error is retryable.

Land 4 before 5 (or merge them) since 5 reworks one step-4 test.

### 6. Tor authority tests — `crates/transport-tor/src/lib.rs` (existing tests module)

- `uri_without_a_host_is_rejected` — `"/no/host"` parses as path-only, hits the missing-host branch (lib.rs:62-64)
- `ipv6_literal_host_keeps_its_brackets` — `"https://[::1]:9067"`; `Uri::host()` returns the bracketed form, which flows into `tor.connect((host, port))`. Pin observed behavior; if brackets surprise at implementation time, that's a real finding to surface, not silently fix.

Skipped deliberately: `channel_lazy` offline (infallible construction, nothing to assert without a TorClient; the ignored live test covers the connected path). This is the transport-tor floor in the baseline table.

### 7. Feature-matrix recipe — `justfile`

```
# Every feature combination compiles, including neither variant and the reduced CLI
feature-check:
    cargo hack check -p lightwallet-core --feature-powerset --all-targets
    cargo hack check -p lightwallet-cli --feature-powerset --all-targets
    cargo nextest run -p lightwallet-core --no-default-features
```

Wire into the aggregate: `check: proto-check mirror-check rpc-coverage-check cargo-check feature-check`. The nextest line makes the feature-free unit tests from steps 1-2 actually run in a reduced config. The CLI powerset is four combos (two build arti, check-only) and is what compile-covers the `cfg(not(feature = "nym"))` twin of `Cli::transport()` and the gated `Transport` enum variants. Expect a small tidy-up: under the no-variant combo, `streamer.rs`'s `pub(crate)` items and `wrap_stream` likely warn as unused; add `#[cfg(any(feature = "canonical", feature = "crosslink"))]` where needed.

### 8. Reorg simulation — `crates/test-support/src/mock.rs` + one test

Mirror the `sent` inbox pattern (Arc<Mutex> shared across clones):

- Change field to `blocks: Arc<Mutex<Vec<$proto::CompactBlock>>>`; `with_blocks` keeps its signature (locks and replaces)
- Add `pub fn replace_chain(&self, blocks: impl IntoIterator<Item = $proto::CompactBlock>)` — works through any clone, like `sent()`
- `get_latest_block`, `block_at`, `get_block_range` lock instead of borrowing (~10 lines)

New test `consumer_detects_a_reorg_through_continuity` in `mock_endpoint.rs`: serve `linked_blocks(0, 5)`, keep a mock clone, sync and hold block 3. `replace_chain` with blocks 0-2 unchanged plus a forked block 3 (hash `mock_hash(1003)`) and a block 4 linking to it. Fetch new block 4: `assert_continuity(held_3, new_4)` is false; fetch new block 3: continuity from block 2 holds on the fork.

### 9. Nym authority parity and proxy addressing — `crates/transport-nym`

`authority()` here is a copy of tor's with the same untested branches. In the existing lib.rs tests module:

- `uri_without_a_host_is_rejected` — mirrors step 6
- `ipv6_literal_host_keeps_its_brackets` — mirrors step 6; here the downstream consequence is observable offline (next bullet)

In `tests/tunnel.rs`, against the socks5 mock (which already decodes all three address types):

- `ip_literal_endpoint_connects_by_address` — endpoint `http://127.0.0.1:9067`; tokio-socks parses the IP, so the recorded CONNECT has `atyp == 0x01`. Pins that the no-DNS-leak property is about names; IP literals go as addresses.
- `ipv6_literal_reaches_the_proxy_as_a_domain` — endpoint `http://[::1]:9067`; `"[::1]"` fails tokio-socks' IpAddr parse and ships as a domain address (`atyp == 0x03`, host `"[::1]"`). A real requester would likely fail to resolve that string; pinning it documents the behavior, and if the assertion surprises at implementation time, surface it as a finding.

### 10. CLI unit tests — `crates/cli/src/main.rs` and `crates/cli/src/render.rs`

main.rs (extend the existing tests module):

- `tx_hex_rejects_junk_and_empty_input` — `parse_tx_hex`: non-hex → err mentioning hex, `""` → "transaction is empty", `"  deadbeef\n"` trims and decodes
- extend `txids_parse_from_display_order` with a non-hex 64-char input, so both rejection reasons (length, alphabet) are pinned

render.rs (new `#[cfg(test)] mod tests`; the helpers are private so the tests live in-file, building typed prost messages and going through `Renderer::new(OutputMode::Json)` + `json`, which the module can call):

- `defaults_are_emitted_not_dropped` — `SendResponse::default()` → `{"errorCode": 0, "errorMessage": ""}`, the documented departure from canonical proto JSON
- `absent_singular_messages_render_null` — `BlockRange::default()`: `start`/`end` are `null`, not missing
- `enums_render_by_name_with_integer_fallback` — `GetSubtreeRootsArg` with `shielded_protocol` 1 → `"orchard"`, 42 → `42`
- `display_order_reverses_only_its_listed_fields` — `CompactTx.txid` prints reversed, `RawTransaction.data` prints wire-order; proves the `(message, field)` keying beyond the e2e-covered `CompactBlock` rows

### 11. CLI e2e over the already-modeled surface — `crates/cli/tests/e2e.rs`

All against the existing mock builders, same serve-then-run pattern:

- `get_transaction_round_trips_display_order` — register `with_transaction(wire_txid, tx)`, invoke with the byte-reversed hex; the call only succeeds if `parse_txid`'s reversal matches the mock's key. Unknown txid exits nonzero.
- `tree_states_render_at_height_and_at_the_tip` — `with_tree_state` at two heights; `get-tree-state` picks by height, `get-latest-tree-state` the highest
- `retryable_failures_say_so_on_stderr` — `with_fault(GetLightdInfo, unavailable)` → stderr contains "(retryable)"; the existing NotFound test already shows the absence case
- `explicit_crosslink_variant_drives_shared_commands` — `--variant crosslink get-block` against the crosslink mock; the dispatch arm nothing currently enters
- `bond_info_takes_wire_order_hex` — `with_bond` round trip plus a "not valid hex" nonzero exit
- `faucet_donation_renders_the_amount` — `with_faucet_amount`
- `completions_print_without_a_url` — `lwcli completions bash` exits 0 with nonempty stdout; pins the early return running before the `--url` requirement
- `a_missing_url_is_a_clear_error` — `get-latest-height` with no `--url`; stderr contains "--url is required"
- `debug_mode_streams_typed_lines` — `--output debug get-block-range`; every line starts with "CompactBlock {" (the `Renderer::item` debug arm; the unary arm is already covered)
- `closed_pipe_ends_the_stream_quietly` — spawn `get-block-range` over a long chain with stdout piped, read one line, drop the pipe, expect exit 0 (the `emit` BrokenPipe branch)
- `exclude_arguments_must_be_hex` — `get-mempool-tx --exclude zz` against a served mock exits nonzero with "--exclude takes hex" (the decode runs after connect, so a URL is needed; the happy path lands in step 12)

### 12. Model the remaining shared RPCs in the mock, then cover `streamer.rs` end to end

The big one: `streamer.rs` at 13.53% is the workspace's worst file because 10 RPCs answer UNIMPLEMENTED. Extend `test-support` (new `Rpc` variants; builders in the macro so both variants get them; everything routed through `check()` and `stream_of` so `with_fault`/`with_stream_fault` work on the new surface for free):

- `with_mempool_txs(Vec<CompactTx>)` — `GetMempoolTx` applies the txid-suffix exclude filter (`ends_with`), the documented semantics
- `with_mempool_stream(Vec<RawTransaction>)` — served then closed (the mock has no "next block" to wait for)
- `with_taddress_txs(address, Vec<RawTransaction>)` — both `get_taddress_transactions` and the deprecated `get_taddress_txids` serve the same table, filtered to `[start, end]` by tx height
- `with_balance(u64)` — both balance RPCs; the client-streaming one drains the request stream and records the addresses in an inbox like `sent()` (say `balance_queries()`)
- `with_utxos(Vec<GetAddressUtxosReply>)` — unary honors `max_entries` (0 = all); the stream form goes through `stream_of`
- `with_subtree_roots(Vec<SubtreeRoot>)` — honors `start_index` and `max_entries`

Tests in `mock_endpoint.rs` (canonical unless noted):

- `mempool_txs_honor_the_exclude_suffixes`
- `mempool_stream_drains_to_completion`
- `taddress_transactions_filter_by_height_range` — also drives the deprecated form under `#[allow(deprecated)]`
- `balance_answers_both_unary_and_client_streaming` — the streaming case asserts the mock saw every address; this is the only client-streaming RPC in the surface, currently zero-covered end to end
- `utxos_respect_max_entries_on_both_forms`
- `subtree_roots_page_from_start_index`
- `crosslink_mempool_txs_flow_through_the_macro_surface` — one crosslink duplicate of one family; full parity is the macro's job and step 5 already proves fault parity
- Retire `unmodeled_rpcs_answer_unimplemented` (`get_taddress_balance` now answers); the step-4 nullifier test keeps the UNIMPLEMENTED path pinned

CLI e2e rides the same builders:

- `mempool_txs_stream_with_excludes` — the happy `--exclude` path step 11 left out
- `address_utxos_render_as_a_list`
- `subtree_roots_take_the_protocol_flag` — the `Protocol` → `ShieldedProtocol` mapping in `main.rs`

After this step `streamer.rs` should read near-full in `just coverage`; if a method still shows red, it's a signal a test above silently skipped it.

## Explicitly not testing

- Mock duplex single-connection semantics: server task handle is private to `serve()`; would need a test-only `serve_with_handle` API with a single consumer. Enforced by construction (`Option::take`, mock.rs:418).
- Stream fault with `after >= item count`: truncate is a no-op and the trailing error is a mock artifact no real server exhibits; pinning it would teach a false wire behavior.
- Per-RPC latency injection: indexers set no deadlines and `serve()` builds the Endpoint internally, so a delay has nothing to trip. `with_fault(rpc, Status::deadline_exceeded(..))` already covers the wire path; retryable classification is pinned in step 2. Revisit if indexers grow timeout config.
- `ping` standalone: mock returns zeros; folded into the lightd-info test.
- `render.rs` `map_key`: neither proto declares a `map<>` field (grepped both service files and compact_formats), so the arm is unreachable until one appears. If protos grow a map, this note is the reminder.
- The CLI Tor arm and transport-tor's `connector`/`channel`/`channel_lazy` offline: they need a bootstrapped Tor client. `just live-check` owns them; this is the accepted floor in the baseline table.
- The https/TLS branch of the CLI's `connect()`: the mock speaks plaintext HTTP/2, and cert fixtures would test tonic's TLS, not this repo's code. The live suite covers TLS against zec.rocks.
- `--timeout`: it configures tonic's `connect_timeout`; a test needs a black-holed address and asserts on wall-clock, which flakes. The flag is plumbing, not logic.
- Runtime behavior of reduced CLI feature builds (the no-nym `Cli::transport()` twin): compile-checked by the step-7 powerset; running the suite per-combo buys one `match` arm for a doubled CI bill.
- `test-support/src/socks5.rs` as a target of its own: it is scaffolding, exercised by the tunnel and e2e suites (89.71% at baseline; step 9's IP-literal tests pick up the 0x01/0x04 arms incidentally).

## Verification

- `just test` (nextest, --all-features) green after each step
- `just check` green after step 7, including the new `feature-check` powerset
- `cargo nextest run -p lightwallet-core --no-default-features` runs the step 1-2 tests
- `just coverage` after each step; the touched file's region column must move. Expected end state: `streamer.rs` from 13.53% lines to near-full, `main.rs` ~71%→~90%, `render.rs` ~63%→~90% (the map arm stays red), overall lines from 66% into the mid-80s with the tor floor accounted for
- Landed 2026-07-16, all steps done, 94 tests: overall lines 66.19%→90.05% (regions 58.45%→83.23%), `streamer.rs` 96.15% lines after its refactor, `main.rs` 82.60% (the rest is the tor arm, the https branch, and connect error contexts), `render.rs` 85.33% (map arm and the non-pipe emit error red), `crosslink.rs` 97.17% after one extra parity test for the hand-written trait methods, and the new `identity.rs` at 100% lines. `transport-tor` reads 43.68%: the accepted connector floor, now wider because the isolation-token variants (`channel_with_isolation`, `channel_lazy_with_isolation`) live behind the same live-Tor wall; `just live-check` owns them
- Steps 4/5/8/11/12: temporarily invert an assertion (e.g. continuity on the forked block, or the atyp byte in step 9) to confirm the test can actually fail, then restore
- After everything lands: pick the CI floor from the fresh report and add `--fail-under-regions` to a CI-facing coverage invocation

## Critical files

- `crates/core/src/header.rs`, `crates/core/src/error.rs`, `crates/core/src/lib.rs` (inline test modules)
- `crates/core/tests/mock_endpoint.rs` (bulk of new core tests + helpers)
- `crates/transport-tor/src/lib.rs`, `crates/transport-nym/src/lib.rs` (authority tests), `crates/transport-nym/tests/tunnel.rs` (proxy addressing)
- `crates/cli/src/main.rs`, `crates/cli/src/render.rs` (unit test modules), `crates/cli/tests/e2e.rs` (new e2e)
- `crates/test-support/src/lib.rs` (Rpc enum growth), `crates/test-support/src/mock.rs` (steps 8 and 12)
- `justfile` (steps 0 and 7)
