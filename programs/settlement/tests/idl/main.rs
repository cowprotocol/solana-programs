//! IDL correctness tests for `programs/settlement/idl/cow_settlement.json`.
//!
//! This program is a native Pinocchio program with a hand-written IDL (no
//! `anchor idl build`/shank step keeps it in sync), so these tests
//! cross-check the checked-in file against the Rust source it describes.
//!
//! [`parse_json`] reads the IDL and [`parse_rust`] reads the Rust source; the
//! tests here only compare what the two report.

mod parse_json;
mod parse_rust;

use std::collections::BTreeSet;

use parse_json::IDL;
use serde_json::Value;
use settlement_interface::{
    pda::{buffer::BUFFER_SEED, order::ORDER_SEED, SETTLEMENT_SEED},
    SettlementAccount, SettlementInstruction,
};

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

fn confirm_idl_match(byte: u8, element_type: &str, idl_name: &str) {
    let idl_element = parse_json::find_item(element_type, idl_name)
        .unwrap_or_else(|| panic!("IDL does not contain defined settlement element {idl_name}"));

    // confirm the discriminator matches
    assert_eq!(
        parse_json::discriminator(idl_element, idl_name),
        vec![byte],
        "IDL discriminator for {idl_name} should be the single byte the code uses"
    );

    // confirm the docs match: everything the Rust enum variant documents must
    // show up, in order, in the IDL element's docs. The IDL is allowed to say
    // more than the Rust source does; it carries notes about what its own
    // grammar can't express (`begin_settle`'s dynamically-shaped tail, for
    // one), which have no business being in the program's own docs.
    let idl_docs = parse_json::docs(idl_element, idl_name);
    let rust_docs = parse_rust::discriminator_variant_docs(element_type, byte);

    let mut unmatched_idl_docs = idl_docs.iter();
    for rust_doc in &rust_docs {
        assert!(
            unmatched_idl_docs.any(|idl_doc| idl_doc == rust_doc),
            "IDL docs for {idl_name} don't document what the Rust source does; missing (or \
             out of order) paragraph:\n{rust_doc}\nIDL docs are:\n{idl_docs:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// JSON validity and formatting
// ---------------------------------------------------------------------------

#[test]
fn idl_is_valid_json() {
    let _ = &*IDL;
}

#[test]
fn idl_is_pretty_formatted() {
    let mut formatted = serde_json::to_string_pretty(&*IDL).expect("IDL JSON should re-serialize");
    formatted.push('\n');
    assert_eq!(
        formatted,
        parse_json::IDL_JSON,
        "IDL isn't canonically formatted; regenerate it with `serde_json::to_string_pretty` \
         plus a trailing newline"
    );
}

#[test]
fn idl_address_matches_declared_program_id() {
    assert_eq!(
        IDL["address"].as_str().expect("address must be a string"),
        settlement_interface::ID.to_string(),
        "IDL `address` must match the program id declared via declare_id! in interface/src/lib.rs"
    );
}

#[test]
fn idl_conforms_to_official_schema() {
    let schema: Value =
        serde_json::from_str(parse_json::SCHEMA_JSON).expect("schema should be valid JSON");
    let validator = jsonschema::validator_for(&schema).expect("schema should compile");
    let errors: Vec<String> = validator.iter_errors(&IDL).map(|e| e.to_string()).collect();
    assert!(
        errors.is_empty(),
        "IDL fails schema validation:\n{}",
        errors.join("\n")
    );
}

#[test]
fn idl_matches_instruction_discriminators() {
    for byte in 0u8..=255 {
        if let Ok(ix) = SettlementInstruction::try_from(byte) {
            confirm_idl_match(byte, "instructions", &pascal_to_snake(&format!("{ix:?}")));
        }
    }
}

#[test]
fn idl_matches_account_discriminators() {
    for byte in 0u8..=255 {
        if let Ok(account) = SettlementAccount::try_from(byte) {
            confirm_idl_match(byte, "accounts", &format!("{account:?}"));
        }
    }
}

/// Cross-checks one IDL `types[]` entry against the Rust struct it describes.
///
/// `idl_name` is passed separately because the two don't always agree:
/// `StateAccount` is called `SettlementState` in the IDL, matching the
/// `SettlementAccount` discriminator variant that names the account.
fn confirm_idl_types_entry(
    rust_source: &parse_rust::Source,
    rust_type_name: &str,
    idl_type_name: &str,
) {
    // load in the idl and rust type definitions
    let type_in_idl = parse_json::find_item("types", idl_type_name)
        .unwrap_or_else(|| panic!("IDL types[] must contain {idl_type_name}"));
    let rust_struct = rust_source.find_struct(rust_type_name);

    // confirm the docs match. `types[]` carries one docs entry per source
    // line where `instructions[]` carries one per paragraph, so both sides are
    // compared as a single joined block: same prose, wrapping not load-bearing.
    assert_eq!(
        parse_json::docs(type_in_idl, idl_type_name).join(" "),
        parse_rust::docs(&rust_struct.attrs).join(" "),
        "documentation between rust and IDL types should be the same for {rust_type_name} and {idl_type_name}"
    );

    // confirm fields match, both in name/order and in type
    assert_eq!(
        parse_json::struct_fields(type_in_idl, idl_type_name),
        parse_rust::struct_fields(&rust_struct, idl_type_name),
        "{idl_type_name}'s IDL fields must match the Rust struct {idl_type_name} in name, order and type"
    );
}

#[test]
fn idl_matches_rust_types() {
    confirm_idl_types_entry(&parse_rust::INTENT_RS, "OrderIntent", "OrderIntent");
    confirm_idl_types_entry(&parse_rust::ORDER_RS, "OrderAccount", "OrderAccount");
    confirm_idl_types_entry(&parse_rust::STATE_RS, "StateAccount", "SettlementState");
}

#[test]
fn idl_matches_rust_errors() {
    let rust_errors_type = parse_rust::INTERFACE_LIB_RS.find_enum("SettlementError");

    for rust_err in &rust_errors_type.variants {
        let idl_err =
            parse_json::find_item("errors", &rust_err.ident.to_string()).unwrap_or_else(|| {
                panic!(
                    "Settlement program error {} is not defined in IDL errors[]",
                    rust_err.ident
                )
            });

        // the idl error's "code" should match up
        let idl_code = idl_err["code"]
            .as_u64()
            .expect("code should be correct type");

        assert_eq!(
            idl_code,
            parse_rust::discriminant(rust_err),
            "IDL errors[] name={} should match the code in rust",
            rust_err.ident
        );

        // the idl error's "msg" should match up
        let rust_msg = parse_rust::docs(&rust_err.attrs).join(" ");
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

    let found = parse_json::const_seeds(&IDL["instructions"]);

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
