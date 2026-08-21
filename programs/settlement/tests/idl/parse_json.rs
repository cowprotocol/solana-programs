//! Readers for the checked-in IDL, `programs/settlement/idl/cow_settlement.json`.
//!
//! Accessors panic naming the entry that went missing or came back the wrong
//! shape, so a malformed IDL fails where it's read rather than where it's
//! compared.

use std::{fmt, sync::LazyLock};

use serde_json::Value;

use crate::parse_rust::normalize_doc;

pub const IDL_JSON: &str = include_str!("../../idl/cow_settlement.json");
pub const SCHEMA_JSON: &str = include_str!("../../idl/schema/idl-spec-v0.1.0.json");

pub static IDL: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(IDL_JSON).expect("IDL must be valid JSON"));

/// A top-level array of the IDL. Naming the sections the tests read as a type
/// keeps a mistyped section from reading as a missing entry.
#[derive(Clone, Copy)]
pub enum Section {
    Instructions,
    Accounts,
    Types,
    Errors,
}

impl Section {
    /// The IDL key this section lives under.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instructions => "instructions",
            Self::Accounts => "accounts",
            Self::Types => "types",
            Self::Errors => "errors",
        }
    }
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The top-level `section[]` array.
pub fn section(section: Section) -> &'static [Value] {
    IDL[section.as_str()]
        .as_array()
        .unwrap_or_else(|| panic!("IDL {section}[] must be an array"))
}

/// Under the array for `section_name`, find the object where the `name` matches the given value.
/// Since each IDL section follows the same pattern where each section is an array of objects which contain a field `name`, this function
/// is useful for finding just about any item we need in the IDL file.
pub fn find_item(section_name: Section, name: &str) -> Option<&'static Value> {
    section(section_name)
        .iter()
        .find(|item| item["name"] == name)
}

/// An entry's `docs`, one [`normalize_doc`]d string per entry, so they compare
/// directly against what [`crate::parse_rust::docs`] reports. Absent docs read
/// as none at all; the caller compares against the Rust source to decide
/// whether that's wrong.
pub fn docs(item: &Value, context: &str) -> Vec<String> {
    let Some(docs) = item.get("docs") else {
        return Vec::new();
    };
    docs.as_array()
        .unwrap_or_else(|| panic!("docs for {context} should be an array"))
        .iter()
        .map(|doc| {
            let doc = doc
                .as_str()
                .unwrap_or_else(|| panic!("doc entry for {context} should be a string"));
            normalize_doc(&[doc])
        })
        .collect()
}

/// An entry's `discriminator` bytes.
pub fn discriminator(item: &Value, context: &str) -> Vec<u8> {
    item["discriminator"]
        .as_array()
        .unwrap_or_else(|| panic!("discriminator for {context} should be an array"))
        .iter()
        .map(|byte| {
            let byte = byte
                .as_u64()
                .unwrap_or_else(|| panic!("discriminator byte for {context} should be a number"));
            u8::try_from(byte)
                .unwrap_or_else(|_| panic!("discriminator byte for {context} should fit in a u8"))
        })
        .collect()
}

/// The `(name, type)` pairs of a struct `types[]` entry, in declaration order.
pub fn struct_fields(item: &Value, context: &str) -> Vec<(String, Value)> {
    item["type"]["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("struct type {context} should have a fields array"))
        .iter()
        .map(|field| {
            (
                field["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("field name in {context} must be a string"))
                    .to_string(),
                field["type"].clone(),
            )
        })
        .collect()
}

/// Every `{"kind": "const"}` seed byte string anywhere below `values`.
pub fn const_seeds(values: &[Value]) -> Vec<Vec<u8>> {
    fn collect(value: &Value, out: &mut Vec<Vec<u8>>) {
        match value {
            Value::Object(map) => {
                if map.get("kind").and_then(Value::as_str) == Some("const") {
                    if let Some(Value::Array(bytes)) = map.get("value") {
                        let decoded: Vec<u8> = bytes
                            .iter()
                            .map(|b| b.as_u64().expect("seed byte must be a number") as u8)
                            .collect();
                        out.push(decoded);
                    }
                }
                for v in map.values() {
                    collect(v, out);
                }
            }
            Value::Array(arr) => arr.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }

    let mut found = Vec::new();
    for value in values {
        collect(value, &mut found);
    }
    found
}
