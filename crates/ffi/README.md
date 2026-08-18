# lightwallet-ffi

UniFFI bindings over `lightwallet-core` for foreign consumers (Kotlin on
Android, Swift on iOS). A thin, lossy projection of core, not a second API:
core's variant-generic clients, associated types, and `BoxStream` stay on the
Rust side. This crate flattens them to what UniFFI can carry.

This is a scaffold. It proves the two RPC shapes the mobile app depends on, one
canonical endpoint each. The rest of the endpoint surface, variant switching,
and transport selection graft onto the same two shapes.

## The two shapes

**Unary.** An async Rust method returning a record or scalar. `latest_height`
becomes a Kotlin `suspend fun latestHeight(): ULong`. Nothing to design.

**Server-streaming.** Core hands back a `BoxStream`, which cannot cross FFI. The
call inverts: `stream_block_range` takes a foreign-implemented `BlockSink` and
drives the stream on the Rust side, pushing each item across the boundary by
calling the sink. The Kotlin side wraps that sink in a `callbackFlow` to recover
an idiomatic `Flow`. This inversion was the one ergonomic risk worth proving
before committing to UniFFI.

## Generating the bindings

```sh
cargo build -p lightwallet-ffi
cargo run -p lightwallet-ffi --bin uniffi-bindgen -- \
  generate --library target/debug/liblightwallet_ffi.dylib \
  --language kotlin --out-dir <dir>
```

On Android the library is the per-ABI `.so` a gradle plugin
(`rust-android-gradle` / cargo-ndk) cross-compiles and bundles into an `.aar`.
The generated Kotlin ships alongside it. `--language swift` covers iOS from the
same crate.

## The Flow bridge

The generated `BlockSink` is a plain Kotlin interface. Wrap the streaming call
in `callbackFlow` once, and every streaming RPC reads as a `Flow` at the call
site:

```kotlin
fun IndexerConnection.blockRange(start: ULong, end: ULong): Flow<BlockSummary> =
    callbackFlow {
        val sink = object : BlockSink {
            override fun onBlock(block: BlockSummary) { trySend(block) }
            override fun onError(message: String) { close(IndexerException(message)) }
            override fun onComplete() { close() }
        }
        streamBlockRange(start, end, sink)
        awaitClose { }
    }
```

Cancellation falls out of this. If the collector stops, `callbackFlow` cancels
its coroutine, which cancels the `suspend streamBlockRange`, which drops the
Rust future and its stream handle, which ends the underlying gRPC call. That is
the cancel path the mobile streaming stories need, with no extra plumbing.

## Scope

Foreign-language consumers only. Every Rust consumer (the CLI, a future Tauri
app) uses `lightwallet-core` directly and keeps the full generic surface.
Browser/WASM is a separate boundary (wasm-bindgen, and no Tor/Nym in a tab), not
this crate.
