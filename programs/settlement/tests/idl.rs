//! IDL correctness tests for `programs/settlement/idl/cow_settlement.json`.
//!
//! This program is a native Pinocchio program with a hand-written IDL (no
//! `anchor idl build`/shank step keeps it in sync), so these tests
//! cross-check the checked-in file against the Rust source it describes
//! instead of trusting it as-is.
//!
//! The vendored schema (`idl/schema/idl-spec-v0.1.0.json`) is a snapshot of
//! <https://raw.githubusercontent.com/solana-foundation/idl-spec/refs/heads/main/schema/v0.1.0.json>;
//! re-fetch it there if the spec version ever bumps.

mod common;

use std::collections::BTreeSet;

use serde_json::{json, Value};
use settlement_interface::{
    pda::{buffer::BUFFER_SEED, order::ORDER_SEED, SETTLEMENT_SEED},
    SettlementAccount, SettlementInstruction,
};

const IDL_JSON: &str = include_str!("../idl/cow_settlement.json");
const IDL_SCHEMA_JSON: &str = include_str!("../idl/schema/idl-spec-v0.1.0.json");
const INTERFACE_LIB_RS: &str = include_str!("../../../interface/src/lib.rs");
const INTENT_RS: &str = include_str!("../../../interface/src/data/intent.rs");
const ORDER_RS: &str = include_str!("../../../interface/src/data/order.rs");
const STATE_RS: &str = include_str!("../../../interface/src/data/state.rs");

fn idl() -> Value {
    serde_json::from_str(IDL_JSON).expect("IDL must be valid JSON")
}

fn find_item_in_idl<'a>(idl: &'a Value, type_name: &str, name: &str) -> Option<&'a Value> {
    idl[type_name]
        .as_array()?
        .iter()
        .find(|ix| ix["name"] == name)
}

fn doc_attr_text(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("doc") {
        return None;
    }
    let syn::Meta::NameValue(nv) = &attr.meta else {
        return None;
    };
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(s),
        ..
    }) = &nv.value
    else {
        return None;
    };
    Some(s.value().trim().to_string())
}

fn normalize_doc(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .replace('`', "")
}

// ---------------------------------------------------------------------------
// JSON validity and formatting
// ---------------------------------------------------------------------------

#[test]
fn idl_is_valid_json() {
    let _: Value = idl();
}

#[test]
fn idl_is_pretty_formatted() {
    let mut formatted = serde_json::to_string_pretty(&idl()).expect("IDL JSON should re-serialize");
    formatted.push('\n');
    assert_eq!(
        formatted, IDL_JSON,
        "IDL isn't canonically formatted; regenerate it with `serde_json::to_string_pretty` \
         plus a trailing newline"
    );
}

#[test]
fn idl_address_matches_declared_program_id() {
    let idl = idl();
    assert_eq!(
        idl["address"].as_str().expect("address must be a string"),
        settlement_interface::ID.to_string(),
        "IDL `address` must match the program id declared via declare_id! in interface/src/lib.rs"
    );
}

#[test]
fn idl_conforms_to_official_schema() {
    let schema: Value = serde_json::from_str(IDL_SCHEMA_JSON).expect("schema should be valid JSON");
    let validator = jsonschema::validator_for(&schema).expect("schema should compile");
    let instance = idl();
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "IDL fails schema validation:\n{}",
        errors.join("\n")
    );
}

fn pascal_to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn confirm_idl_match(idl: &Value, byte: u8, element_type: &str, idl_name: &str) {
    let idl_element = find_item_in_idl(idl, element_type, idl_name)
        .unwrap_or_else(|| panic!("IDL does not contain defined settlement element {idl_name}"));

    // confirm the discriminator matches
    let disc = idl_element["discriminator"]
        .as_array()
        .unwrap_or_else(|| panic!("discriminator for {idl_name} should be an array"));

    assert_eq!(
        disc.len(),
        1,
        "instruction discriminator {disc:?} for {idl_name} should be 1 byte"
    );
    assert_eq!(
        disc[0].as_u64(),
        Some(byte as u64),
        "instruction discriminator byte for {idl_name} doesn't match up with the code"
    );

    // confirm the docs match
    /*let docs = ix["docs"]
    .as_array()
    .expect("docs for {idl_name} should be an array");*/

    // TODO
}

#[test]
fn idl_matches_instruction_discriminators() {
    let idl = idl();
    for byte in 0u8..=255 {
        if let Ok(ix) = SettlementInstruction::try_from(byte) {
            confirm_idl_match(
                &idl,
                byte,
                "instructions",
                &pascal_to_snake(&format!("{ix:?}")),
            );
        }
    }
}

#[test]
fn idl_matches_account_discriminators() {
    let idl = idl();
    for byte in 0u8..=255 {
        if let Ok(account) = SettlementAccount::try_from(byte) {
            confirm_idl_match(&idl, byte, "accounts", &format!("{account:?}"));
        }
    }
}

/// Translates a Rust field type into the IDL spec's type grammar, so field
/// types can be compared as JSON. Panics on anything the program's data types
/// don't currently use.
fn rust_type_to_idl(ty: &syn::Type, context: &str) -> Value {
    match ty {
        syn::Type::Path(path) => {
            let ident = path
                .path
                .get_ident()
                .unwrap_or_else(|| panic!("{context}: expected a plain type name"))
                .to_string();
            match ident.as_str() {
                "Pubkey" => json!("pubkey"),
                "bool" | "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                | "i128" => json!(ident),
                // Anything else is one of this crate's own types, which the IDL
                // carries as its own `types[]` entry and references by name.
                _ => json!({ "defined": { "name": ident } }),
            }
        }
        syn::Type::Array(array) => {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(len),
                ..
            }) = &array.len
            else {
                panic!("{context}: array length must be an integer literal");
            };
            let len: u64 = len.base10_parse().expect("array length must be a u64");
            json!({ "array": [rust_type_to_idl(&array.elem, context), len] })
        }
        _ => panic!("{context}: unsupported field type"),
    }
}

/// Cross-checks one IDL `types[]` entry against the Rust struct it describes.
///
/// `idl_name` is passed separately because the two don't always agree:
/// `StateAccount` is called `SettlementState` in the IDL, matching the
/// `SettlementAccount` discriminator variant that names the account.
fn confirm_idl_types_entry(idl: &Value, rust_file_name: &str, rust_type_name: &str, idl_type_name: &str) {
    // load in the idl and rust type definitions
    let type_in_idl = idl["types"]
        .as_array()
        .expect("types must be an array")
        .iter()
        .find(|t| t["name"] == idl_type_name)
        .unwrap_or_else(|| panic!("IDL types[] must contain {idl_type_name}"));

    let file = syn::parse_file(rust_file_name).expect("Rust source must parse");
    let rust_struct = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(s) if s.ident == rust_type_name => Some(s),
            _ => None,
        })
        .unwrap_or_else(|| panic!("struct {rust_type_name} not found in Rust source"));

    // confirm the docs match
    let idl_docs: Vec<String> = type_in_idl["docs"]
        .as_array()
        .unwrap_or_else(|| panic!("docs should be an array for {rust_type_name}"))
        .iter()
        .map(|i| i.as_str().expect("doc item should be a string").to_string())
        .collect();

    let rust_docs: Vec<String> = rust_struct.attrs.iter().filter_map(doc_attr_text).collect();

    assert_eq!(
        idl_docs, rust_docs,
        "documentation between rust and IDL types should be the same for {rust_type_name} and {idl_type_name}"
    );

    // confirm fields match, both in name/order and in type
    let idl_fields: Vec<(String, Value)> = type_in_idl["type"]["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("struct type {idl_type_name} should have a fields array"))
        .iter()
        .map(|f| {
            (
                f["name"]
                    .as_str()
                    .expect("field name must be a string")
                    .to_string(),
                f["type"].clone(),
            )
        })
        .collect();

    let rust_fields: Vec<(String, Value)> = rust_struct
        .fields
        .iter()
        .map(|f| {
            let name = f
                .ident
                .as_ref()
                .unwrap_or_else(|| panic!("{idl_type_name} should have named fields"))
                .to_string();
            let idl_type = rust_type_to_idl(&f.ty, &format!("{idl_type_name}.{name}"));
            (name, idl_type)
        })
        .collect();

    assert_eq!(
        idl_fields, rust_fields,
        "{idl_type_name}'s IDL fields must match the Rust struct {idl_type_name} in name, order and type"
    );
}

#[test]
fn idl_matches_rust_types() {
    let idl = idl();

    confirm_idl_types_entry(&idl, INTENT_RS, "OrderIntent", "OrderIntent");
    confirm_idl_types_entry(&idl, ORDER_RS, "OrderAccount", "OrderAccount");
    confirm_idl_types_entry(&idl, STATE_RS, "StateAccount", "SettlementState");
}

#[test]
fn idl_matches_rust_errors() {
    let rust_file = syn::parse_file(INTERFACE_LIB_RS).expect("interface/src/lib.rs must parse");
    let rust_errors_type = rust_file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(e) if e.ident == "SettlementError" => Some(e),
            _ => None,
        })
        .expect("SettlementError enum must exist in interface/src/lib.rs");

    let idl = idl();
    for rust_err in &rust_errors_type.variants {
        let idl_err =
            find_item_in_idl(&idl, "errors", &rust_err.ident.to_string()).unwrap_or_else(|| {
                panic!(
                    "Settlement program error {} is not defined in IDL errors[]",
                    rust_err.ident
                )
            });

        // the idl error's "code" should match up
        let rust_code = match &rust_err.discriminant {
            Some((
                _,
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(i),
                    ..
                }),
            )) => i
                .base10_parse::<u32>()
                .expect("discriminant must be a u32 literal"),
            Some(_) => panic!("unexpected non-literal discriminant on {}", rust_err.ident),
            None => panic!("discriminant should be defined on {}", rust_err.ident),
        };

        let idl_code = idl_err["code"]
            .as_u64()
            .expect("code should be correct type");

        assert_eq!(
            idl_code, rust_code as u64,
            "IDL errors[] name={} should match the code in rust",
            rust_err.ident
        );

        // the idl error's "msg" should match up
        let rust_doc_lines: Vec<String> = rust_err.attrs.iter().filter_map(doc_attr_text).collect();
        let rust_msg = normalize_doc(&rust_doc_lines);

        let idl_msg = idl_err["msg"].as_str().expect("msg must be a string");

        assert_eq!(
            idl_msg, rust_msg,
            "IDL errors[] name={} should match informational msg",
            rust_err.ident
        );
    }
}

#[test]
fn idl_pda_seed_literals_match_pda_module() {
    let known: BTreeSet<&[u8]> = [SETTLEMENT_SEED, BUFFER_SEED, ORDER_SEED]
        .into_iter()
        .collect();

    fn collect_const_seeds(value: &Value, out: &mut Vec<Vec<u8>>) {
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
                    collect_const_seeds(v, out);
                }
            }
            Value::Array(arr) => arr.iter().for_each(|v| collect_const_seeds(v, out)),
            _ => {}
        }
    }

    let idl = idl();
    let mut found = Vec::new();
    collect_const_seeds(&idl["instructions"], &mut found);

    // Every const seed the IDL does declare must be a real seed constant. Not
    // every seed constant needs to show up: `order_pda`'s canonical seed
    // includes `sha256(intent)`, which the IDL can't express as a static
    // `pda` entry at all (documented in create_order's docs), so `ORDER_SEED`
    // legitimately never appears here.
    for seed in &found {
        assert!(
            known.contains(seed.as_slice()),
            "IDL PDA const seed {:?} doesn't match any seed constant in interface::pda",
            String::from_utf8_lossy(seed),
        );
    }
    assert!(
        found.iter().any(|s| s.as_slice() == SETTLEMENT_SEED),
        "expected SETTLEMENT_SEED to appear in some IDL PDA `seeds`",
    );
    assert!(
        found.iter().any(|s| s.as_slice() == BUFFER_SEED),
        "expected BUFFER_SEED to appear in some IDL PDA `seeds`",
    );
}
