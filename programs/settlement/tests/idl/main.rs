//! IDL correctness tests for `programs/settlement/idl/cow_settlement.json`.
//!
//! This program is a native Pinocchio program with a hand-written IDL (no
//! `anchor idl build`/shank step keeps it in sync), so these tests cross-check
//! the checked-in file against the Rust source it describes.
//!
//! The cross-check runs in one direction. [`parse_rust`] reads the program's
//! source, [`generate`] assembles what it finds into a partial IDL — a document
//! shaped like the real one, carrying only the facts the source pins — and
//! [`superset`] asserts the checked-in file states all of them. Every run also
//! writes that generated document next to the build output, so a failure can be
//! read as a diff rather than as a list of assertions.
//!
//! The remaining tests here are the ones with no Rust-side counterpart at all:
//! the file has to be valid, canonically formatted JSON, and it has to satisfy
//! the IDL spec's own schema.

mod generate;
mod parse_rust;
mod superset;

use std::{fs, path::PathBuf, sync::LazyLock};

use serde_json::Value;

const IDL_JSON: &str = include_str!("../../idl/cow_settlement.json");
const SCHEMA_JSON: &str = include_str!("../../idl/schema/idl-spec-v0.1.0.json");

static IDL: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(IDL_JSON).expect("IDL must be valid JSON"));

/// Where [`idl_states_everything_the_rust_source_does`] leaves the document it
/// generated, for reading by hand when the assertion it drives fails.
///
/// `CARGO_TARGET_TMPDIR` is `target/tmp`, so its parent is the target directory
/// wherever cargo put it.
fn generated_idl_path() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("CARGO_TARGET_TMPDIR lives inside the target directory")
        .join("generated_cow_settlement_idl.json")
}

#[test]
fn idl_is_pretty_formatted() {
    let mut formatted = serde_json::to_string_pretty(&*IDL).expect("IDL JSON should re-serialize");
    formatted.push('\n');
    assert_eq!(
        formatted, IDL_JSON,
        "IDL isn't canonically formatted; regenerate it with `serde_json::to_string_pretty` \
         plus a trailing newline"
    );
}

#[test]
fn idl_conforms_to_official_schema() {
    let schema: Value = serde_json::from_str(SCHEMA_JSON).expect("schema should be valid JSON");

    // The bundled schema has to be the one `metadata.spec` claims to follow;
    // validating against some other spec's schema would prove nothing about
    // the version the IDL advertises.
    let spec = IDL["metadata"]["spec"]
        .as_str()
        .expect("metadata.spec must be a string");
    let schema_id = schema["$id"].as_str().expect("schema $id must be a string");
    assert!(
        schema_id.ends_with(&format!("v{spec}.json")),
        "IDL `metadata.spec` is {spec} but the schema it's validated against is {schema_id}"
    );

    let validator = jsonschema::validator_for(&schema).expect("schema should compile");
    let errors: Vec<String> = validator.iter_errors(&IDL).map(|e| e.to_string()).collect();
    assert!(
        errors.is_empty(),
        "IDL fails schema validation:\n{}",
        errors.join("\n")
    );
}

/// The one test that compares the two sides directly. Everything the Rust source says
/// about the program's interface has to be in the checked-in IDL, said the same
/// way; what the source can't say, the IDL is free to fill in.
#[test]
fn idl_matches_everything_generated_from_rust() {
    let generated = generate::partial_idl();

    let path = generated_idl_path();
    let mut json = serde_json::to_string_pretty(&generated).expect("generated IDL must serialize");
    json.push('\n');
    fs::write(&path, &json)
        .unwrap_or_else(|err| panic!("{} must be writable: {err}", path.display()));
    println!("generated IDL written to {}", path.display());

    superset::assert_superset(&generated, &IDL);
}

/// The IDL specification provides no way to specify the numeric value
/// of enum variants. If an enum is used as an input for an instruction or
/// account encoding (such as with OrderKind), a mismatch in the order
/// in the IDL would lead to mis-encoding. This test prevents them from
/// happening by making the ordering of variants sensitive.
#[test]
fn enum_type_discriminants_match_variant_order() {
    for (source, name) in generate::ENUM_TYPES {
        for (index, variant) in source.find_enum(name).variants.iter().enumerate() {
            let index = u64::try_from(index).expect("variant index should fit in a u64");
            if let Some(declared) = parse_rust::declared_discriminant(variant) {
                assert_eq!(
                    declared, index,
                    "{name}::{} declares discriminant {declared} but sits at index {index}; the \
                     IDL can only express a variant's wire value as its position",
                    variant.ident
                );
            }
        }
    }
}
