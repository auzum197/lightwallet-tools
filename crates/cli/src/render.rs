//! Response rendering. The generated prost types carry no serde impls, so the
//! JSON path re-encodes each typed response and decodes it as a
//! `prost_reflect::DynamicMessage` against the workspace descriptor set, then
//! walks it into a `serde_json::Value` with two departures from canonical
//! protobuf JSON: `bytes` fields print as hex rather than base64, and every
//! field is emitted, defaults included (a debug tool that prints `{}` for a
//! successful `SendResponse` helps nobody).

use anyhow::{Context, Result};
use clap::ValueEnum;
use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MapKey, ReflectMessage, Value,
};
use std::fmt::Debug;

/// Txid and block-hash fields, keyed by (message, proto field name). These
/// cross the CLI boundary byte-reversed (display order, as explorers and
/// zcashd show them). Every other bytes field prints wire-order hex.
const DISPLAY_ORDER: &[(&str, &str)] = &[
    ("BlockID", "hash"),
    ("TxFilter", "hash"),
    ("CompactBlock", "hash"),
    ("CompactBlock", "prevHash"),
    ("CompactTx", "txid"),
    ("CompactTxIn", "prevoutTxid"),
    ("SubtreeRoot", "completingBlockHash"),
    ("GetAddressUtxosReply", "txid"),
];

#[derive(Clone, Copy, ValueEnum)]
pub enum OutputMode {
    /// Pretty JSON for unary responses, NDJSON for stream items.
    Json,
    /// Rust `Debug` formatting of the generated types, unprocessed.
    Debug,
}

/// Renders typed gRPC responses to stdout in the selected [`OutputMode`].
pub struct Renderer {
    pool: DescriptorPool,
    mode: OutputMode,
}

impl Renderer {
    pub fn new(mode: OutputMode) -> Result<Self> {
        // The overlay is canonical plus additive RPCs (`just mirror-check`
        // enforces it), so the crosslink descriptor set covers both variants.
        let pool = DescriptorPool::decode(lightwallet_proto_crosslink::FILE_DESCRIPTOR_SET)
            .context("decoding the workspace descriptor set")?;
        Ok(Self { pool, mode })
    }

    /// A unary response: pretty JSON, or `{:#?}` in debug mode.
    pub fn unary<M: prost::Message + Debug>(&self, msg: &M, type_name: &str) -> Result<()> {
        match self.mode {
            OutputMode::Json => emit(&serde_json::to_string_pretty(&self.json(msg, type_name)?)?),
            OutputMode::Debug => emit(&format!("{msg:#?}")),
        }
    }

    /// One stream item: a single NDJSON line, or `{:?}` in debug mode.
    pub fn item<M: prost::Message + Debug>(&self, msg: &M, type_name: &str) -> Result<()> {
        match self.mode {
            OutputMode::Json => emit(&serde_json::to_string(&self.json(msg, type_name)?)?),
            OutputMode::Debug => emit(&format!("{msg:?}")),
        }
    }

    fn json<M: prost::Message>(&self, msg: &M, type_name: &str) -> Result<serde_json::Value> {
        let full_name = format!("cash.z.wallet.sdk.rpc.{type_name}");
        let desc = self
            .pool
            .get_message_by_name(&full_name)
            .with_context(|| format!("{full_name} missing from the descriptor set"))?;
        let dynamic = DynamicMessage::decode(desc, msg.encode_to_vec().as_slice())
            .context("re-decoding the response for JSON output")?;
        Ok(message_json(&dynamic))
    }
}

/// Write one output line, exiting quietly when the downstream pipe has closed
/// (`lwcli ... | head` must not panic mid-stream).
pub fn emit(line: &str) -> Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    if let Err(e) = writeln!(stdout, "{line}") {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        return Err(e.into());
    }
    Ok(())
}

fn message_json(msg: &DynamicMessage) -> serde_json::Value {
    let desc = msg.descriptor();
    let fields = desc
        .fields()
        .map(|field| {
            // An absent singular message is null; absent scalars keep their
            // proto3 defaults so the output shape is stable.
            let singular_message =
                matches!(field.kind(), Kind::Message(_)) && !field.is_list() && !field.is_map();
            let json = if singular_message && !msg.has_field(&field) {
                serde_json::Value::Null
            } else {
                value_json(desc.name(), &field, &msg.get_field(&field))
            };
            (field.name().to_string(), json)
        })
        .collect();
    serde_json::Value::Object(fields)
}

fn value_json(message: &str, field: &FieldDescriptor, value: &Value) -> serde_json::Value {
    match value {
        Value::Bool(b) => (*b).into(),
        Value::I32(n) => (*n).into(),
        Value::I64(n) => (*n).into(),
        Value::U32(n) => (*n).into(),
        Value::U64(n) => (*n).into(),
        Value::F32(x) => f64::from(*x).into(),
        Value::F64(x) => (*x).into(),
        Value::String(s) => s.as_str().into(),
        Value::Bytes(b) => bytes_json(message, field.name(), b).into(),
        Value::EnumNumber(n) => enum_json(field, *n),
        Value::Message(m) => message_json(m),
        Value::List(items) => items
            .iter()
            .map(|item| value_json(message, field, item))
            .collect(),
        Value::Map(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| (map_key(key), value_json(message, field, value)))
                .collect(),
        ),
    }
}

fn bytes_json(message: &str, field: &str, bytes: &[u8]) -> String {
    if DISPLAY_ORDER.contains(&(message, field)) {
        hex::encode(bytes.iter().rev().copied().collect::<Vec<_>>())
    } else {
        hex::encode(bytes)
    }
}

fn enum_json(field: &FieldDescriptor, number: i32) -> serde_json::Value {
    match field.kind() {
        Kind::Enum(desc) => desc
            .get_value(number)
            .map(|v| v.name().into())
            .unwrap_or_else(|| number.into()),
        _ => number.into(),
    }
}

fn map_key(key: &MapKey) -> String {
    match key {
        MapKey::Bool(b) => b.to_string(),
        MapKey::I32(n) => n.to_string(),
        MapKey::I64(n) => n.to_string(),
        MapKey::U32(n) => n.to_string(),
        MapKey::U64(n) => n.to_string(),
        MapKey::String(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightwallet_proto_crosslink as proto;

    fn render_json<M: prost::Message>(msg: &M, type_name: &str) -> serde_json::Value {
        Renderer::new(OutputMode::Json)
            .unwrap()
            .json(msg, type_name)
            .unwrap()
    }

    #[test]
    fn defaults_are_emitted_not_dropped() {
        let json = render_json(&proto::SendResponse::default(), "SendResponse");
        assert_eq!(json["errorCode"], 0);
        assert_eq!(json["errorMessage"], "");
    }

    #[test]
    fn absent_singular_messages_render_null() {
        let json = render_json(&proto::BlockRange::default(), "BlockRange");
        assert!(json["start"].is_null());
        assert!(json["end"].is_null());
    }

    #[test]
    fn enums_render_by_name_with_integer_fallback() {
        let named = proto::GetSubtreeRootsArg {
            shielded_protocol: 1,
            ..Default::default()
        };
        assert_eq!(
            render_json(&named, "GetSubtreeRootsArg")["shieldedProtocol"],
            "orchard"
        );

        let unknown = proto::GetSubtreeRootsArg {
            shielded_protocol: 42,
            ..Default::default()
        };
        assert_eq!(
            render_json(&unknown, "GetSubtreeRootsArg")["shieldedProtocol"],
            42
        );
    }

    #[test]
    fn display_order_reverses_only_its_listed_fields() {
        let mut txid = vec![0u8; 32];
        txid[0] = 0xff;
        let tx = proto::CompactTx {
            txid,
            ..Default::default()
        };
        let rendered = render_json(&tx, "CompactTx")["txid"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            rendered.ends_with("ff"),
            "txid must print reversed: {rendered}"
        );

        let raw = proto::RawTransaction {
            data: vec![0xff, 0x00],
            height: 0,
        };
        assert_eq!(render_json(&raw, "RawTransaction")["data"], "ff00");
    }
}
