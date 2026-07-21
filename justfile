# Zcash multi-variant lightwallet protocol layer

canonical_repo := "https://github.com/zcash/lightwallet-protocol.git"
canonical_service := "proto/canonical/walletrpc/service.proto"
overlay := "proto/overlay/crosslink.proto"
snapshot := "proto/overlay/reference/crosslink_monolith-service.proto"
snapshot_url := "https://raw.githubusercontent.com/ShieldedLabs/crosslink_monolith/main/zaino/zaino-proto/lightwallet-protocol/walletrpc/service.proto"

default:
    @just --list

# Offline checks: protos compile, overlay matches canonical plus additions, workspace builds
check: proto-check mirror-check rpc-coverage-check cargo-check feature-check

# Workspace compiles, both variants (proto crates regenerate bindings via build.rs)
cargo-check:
    cargo check --workspace --all-targets --all-features

# Every feature combination compiles, including neither variant and the reduced CLI
feature-check:
    cargo hack check -p lightwallet-core --feature-powerset --all-targets
    cargo hack check -p lightwallet-cli --feature-powerset --all-targets
    cargo nextest run -p lightwallet-core --no-default-features

# Offline test suite: unit tests + the in-memory mock harness (no network)
test:
    cargo nextest run --workspace --all-features

# Line/region coverage over the offline suite. Extra args pass through,
# e.g. `just coverage --html --open` for annotated source.
coverage *args='':
    cargo llvm-cov nextest --workspace --all-features {{args}}

# Live-endpoint validation, nightly not per-commit: indexers against real servers,
# plus the Tor transport (bootstraps arti). Canonical defaults to zec.rocks; export
# LIGHTWALLET_CROSSLINK_URL to cover the current featurenet.
live-check:
    cargo nextest run --workspace --all-features --run-ignored ignored-only

# Nym live run: the generic sync loop through a running nym-socks5-client.
# Export LIGHTWALLET_NYM_SOCKS5_ADDR (e.g. 127.0.0.1:1080); the test skips
# when it is unset. Prints the sync wall-clock, the promotion gate's
# measurement (CONTEXT.md).
live-check-nym:
    cargo nextest run -p lightwallet-transport-nym --run-ignored ignored-only --no-capture

# Both proto sets compile under protoc
proto-check:
    protoc -I proto/canonical/walletrpc --descriptor_set_out=/dev/null {{canonical_service}}
    protoc -I proto/canonical/walletrpc -I proto/overlay --descriptor_set_out=/dev/null {{overlay}}
    @echo "ok: canonical and overlay compile"

# Overlay must be canonical service.proto + added lines only (no edits/deletions)
mirror-check:
    #!/usr/bin/env bash
    set -euo pipefail
    removed=$(diff {{canonical_service}} {{overlay}} | grep -c '^<' || true)
    if [ "$removed" -ne 0 ]; then
        echo "FAIL: overlay is not purely additive vs canonical service.proto:" >&2
        diff {{canonical_service}} {{overlay}} | grep '^<' >&2
        echo "re-sync the mirror (required after every subtree pull of canonical)" >&2
        exit 1
    fi
    echo "ok: overlay == canonical + $(diff {{canonical_service}} {{overlay}} | grep -c '^>') added lines"

# Every RPC in the reference snapshot must exist in the overlay
rpc-coverage-check:
    #!/usr/bin/env bash
    set -euo pipefail
    missing=$(comm -23 \
        <(grep -oE 'rpc [A-Za-z0-9_]+' {{snapshot}} | sort -u) \
        <(grep -oE 'rpc [A-Za-z0-9_]+' {{overlay}} | sort -u))
    if [ -n "$missing" ]; then
        echo "FAIL: snapshot declares RPCs missing from the overlay:" >&2
        echo "$missing" >&2
        exit 1
    fi
    echo "ok: every snapshot RPC exists in the overlay"

# Diff the committed snapshot against crosslink_monolith's live copy
upstream-check:
    #!/usr/bin/env bash
    set -euo pipefail
    # Fetches the current copy of crosslink_monolith's wallet-facing
    # service.proto and diffs it against our pinned snapshot.
    if curl -sf "{{snapshot_url}}" | diff {{snapshot}} -; then
        echo "ok: snapshot matches crosslink_monolith main"
    else
        # A human decides what the diff means before the overlay changes.
        echo "UPSTREAM CHANGED: crosslink_monolith's service.proto differs from our snapshot." >&2
        echo "Inspect the diff, update {{overlay}} if the CROSSLINK surface moved," >&2
        echo "then run: just snapshot-refresh  (and bump provenance in reference/README.md)" >&2
        exit 1
    fi

# Replace the reference snapshot with the current live copy (then update provenance!)
snapshot-refresh:
    curl -sf "{{snapshot_url}}" -o {{snapshot}}
    @echo "snapshot refreshed. Update the commit and dates in proto/overlay/reference/README.md:"
    @git ls-remote https://github.com/ShieldedLabs/crosslink_monolith.git HEAD | cut -f1

# Pull canonical subtree at a new tag, e.g.: just canonical-pull v0.6.0
canonical-pull tag:
    git subtree pull --prefix proto/canonical {{canonical_repo}} {{tag}} --squash
    @echo "remember: re-sync {{overlay}} against the new canonical, then: just check"
