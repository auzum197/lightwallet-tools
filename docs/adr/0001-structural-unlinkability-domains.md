# Structural unlinkability domains

A tonic channel is one long-lived HTTP/2 connection, and a TCP stream over
Tor keeps its original circuit for its whole life, so the only place a
privacy boundary can be drawn is at construction. We made the boundaries
structural. `lightwallet-transport-tor` mints a fresh stream-isolation token
per `channel()`/`channel_lazy()` call, so two channels never share a circuit
unless the caller groups them deliberately with `channel_with_isolation`.
The identity-bearing RPCs (request content names a txid, a transparent
address, or a held-transaction list) live only on the per-domain
`CanonicalIdentityClient`/`CrosslinkIdentityClient` types. The indexers
cannot issue them. A wallet expresses its own partition by how many identity
clients it mints, one per identity it wants a server to see as a stranger,
and core never chooses the partition for it.

## Considered Options

- **Opt-in isolation** (the prior state: docs advised `isolated_client()`).
  Rejected because privacy that depends on the caller reading documentation
  defaults to off. Every channel in a process silently shared circuits.
- **Fresh circuit per request.** Unachievable over a long-lived channel
  without rebuilding it per RPC, and it misidentifies the unit: circuits may
  be shared freely within a domain, never across domains.
- **A `Broadcaster` carrying only `send_transaction`.** Too narrow.
  `get_transaction` and the address and utxo queries name wallet identifiers
  just as directly.
- **One shared second domain for all sensitive RPCs.** Protects the sync
  fingerprint while linking the wallet's txids and addresses to each other,
  which rebuilds the identity inside domain two.

## Consequences

Every channel pays its own circuit build, so identity clients should sit on
`channel_lazy` and the cost lands on first use. Over the direct transport
the split still binds (the types force it) but delivers no unlinkability,
since one IP links everything. A future prefs-taking variant of the
connector must keep the isolation token as its own required parameter rather
than burying it inside `StreamPrefs`, so domain membership stays outside the
configurable surface.
