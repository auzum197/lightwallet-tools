//! Shared transaction projection for the lightwallet tools.
//!
//! The one place a serialized Zcash transaction becomes a readable view: txid,
//! version, size, per-pool inputs and outputs, moved value, and fee. Both the
//! inspector (`lwcli`) and the dashboard (`lwtui`) parse through [`parse`], so
//! they can never disagree about what a transaction is; they differ only in how
//! they present it. [`ParsedTx`] serializes to the canonical JSON shape, so the
//! tools' machine output stays identical too.
//!
//! A transaction's bytes carry its transparent outputs' values but not its
//! transparent inputs': an input names the funding output ([`OutPoint`]) it
//! spends, and the amount lives in that output. [`parse`] therefore leaves the
//! input total [`InputTotal::Pending`] and records the prevouts; a caller that
//! can follow them ([`ParsedTx::resolve`]) fills the total and turns the fee
//! exact.

use std::fmt;

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

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

/// The value a transaction moved, as far as the wire reveals it.
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
pub struct ParsedTx {
    /// Txid in display order (explorer order).
    pub txid: Option<String>,
    /// Transaction version, as `zcash_primitives` names it (e.g. `V5`, `V6`).
    pub version: Option<String>,
    /// Serialized size in bytes.
    pub size: usize,
    pub inputs: Vec<Pool>,
    pub outputs: Vec<Pool>,
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
        let pools = format!("{} → {}", pool_join(&self.inputs), pool_join(&self.outputs));
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

fn pool_join(pools: &[Pool]) -> String {
    if pools.is_empty() {
        return "none".to_string();
    }
    pools
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("+")
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

/// Read a serialized transaction into its projection. `branch` is the
/// deployment's current consensus branch id, needed to select v5/v6 rules.
pub fn parse(data: &[u8], branch: u32) -> ParsedTx {
    let mut view = ParsedTx {
        txid: None,
        version: None,
        size: data.len(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        value: Value::Unknown,
        vout_values: Vec::new(),
        prevouts: Vec::new(),
        input_total: InputTotal::None,
        fee_base: None,
        error: None,
    };

    // BranchId::try_from rejects an id no upgrade defines; fall back to Nu5 so a
    // stale/zero branch still attempts a parse rather than dropping every tx.
    let branch = BranchId::try_from(branch).unwrap_or(BranchId::Nu5);
    let tx = match Transaction::read(data, branch) {
        Ok(tx) => tx,
        Err(e) => {
            view.error = Some(format!("unparseable: {e}"));
            return view;
        }
    };

    view.version = Some(format!("{:?}", tx.version()));
    view.txid = Some(tx.txid().to_string());

    let mut transparent_out: i64 = 0;
    let mut shielded_output = false;
    if let Some(t) = tx.transparent_bundle() {
        if !t.vin.is_empty() {
            view.inputs.push(Pool::Transparent);
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
            view.outputs.push(Pool::Transparent);
            view.vout_values = t.vout.iter().map(|o| u64::from(o.value()) as i64).collect();
            transparent_out = view.vout_values.iter().sum();
        }
    }

    // Sum of shielded value balances: net value leaving the shielded pools into
    // the transparent value pool. For a fully-shielded tx this is the fee.
    let mut value_balance: i64 = 0;
    if let Some(s) = tx.sapling_bundle() {
        if !s.shielded_spends().is_empty() {
            view.inputs.push(Pool::Sapling);
        }
        if !s.shielded_outputs().is_empty() {
            view.outputs.push(Pool::Sapling);
            shielded_output = true;
        }
        value_balance += i64::from(s.value_balance());
    }
    // Orchard/Ironwood actions carry a spend and an output each, so the pool
    // sits on both sides whenever its bundle is present.
    if let Some(o) = tx.orchard_bundle() {
        view.inputs.push(Pool::Orchard);
        view.outputs.push(Pool::Orchard);
        shielded_output = true;
        value_balance += i64::from(o.value_balance());
    }
    if let Some(i) = tx.ironwood_bundle() {
        view.inputs.push(Pool::Ironwood);
        view.outputs.push(Pool::Ironwood);
        shielded_output = true;
        value_balance += i64::from(i.value_balance());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn transparent_pending(prevouts: usize) -> ParsedTx {
        ParsedTx {
            txid: Some("ab".repeat(32)),
            version: Some("V5".into()),
            size: 300,
            inputs: vec![Pool::Transparent],
            outputs: vec![Pool::Transparent],
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
        let view = parse(&[0xff, 0x00, 0x13, 0x37], 0);
        assert!(view.txid.is_none());
        assert!(view.error.is_some());
        assert!(matches!(view.fee(), Fee::Unknown));
        assert!(matches!(view.value, Value::Unknown));
        assert!(matches!(view.input_total, InputTotal::None));
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
        let mut view = parse(&[0xff, 0x00], 0);
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
        let line = parse(&[0xff, 0x00], 0).summary();
        assert!(line.contains("(no txid)"));
        assert!(line.contains("unparseable"));
    }

    #[test]
    fn a_shielded_value_serializes_as_the_marker() {
        let view = ParsedTx {
            txid: Some("cd".into()),
            version: Some("V6".into()),
            size: 2000,
            inputs: vec![Pool::Orchard, Pool::Ironwood],
            outputs: vec![Pool::Orchard, Pool::Ironwood],
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
    }
}
