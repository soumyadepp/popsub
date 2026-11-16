//! JSON Schema for the protocol, for client SDKs in other languages.
//!
//! Built from the same Rust types the server uses, so it can't drift from
//! what the server accepts and sends. The generated files live in
//! `docs/protocol/`, and `schema_files_match_the_types` fails when they are
//! out of date; `make schema` regenerates them. Only compiled with the
//! `schema` feature, so the crate's default build still depends only on
//! `serde`.
//!
//! - `client.schema.json` describes every frame a client sends (`ClientFrame`).
//! - `server.schema.json` describes every frame the server sends (`ServerFrame`).
//! - `close-codes.json` lists the WebSocket close codes, which aren't JSON
//!   frames and so aren't in either schema.

use schemars::{JsonSchema, Schema, generate::SchemaSettings};
use serde_json::{Value, json};

use crate::{ClientFrame, CloseCode, PROTOCOL_VERSION, ServerFrame};

/// The base URL the schemas' `$id`s are built on.
const SCHEMA_BASE_ID: &str = "https://github.com/soumyadepp/popsub/blob/main/docs/protocol";

/// The generated files, as `(file name, contents)`.
///
/// Returns:
///     Each file's name under `docs/protocol/` and its pretty-printed JSON,
///     ending in a newline.
pub fn schema_files() -> Vec<(&'static str, String)> {
    vec![
        (
            "client.schema.json",
            pretty(&frame_schema::<ClientFrame>(
                "client.schema.json",
                "Every frame a PopSub client sends.",
            )),
        ),
        (
            "server.schema.json",
            pretty(&frame_schema::<ServerFrame>(
                "server.schema.json",
                "Every frame a PopSub server sends.",
            )),
        ),
        ("close-codes.json", pretty(&close_codes())),
    ]
}

/// Builds the draft 2020-12 schema for one frame type, labelled with the
/// protocol version.
///
/// Args:
///     file: The file name, for the schema's `$id`.
///     description: What the schema describes.
///
/// Returns:
///     The schema as JSON.
fn frame_schema<T: JsonSchema>(file: &str, description: &str) -> Value {
    let generator = SchemaSettings::draft2020_12().into_generator();
    let mut schema: Schema = generator.into_root_schema_for::<T>();
    schema.insert("$id".to_string(), json!(format!("{SCHEMA_BASE_ID}/{file}")));
    schema.insert("description".to_string(), json!(description));
    schema.insert("x-protocol-version".to_string(), json!(PROTOCOL_VERSION));
    schema.to_value()
}

/// Lists every close code with its reason and whether to reconnect.
///
/// Returns:
///     The list as JSON.
fn close_codes() -> Value {
    let codes: Vec<Value> = CloseCode::ALL
        .iter()
        .map(|code| {
            json!({
                "code": code.code(),
                "reason": code.reason(),
                "should_reconnect": code.should_reconnect(),
            })
        })
        .collect();
    json!({
        "description": "WebSocket close codes a PopSub server sends, with whether a client should reconnect on its own.",
        "x-protocol-version": PROTOCOL_VERSION,
        "close_codes": codes,
    })
}

/// Pretty-prints JSON with a trailing newline, as the files are stored.
///
/// Args:
///     value: The JSON.
///
/// Returns:
///     The text.
fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON values always serialise");
    text.push('\n');
    text
}
