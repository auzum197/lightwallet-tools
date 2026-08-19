//! Shared transaction projection for the lightwallet tools.
//!
//! The one place a serialized Zcash transaction becomes a readable view: txid,
//! version, size, per-pool inputs and outputs down to nullifiers and scripts,
//! moved value, and fee. Both the inspector (`lwcli`) and the dashboard
//! (`lwtui`) parse through [`parse`], so they can never disagree about what a
//! transaction is; they differ only in how they present it. [`ParsedTx`]
//! serializes to the canonical JSON shape, so the tools' machine output stays
//! identical too.
//!
//! A transaction's bytes carry its transparent outputs' values but not its
//! transparent inputs': an input names the funding output ([`OutPoint`]) it
//! spends, and the amount lives in that output. [`parse`] therefore leaves the
//! input total [`InputTotal::Pending`] and records the prevouts; a caller that
//! can follow them ([`ParsedTx::resolve`]) fills the total and turns the fee
//! exact. Absent that resolution, [`parse`] reads only the transaction bytes,
//! no keys and no spent UTXOs, so no note plaintext and no shielded amount ever
//! appears.

mod params;

use std::fmt;

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, Parameters};

pub use params::{ChainParams, FeatureNet};
// Re-exported so callers naming a height for `parse` need no direct
// `zcash_protocol` dependency.
pub use zcash_protocol::consensus::BlockHeight;

/// A value pool a transaction touches on one side (input or output).
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Pool {
    Transparent,
    Sapling,
    Orchard,
    Ironwood,
}

impl fmt::Display for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Pool::Transparent => "transparent",
            Pool::Sapling => "sapling",
            Pool::Orchard => "orchard",
            Pool::Ironwood => "ironwood",
        };
        f.write_str(name)
    }
}

/// An encrypted blob shown as its length, never its bytes: the ciphertext is
/// public but says nothing without a key, and the raw hexdump already carries
/// every byte for anyone who wants them.
#[derive(Clone, Copy, Serialize)]
pub struct Blob {
    pub bytes: usize,
}

/// A transparent input: which prior output it spends, its unlocking script, and
/// its sequence number.
#[derive(Clone, Serialize)]
pub struct TxIn {
    pub prevout_txid: String,
    pub prevout_index: u32,
    pub script_sig: String,
    pub sequence: u32,
}

/// A transparent output: its value in the clear, its locking script, and the
/// decoded address for a standard P2PKH/P2SH script (`None` for non-standard,
/// where the script hex is the only handle).
#[derive(Clone, Serialize)]
pub struct TxOut {
    pub value_zat: i64,
    pub script_pubkey: String,
    pub address: Option<String>,
}

/// A Sapling spend: the note's nullifier and the randomized verification key.
#[derive(Clone, Serialize)]
pub struct SaplingSpend {
    pub nullifier: String,
    pub rk: String,
}

/// A Sapling output: the note commitment, ephemeral key, and the two
/// ciphertexts as presence-plus-length.
#[derive(Clone, Serialize)]
pub struct SaplingOutput {
    pub cmu: String,
    pub ephemeral_key: String,
    pub enc_ciphertext: Blob,
    pub out_ciphertext: Blob,
}

/// The spend half of an Orchard/Ironwood action.
#[derive(Clone, Serialize)]
pub struct ActionSpend {
    pub nullifier: String,
    pub rk: String,
}

/// The output half of an Orchard/Ironwood action.
#[derive(Clone, Serialize)]
pub struct ActionOutput {
    pub cmx: String,
    pub ephemeral_key: String,
    pub enc_ciphertext: Blob,
    pub out_ciphertext: Blob,
}

/// An Orchard/Ironwood bundle's spend/output enable flags.
#[derive(Clone, Copy, Serialize)]
pub struct Flags {
    pub spends_enabled: bool,
    pub outputs_enabled: bool,
}

/// One pool's contribution on the spend side. Bundle-level facts (anchor, value
/// balance, flags) sit on the entry once, not duplicated per item.
#[derive(Clone, Serialize)]
#[serde(tag = "pool", rename_all = "lowercase")]
pub enum PoolInput {
    Transparent {
        vin: Vec<TxIn>,
    },
    Sapling {
        anchor: String,
        value_balance_zat: i64,
        spends: Vec<SaplingSpend>,
    },
    Orchard {
        anchor: String,
        value_balance_zat: i64,
        flags: Flags,
        spends: Vec<ActionSpend>,
    },
    Ironwood {
        anchor: String,
        value_balance_zat: i64,
        flags: Flags,
        spends: Vec<ActionSpend>,
    },
}

impl PoolInput {
    /// Which pool this entry describes.
    pub fn pool(&self) -> Pool {
        match self {
            PoolInput::Transparent { .. } => Pool::Transparent,
            PoolInput::Sapling { .. } => Pool::Sapling,
            PoolInput::Orchard { .. } => Pool::Orchard,
            PoolInput::Ironwood { .. } => Pool::Ironwood,
        }
    }
}

/// One pool's contribution on the output side.
#[derive(Clone, Serialize)]
#[serde(tag = "pool", rename_all = "lowercase")]
pub enum PoolOutput {
    Transparent { vout: Vec<TxOut> },
    Sapling { outputs: Vec<SaplingOutput> },
    Orchard { outputs: Vec<ActionOutput> },
    Ironwood { outputs: Vec<ActionOutput> },
}

impl PoolOutput {
    /// Which pool this entry describes.
    pub fn pool(&self) -> Pool {
        match self {
            PoolOutput::Transparent { .. } => Pool::Transparent,
            PoolOutput::Sapling { .. } => Pool::Sapling,
            PoolOutput::Orchard { .. } => Pool::Orchard,
            PoolOutput::Ironwood { .. } => Pool::Ironwood,
        }
    }
}

/// The value a transaction moved, as far as the wire reveals it.
#[derive(Clone)]
pub enum Value {
    /// Fully transparent outputs: the amount is in the clear (zatoshis).
    Clear(i64),
    /// The tx has shielded outputs, so the moved amount is encrypted.
    Shielded,
    /// The tx did not parse, so there's no value to show.
    Unknown,
}

/// The funding output a transparent input spends: a transaction hash plus the
/// index of the output within it. The value the input contributes lives in that
/// output, off-chain from the spending tx, so learning it means following this
/// reference to its source.
#[derive(Clone)]
pub struct OutPoint {
    /// Funding transaction hash in wire (internal) byte order, as
    /// `get_transaction` wants it. Display order is its reverse.
    pub hash: [u8; 32],
    /// Index of the funded output within that transaction.
    pub index: u32,
}

impl OutPoint {
    /// The funding txid in display (explorer) order.
    pub fn txid(&self) -> String {
        hash_to_display(&self.hash)
    }
}

impl Serialize for OutPoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("txid", &self.txid())?;
        map.serialize_entry("index", &self.index)?;
        map.end()
    }
}

/// The summed value of a transaction's transparent inputs, and whether it is
/// yet known. Distinct from the shielded case in [`Value`]: shielded amounts are
/// unknowable without keys, an input total is knowable but needs a lookup.
#[derive(Clone)]
pub enum InputTotal {
    /// No transparent inputs; there is nothing to resolve.
    None,
    /// Transparent inputs present, their funding values not yet looked up.
    Pending,
    /// Summed transparent input value, in zatoshis.
    Known(i64),
    /// A funding lookup failed, so the total (and the fee) can't be shown.
    Unresolvable,
}

/// A transaction fee, and whether it is yet knowable. Derived from the input
/// total: exact once the transparent inputs resolve, pending until then.
pub enum Fee {
    /// Exact fee in zatoshis.
    Known(i64),
    /// Waiting on transparent input resolution.
    Pending,
    /// Input resolution failed, so the fee can't be shown.
    Unresolvable,
    /// The tx did not parse.
    Unknown,
}

/// A serialized transaction, read into the fields a human or a program wants.
/// `txid` is `None` only when the bytes did not parse, in which case `error`
/// carries why and the row still means something.
#[derive(Clone)]
pub struct ParsedTx {
    /// Txid in display order (explorer order).
    pub txid: Option<String>,
    /// Transaction version, as `zcash_primitives` names it (e.g. `V5`, `V6`).
    pub version: Option<String>,
    /// Serialized size in bytes.
    pub size: usize,
    /// The consensus branch the tx decoded under, so the ruleset is legible.
    pub consensus_branch_id: Option<String>,
    pub lock_time: Option<u32>,
    pub expiry_height: Option<u32>,
    pub inputs: Vec<PoolInput>,
    pub outputs: Vec<PoolOutput>,
    pub value: Value,
    /// Transparent output values in order (zatoshis). What this transaction
    /// offers as funding to a later one: a spender's [`OutPoint`] with index `i`
    /// draws `vout_values[i]`.
    pub vout_values: Vec<i64>,
    /// The funding outputs this transaction's transparent inputs spend, in input
    /// order. Empty when the tx has no transparent inputs. Following these is
    /// what turns [`InputTotal::Pending`] into a known total.
    pub prevouts: Vec<OutPoint>,
    /// The transparent input total's resolution state.
    pub input_total: InputTotal,
    /// The fee's input-independent term, `shielded value balance - transparent
    /// out`, always known from the tx bytes. `None` only on a parse failure.
    /// Added to the resolved input total to get the exact fee. Not serialized:
    /// callers read [`ParsedTx::fee`], not this term.
    pub fee_base: Option<i64>,
    pub error: Option<String>,
}

impl ParsedTx {
    /// The fee, derived from the input total. Exact once the transparent inputs
    /// resolve, or immediately when there are none.
    pub fn fee(&self) -> Fee {
        let Some(base) = self.fee_base else {
            return Fee::Unknown;
        };
        match self.input_total {
            InputTotal::None => Fee::Known(base),
            InputTotal::Pending => Fee::Pending,
            InputTotal::Known(total) => Fee::Known(base + total),
            InputTotal::Unresolvable => Fee::Unresolvable,
        }
    }

    /// Apply a followed transparent input total: `Some(total)` when every
    /// prevout resolved, `None` when one could not be, which leaves the total
    /// (and the fee) unresolvable rather than wrong. A no-op unless the tx was
    /// awaiting resolution.
    pub fn resolve(&mut self, total: Option<i64>) {
        if !matches!(self.input_total, InputTotal::Pending) {
            return;
        }
        self.input_total = match total {
            Some(total) => InputTotal::Known(total),
            None => InputTotal::Unresolvable,
        };
    }

    /// A compact one-line human summary: short txid, version, pools, value, fee.
    /// The censoring is textual here (`shielded`), not the TUI's redaction bar.
    pub fn summary(&self) -> String {
        if let Some(err) = &self.error {
            return format!("(no txid)  {err}");
        }
        let txid = self
            .txid
            .as_deref()
            .map(short_txid)
            .unwrap_or_else(|| "(no txid)".to_string());
        let version = self.version.as_deref().unwrap_or("?");
        let pools = format!(
            "{} → {}",
            input_pools(&self.inputs),
            output_pools(&self.outputs)
        );
        let value = match self.value {
            Value::Clear(zats) => format_zats(zats),
            Value::Shielded => "shielded".to_string(),
            Value::Unknown => "—".to_string(),
        };
        format!(
            "{txid}  {version}  {pools}  {value}  fee {}",
            fee_text(&self.fee())
        )
    }
}

/// The pools present on the input side, `+`-joined, or `none`.
pub fn input_pools(inputs: &[PoolInput]) -> String {
    pool_join(inputs.iter().map(|p| p.pool()))
}

/// The pools present on the output side, `+`-joined, or `none`.
pub fn output_pools(outputs: &[PoolOutput]) -> String {
    pool_join(outputs.iter().map(|p| p.pool()))
}

fn pool_join(pools: impl Iterator<Item = Pool>) -> String {
    let joined = pools.map(|p| p.to_string()).collect::<Vec<_>>().join("+");
    if joined.is_empty() {
        "none".to_string()
    } else {
        joined
    }
}

fn short_txid(txid: &str) -> String {
    if txid.len() <= 16 {
        return txid.to_string();
    }
    format!("{}…{}", &txid[..8], &txid[txid.len() - 6..])
}

/// A fee rendered for a human: an amount, or a word for the states that have no
/// number yet.
fn fee_text(fee: &Fee) -> String {
    match fee {
        Fee::Known(zats) => format_zats(*zats),
        // "unresolved", not "pending": the shared summary serves lwcli too,
        // which reads a tx without following prevouts, so nothing is in flight.
        Fee::Pending => "unresolved".to_string(),
        Fee::Unresolvable => "?".to_string(),
        Fee::Unknown => "—".to_string(),
    }
}

/// A funding hash (wire order) as a display-order txid.
fn hash_to_display(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in hash.iter().rev() {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Format zatoshis as ZEC without trailing zeros. Integer arithmetic keeps all
/// eight places exact: 10_000 reads "0.0001 ZEC", 100_000_000 reads "1 ZEC".
pub fn format_zats(zats: i64) -> String {
    let abs = zats.unsigned_abs();
    let whole = abs / 100_000_000;
    let frac = abs % 100_000_000;
    let sign = if zats < 0 { "-" } else { "" };
    if frac == 0 {
        format!("{sign}{whole} ZEC")
    } else {
        let frac = format!("{frac:08}");
        format!("{sign}{whole}.{} ZEC", frac.trim_end_matches('0'))
    }
}

impl Serialize for ParsedTx {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("txid", &self.txid)?;
        map.serialize_entry("version", &self.version)?;
        map.serialize_entry("size", &self.size)?;
        map.serialize_entry("consensus_branch_id", &self.consensus_branch_id)?;
        map.serialize_entry("lock_time", &self.lock_time)?;
        map.serialize_entry("expiry_height", &self.expiry_height)?;
        map.serialize_entry("inputs", &self.inputs)?;
        map.serialize_entry("outputs", &self.outputs)?;
        // Value is either a clear amount or the censored marker, never both, so
        // the key itself carries the distinction.
        match self.value {
            Value::Clear(zats) => map.serialize_entry("value_zat", &zats)?,
            Value::Shielded => map.serialize_entry("value", "shielded")?,
            Value::Unknown => map.serialize_entry("value", &Option::<i64>::None)?,
        }
        if !self.prevouts.is_empty() {
            map.serialize_entry("prevouts", &self.prevouts)?;
        }
        // The transparent input total, keyed like value: an amount under
        // `inputs_value_zat`, a state word under `inputs_value`, absent when the
        // tx has no transparent inputs.
        match self.input_total {
            InputTotal::None => {}
            InputTotal::Pending => map.serialize_entry("inputs_value", "unresolved")?,
            InputTotal::Known(total) => map.serialize_entry("inputs_value_zat", &total)?,
            InputTotal::Unresolvable => map.serialize_entry("inputs_value", "unresolvable")?,
        }
        match self.fee() {
            Fee::Known(zats) => map.serialize_entry("fee", &zats)?,
            Fee::Pending => map.serialize_entry("fee", "unresolved")?,
            Fee::Unresolvable => map.serialize_entry("fee", "unresolvable")?,
            Fee::Unknown => map.serialize_entry("fee", &Option::<i64>::None)?,
        }
        map.serialize_entry("error", &self.error)?;
        map.end()
    }
}

/// Read a serialized transaction into its projection. The branch is derived from
/// `height` under `params`, so a historical tx decodes under the rules active at
/// its own height, not the tip's. A mempool tx has no height: pass the tip.
pub fn parse<P: Parameters>(data: &[u8], height: BlockHeight, params: &P) -> ParsedTx {
    let mut view = ParsedTx {
        txid: None,
        version: None,
        size: data.len(),
        consensus_branch_id: None,
        lock_time: None,
        expiry_height: None,
        inputs: Vec::new(),
        outputs: Vec::new(),
        value: Value::Unknown,
        vout_values: Vec::new(),
        prevouts: Vec::new(),
        input_total: InputTotal::None,
        fee_base: None,
        error: None,
    };

    let branch = BranchId::for_height(params, height);
    let tx = match Transaction::read(data, branch) {
        Ok(tx) => tx,
        Err(e) => {
            view.error = Some(format!("unparseable: {e}"));
            return view;
        }
    };

    view.version = Some(format!("{:?}", tx.version()));
    view.txid = Some(tx.txid().to_string());
    view.consensus_branch_id = Some(format!("{:?}", tx.consensus_branch_id()).to_lowercase());
    view.lock_time = Some(tx.lock_time());
    view.expiry_height = Some(u32::from(tx.expiry_height()));

    let net = params.network_type();
    let mut transparent_out: i64 = 0;
    let mut shielded_output = false;
    // Net value leaving the shielded pools into the transparent pool. For a
    // fully-shielded tx this is the fee.
    let mut value_balance: i64 = 0;

    if let Some(t) = tx.transparent_bundle() {
        if !t.vin.is_empty() {
            let vin = t
                .vin
                .iter()
                .map(|i| TxIn {
                    prevout_txid: i.prevout().txid().to_string(),
                    prevout_index: i.prevout().n(),
                    script_sig: hex::encode(&i.script_sig().0.0),
                    sequence: i.sequence(),
                })
                .collect();
            view.inputs.push(PoolInput::Transparent { vin });
            view.prevouts = t
                .vin
                .iter()
                .map(|vin| OutPoint {
                    hash: *vin.prevout().hash(),
                    index: vin.prevout().n(),
                })
                .collect();
            view.input_total = InputTotal::Pending;
        }
        if !t.vout.is_empty() {
            let vout = t
                .vout
                .iter()
                .map(|o| TxOut {
                    value_zat: o.value().into_u64() as i64,
                    script_pubkey: hex::encode(&o.script_pubkey().0.0),
                    address: o
                        .recipient_address()
                        .map(|a| a.to_zcash_address(net).to_string()),
                })
                .collect();
            view.outputs.push(PoolOutput::Transparent { vout });
            view.vout_values = t.vout.iter().map(|o| o.value().into_u64() as i64).collect();
            transparent_out = view.vout_values.iter().sum();
        }
    }

    if let Some(s) = tx.sapling_bundle() {
        let vb = i64::from(*s.value_balance());
        value_balance += vb;
        if !s.shielded_spends().is_empty() {
            let anchor = hex::encode(s.shielded_spends()[0].anchor().to_bytes());
            let spends = s
                .shielded_spends()
                .iter()
                .map(|sd| SaplingSpend {
                    nullifier: hex::encode(sd.nullifier().0),
                    rk: hex::encode(<[u8; 32]>::from(*sd.rk())),
                })
                .collect();
            view.inputs.push(PoolInput::Sapling {
                anchor,
                value_balance_zat: vb,
                spends,
            });
        }
        if !s.shielded_outputs().is_empty() {
            shielded_output = true;
            let outputs = s
                .shielded_outputs()
                .iter()
                .map(|od| SaplingOutput {
                    cmu: hex::encode(od.cmu().to_bytes()),
                    ephemeral_key: hex::encode(od.ephemeral_key().0),
                    enc_ciphertext: Blob {
                        bytes: od.enc_ciphertext().len(),
                    },
                    out_ciphertext: Blob {
                        bytes: od.out_ciphertext().len(),
                    },
                })
                .collect();
            view.outputs.push(PoolOutput::Sapling { outputs });
        }
    }

    // Orchard and Ironwood share the action bundle type; the pool tag is the
    // only thing that differs on the way into the view.
    if let Some(o) = tx.orchard_bundle() {
        let (anchor, vb, flags, spends, outputs) = read_actions(o);
        value_balance += vb;
        shielded_output = true;
        view.inputs.push(PoolInput::Orchard {
            anchor,
            value_balance_zat: vb,
            flags,
            spends,
        });
        view.outputs.push(PoolOutput::Orchard { outputs });
    }
    if let Some(i) = tx.ironwood_bundle() {
        let (anchor, vb, flags, spends, outputs) = read_actions(i);
        value_balance += vb;
        shielded_output = true;
        view.inputs.push(PoolInput::Ironwood {
            anchor,
            value_balance_zat: vb,
            flags,
            spends,
        });
        view.outputs.push(PoolOutput::Ironwood { outputs });
    }

    // The moved value is the clear output total only when nothing shielded is
    // on the output side; any shielded output hides it.
    view.value = if shielded_output {
        Value::Shielded
    } else {
        Value::Clear(transparent_out)
    };

    // The fee is `inputs - outputs`. Its input-independent term is known now;
    // the transparent input total is added once resolved. A fully-shielded tx
    // has no transparent inputs, so this term already is the fee.
    view.fee_base = Some(value_balance - transparent_out);
    view
}

/// Read the fields the view wants off an Orchard-shaped bundle (Orchard or
/// Ironwood, same type). Returns anchor hex, value balance, flags, and the
/// per-action spend and output halves.
fn read_actions<A, V>(
    bundle: &orchard::Bundle<A, V>,
) -> (String, i64, Flags, Vec<ActionSpend>, Vec<ActionOutput>)
where
    A: orchard::bundle::Authorization,
    V: Copy,
    i64: From<V>,
{
    let anchor = hex::encode(bundle.anchor().to_bytes());
    let vb = i64::from(*bundle.value_balance());
    let flags = Flags {
        spends_enabled: bundle.flags().spends_enabled(),
        outputs_enabled: bundle.flags().outputs_enabled(),
    };
    let mut spends = Vec::new();
    let mut outputs = Vec::new();
    for act in bundle.actions().iter() {
        spends.push(ActionSpend {
            nullifier: hex::encode(act.nullifier().to_bytes()),
            rk: hex::encode(<[u8; 32]>::from(act.rk())),
        });
        let note = act.encrypted_note();
        outputs.push(ActionOutput {
            cmx: hex::encode(act.cmx().to_bytes()),
            ephemeral_key: hex::encode(note.epk_bytes),
            enc_ciphertext: Blob {
                bytes: note.enc_ciphertext.len(),
            },
            out_ciphertext: Blob {
                bytes: note.out_ciphertext.len(),
            },
        });
    }
    (anchor, vb, flags, spends, outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ChainParams {
        ChainParams::featurenet()
    }

    fn transparent_pending(prevouts: usize) -> ParsedTx {
        ParsedTx {
            txid: Some("ab".repeat(32)),
            version: Some("V5".into()),
            size: 300,
            consensus_branch_id: Some("nu5".into()),
            lock_time: Some(0),
            expiry_height: Some(0),
            inputs: vec![PoolInput::Transparent { vin: Vec::new() }],
            outputs: vec![PoolOutput::Transparent { vout: Vec::new() }],
            value: Value::Clear(400_000),
            vout_values: vec![400_000],
            prevouts: (0..prevouts)
                .map(|i| OutPoint {
                    hash: [i as u8; 32],
                    index: i as u32,
                })
                .collect(),
            input_total: InputTotal::Pending,
            // outputs 400_000, no shielded, so fee = resolved inputs - 400_000.
            fee_base: Some(-400_000),
            error: None,
        }
    }

    #[test]
    fn unparseable_bytes_yield_a_degraded_view() {
        let view = parse(
            &[0xff, 0x00, 0x13, 0x37],
            BlockHeight::from_u32(1),
            &params(),
        );
        assert!(view.txid.is_none());
        assert!(view.error.is_some());
        assert!(matches!(view.fee(), Fee::Unknown));
        assert!(matches!(view.value, Value::Unknown));
        assert!(matches!(view.input_total, InputTotal::None));
    }

    #[test]
    fn a_clear_value_serializes_with_value_zat() {
        let view = ParsedTx {
            txid: Some("ab".into()),
            version: Some("V5".into()),
            size: 100,
            consensus_branch_id: Some("nu5".into()),
            lock_time: Some(0),
            expiry_height: Some(0),
            inputs: vec![PoolInput::Transparent {
                vin: vec![TxIn {
                    prevout_txid: "cd".into(),
                    prevout_index: 0,
                    script_sig: "".into(),
                    sequence: u32::MAX,
                }],
            }],
            outputs: vec![PoolOutput::Transparent {
                vout: vec![TxOut {
                    value_zat: 500_000,
                    script_pubkey: "76a914".into(),
                    address: Some("t1abc".into()),
                }],
            }],
            value: Value::Clear(500_000),
            vout_values: vec![500_000],
            prevouts: Vec::new(),
            input_total: InputTotal::None,
            fee_base: None,
            error: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"value_zat\":500000"));
        assert!(json.contains("\"pool\":\"transparent\""));
        assert!(json.contains("\"consensus_branch_id\":\"nu5\""));
    }

    #[test]
    fn a_pending_input_total_yields_a_pending_fee() {
        let view = transparent_pending(2);
        assert!(matches!(view.fee(), Fee::Pending));
        assert!(view.summary().contains("fee unresolved"));
    }

    #[test]
    fn resolving_inputs_makes_the_fee_exact() {
        let mut view = transparent_pending(1);
        // 410_000 in, 400_000 out => 10_000 fee.
        view.resolve(Some(410_000));
        assert!(matches!(view.input_total, InputTotal::Known(410_000)));
        assert!(matches!(view.fee(), Fee::Known(10_000)));
        assert!(view.summary().contains("fee 0.0001 ZEC"));
    }

    #[test]
    fn a_failed_lookup_leaves_the_fee_unresolvable_not_wrong() {
        let mut view = transparent_pending(1);
        view.resolve(None);
        assert!(matches!(view.input_total, InputTotal::Unresolvable));
        assert!(matches!(view.fee(), Fee::Unresolvable));
    }

    #[test]
    fn resolve_is_inert_without_pending_inputs() {
        let mut view = parse(&[0xff, 0x00], BlockHeight::from_u32(0), &params());
        view.resolve(Some(1_000));
        assert!(matches!(view.input_total, InputTotal::None));
    }

    #[test]
    fn a_pending_tx_serializes_its_prevouts_and_states() {
        let json = serde_json::to_string(&transparent_pending(1)).unwrap();
        assert!(json.contains("\"inputs_value\":\"unresolved\""));
        assert!(json.contains("\"fee\":\"unresolved\""));
        assert!(json.contains("\"prevouts\":[{"));
    }

    #[test]
    fn a_resolved_tx_serializes_amounts() {
        let mut view = transparent_pending(1);
        view.resolve(Some(410_000));
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"inputs_value_zat\":410000"));
        assert!(json.contains("\"fee\":10000"));
    }

    #[test]
    fn format_zats_trims_trailing_zeros() {
        assert_eq!(format_zats(10_000), "0.0001 ZEC");
        assert_eq!(format_zats(100_000_000), "1 ZEC");
        assert_eq!(format_zats(0), "0 ZEC");
        assert_eq!(format_zats(1), "0.00000001 ZEC");
    }

    #[test]
    fn an_outpoint_reads_its_hash_in_display_order() {
        let mut hash = [0u8; 32];
        hash[0] = 0xaa;
        hash[31] = 0xff;
        let op = OutPoint { hash, index: 0 };
        // Display order is the reverse of wire order: last byte reads first.
        assert!(op.txid().starts_with("ff"));
        assert!(op.txid().ends_with("aa"));
    }

    #[test]
    fn summary_reads_as_one_human_line() {
        let mut view = transparent_pending(1);
        view.resolve(Some(410_000));
        let line = view.summary();
        assert!(line.contains("transparent → transparent"));
        assert!(line.contains("fee 0.0001 ZEC"));
    }

    #[test]
    fn summary_of_a_failed_parse_names_no_txid() {
        let line = parse(&[0xff, 0x00], BlockHeight::from_u32(1), &params()).summary();
        assert!(line.contains("(no txid)"));
        assert!(line.contains("unparseable"));
    }

    #[test]
    fn a_shielded_value_serializes_as_the_marker() {
        let view = ParsedTx {
            txid: Some("cd".into()),
            version: Some("V6".into()),
            size: 2000,
            consensus_branch_id: Some("nu6_3".into()),
            lock_time: Some(0),
            expiry_height: Some(0),
            inputs: vec![PoolInput::Orchard {
                anchor: "00".into(),
                value_balance_zat: 0,
                flags: Flags {
                    spends_enabled: true,
                    outputs_enabled: true,
                },
                spends: Vec::new(),
            }],
            outputs: vec![PoolOutput::Orchard {
                outputs: Vec::new(),
            }],
            value: Value::Shielded,
            vout_values: Vec::new(),
            prevouts: Vec::new(),
            input_total: InputTotal::None,
            fee_base: Some(20_000),
            error: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"value\":\"shielded\""));
        assert!(json.contains("\"fee\":20000"));
        assert!(json.contains("\"pool\":\"orchard\""));
    }
}
