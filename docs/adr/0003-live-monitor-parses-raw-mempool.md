# Live monitor parses the raw mempool stream

The Live monitor (`lwtui`) feeds its mempool view from `get_mempool_stream`
on the indexer client, which yields `RawTransaction` (serialized tx bytes plus
a height), and parses each transaction client-side with `zcash_primitives` to
recover the txid and pool structure. The obvious alternative, `get_mempool_tx`
on the identity client, returns `CompactTx` with a server-computed `txid` and
per-pool counts already in hand, no transaction parser and no orchard/halo2
dependency. We took the raw path anyway: it is chain-wide and identity-free by
construction, which is what a neutral mempool viewer is, while `get_mempool_tx`
is an identity-bearing RPC shaped for a wallet excluding its own held
transactions. The txid a raw v5/v6 transaction lacks is a BLAKE2b tree hash
over structured digests, not a hash of the bytes, so recovering it means a real
parser: `Transaction::read` fed the deployment's current consensus `BranchId`
from `GetLightdInfo`, then `tx.txid()` and the bundle accessors.

## Considered Options

- **`get_mempool_tx` / `CompactTx`.** The txid and pool counts arrive
  server-computed, no parser, no Zcash crypto dependency. Rejected because it
  is an identity-bearing RPC: its request carries an exclude list naming held
  transactions, so it lives on the per-domain identity client (ADR 0001). Using
  it with an empty exclude list to fake a firehose puts a chain-wide diagnostic
  view on the identity surface, the wrong seam for a tool that names nothing.
- **`get_mempool_stream`, hash the bytes for a txid.** A double-SHA256 of the
  serialized transaction is the txid only pre-v5. A mempool holds v5 and v6
  transactions, whose txid is a personalized tree hash, so a byte hash would
  display ids no block explorer agrees with.

## Consequences

The monitor carries `zcash_primitives >= 0.30.0` and `orchard >= 0.15`, the
first releases that parse V6 (Ironwood) transactions via a separate
`ironwood_bundle()`. Nothing older reads a V6 transaction, it fails at parse
rather than dropping the Ironwood actions, so the version floor is load-bearing
once NU6.3 is live. A transaction the parser cannot read gets an inline
degraded row, never a silent drop, so the mempool never looks emptier than it
is. Fee is exact only when a transaction has no transparent inputs (their
amounts live in the spent UTXOs, off-chain from the tx bytes); otherwise the
row omits it rather than pay a prevout lookup per input on a live stream.
