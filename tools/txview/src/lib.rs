//! Shared transaction projection for the lightwallet tools.
//!
//! The one place a serialized Zcash transaction becomes a readable view: txid,
//! version, size, per-pool inputs and outputs, moved value, and fee. Both the
//! inspector (`lwcli`) and the dashboard (`lwtui`) parse through [`parse`], so
//! they can never disagree about what a transaction is; they differ only in how
//! they present it. [`ParsedTx`] serializes to the canonical JSON shape, so the
//! tools' machine output stays identical too.

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
    /// Zatoshis, present only when computable from the tx alone (no transparent
    /// inputs, whose amounts live off-chain in the spent UTXOs).
    pub fee: Option<i64>,
    pub error: Option<String>,
}

impl ParsedTx {
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
        let fee = self.fee.map(format_zats).unwrap_or_else(|| "—".to_string());
        format!("{txid}  {version}  {pools}  {value}  fee {fee}")
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
        map.serialize_entry("fee", &self.fee)?;
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
        fee: None,
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

    let mut no_transparent_inputs = true;
    let mut transparent_out: i64 = 0;
    let mut shielded_output = false;
    if let Some(t) = tx.transparent_bundle() {
        if !t.vin.is_empty() {
            view.inputs.push(Pool::Transparent);
            no_transparent_inputs = false;
        }
        if !t.vout.is_empty() {
            view.outputs.push(Pool::Transparent);
            transparent_out = t.vout.iter().map(|o| u64::from(o.value()) as i64).sum();
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

    if no_transparent_inputs {
        view.fee = Some(value_balance - transparent_out);
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unparseable_bytes_yield_a_degraded_view() {
        let view = parse(&[0xff, 0x00, 0x13, 0x37], 0);
        assert!(view.txid.is_none());
        assert!(view.error.is_some());
        assert!(view.fee.is_none());
        assert!(matches!(view.value, Value::Unknown));
    }

    #[test]
    fn a_clear_value_serializes_with_value_zat() {
        let view = ParsedTx {
            txid: Some("ab".into()),
            version: Some("V5".into()),
            size: 100,
            inputs: vec![Pool::Transparent],
            outputs: vec![Pool::Transparent],
            value: Value::Clear(500_000),
            fee: None,
            error: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"value_zat\":500000"));
        assert!(json.contains("\"inputs\":[\"transparent\"]"));
    }

    #[test]
    fn format_zats_trims_trailing_zeros() {
        assert_eq!(format_zats(10_000), "0.0001 ZEC");
        assert_eq!(format_zats(100_000_000), "1 ZEC");
        assert_eq!(format_zats(0), "0 ZEC");
        assert_eq!(format_zats(1), "0.00000001 ZEC");
    }

    #[test]
    fn summary_reads_as_one_human_line() {
        let view = ParsedTx {
            txid: Some("ab".repeat(32)),
            version: Some("V5".into()),
            size: 500,
            inputs: vec![Pool::Transparent],
            outputs: vec![Pool::Transparent],
            value: Value::Clear(500_000),
            fee: Some(10_000),
            error: None,
        };
        let line = view.summary();
        assert!(line.contains("transparent → transparent"));
        assert!(line.contains("0.005 ZEC"));
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
            fee: Some(20_000),
            error: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"value\":\"shielded\""));
        assert!(json.contains("\"fee\":20000"));
    }
}
