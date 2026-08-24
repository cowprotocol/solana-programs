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

use cow_settlement_interface::{
    pda::{buffer::BUFFER_SEED, order::ORDER_SEED, SETTLEMENT_SEED},
    SettlementAccount, SettlementInstruction,
};
use parse_json::{Section, IDL};
use serde_json::{json, Value};

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

fn confirm_idl_match(idl_section: Section, idl_name: &str, discriminator_byte: u8) {
    let idl_element = parse_json::find_item(idl_section, idl_name)
        .unwrap_or_else(|| panic!("IDL does not contain defined settlement element {idl_name}"));

    // confirm the discriminator matches
    assert_eq!(
        parse_json::discriminator(idl_element, idl_name),
        vec![discriminator_byte],
        "IDL discriminator for {idl_name} should be the single byte the code uses"
    );

    // confirm the docs match: everything the Rust enum variant documents must
    // show up, in order, in the IDL element's docs. The IDL is allowed to say
    // more than the Rust source does; it carries notes about what its own
    // grammar can't express (`begin_settle`'s dynamically-shaped tail, for
    // one), which have no business being in the program's own docs.
    let idl_docs = parse_json::docs(idl_element, idl_name);
    let rust_docs = parse_rust::discriminator_variant_docs(idl_section, discriminator_byte);

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
        cow_settlement_interface::ID.to_string(),
        "IDL `address` must match the program id declared via declare_id! in interface/src/lib.rs"
    );
}

#[test]
fn idl_version_matches_cargo_package_version() {
    assert_eq!(
        IDL["metadata"]["version"]
            .as_str()
            .expect("metadata.version must be a string"),
        env!("CARGO_PKG_VERSION"),
        "IDL `metadata.version` must match the settlement program's cargo package version; \
         bumping the minor version also moves every PDA, so a stale value here hides that"
    );
}

#[test]
fn idl_conforms_to_official_schema() {
    let schema: Value =
        serde_json::from_str(parse_json::SCHEMA_JSON).expect("schema should be valid JSON");

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

/// Asserts `section[]` holds nothing beyond the `matched` entries a test just
/// cross-checked against the Rust source. Every check reads the Rust source and
/// looks for what it found in the IDL, never the other way around, so without
/// this an entry the IDL invented, or one a rename left behind, is never looked
/// at by anything.
fn confirm_no_extra_idl_entries(idl_section: Section, matched: usize) {
    assert_eq!(
        parse_json::section(idl_section).len(),
        matched,
        "IDL {idl_section}[] carries entries with no counterpart in the Rust source"
    );
}

#[test]
fn idl_matches_instruction_discriminators() {
    let mut matched = 0;
    for byte in 0u8..=255 {
        if let Ok(ix) = SettlementInstruction::try_from(byte) {
            confirm_idl_match(
                Section::Instructions,
                &pascal_to_snake(&format!("{ix:?}")),
                byte,
            );
            matched += 1;
        }
    }
    confirm_no_extra_idl_entries(Section::Instructions, matched);
}

#[test]
fn idl_matches_account_discriminators() {
    let mut matched = 0;
    for byte in 0u8..=255 {
        if let Ok(account) = SettlementAccount::try_from(byte) {
            confirm_idl_match(Section::Accounts, &format!("{account:?}"), byte);
            matched += 1;
        }
    }
    confirm_no_extra_idl_entries(Section::Accounts, matched);
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
    let type_in_idl = parse_json::find_item(Section::Types, idl_type_name)
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

/// Cross-checks one IDL `types[]` entry against the Rust enum it describes.
///
/// The spec's enum variants carry a name and nothing else: no discriminant,
/// since a variant's index is its wire byte, and nowhere to put docs. So the
/// variant names in order, plus the type's own docs, are the whole comparison.
fn confirm_idl_types_enum(rust_source: &parse_rust::Source, name: &str) {
    let type_in_idl = parse_json::find_item(Section::Types, name)
        .unwrap_or_else(|| panic!("IDL types[] must contain {name}"));
    let rust_enum = rust_source.find_enum(name);

    // docs are joined and compared as one block, as they are for structs
    assert_eq!(
        parse_json::docs(type_in_idl, name).join(" "),
        parse_rust::docs(&rust_enum.attrs).join(" "),
        "documentation between rust and IDL types should be the same for {name}"
    );

    assert_eq!(
        parse_json::enum_variants(type_in_idl, name),
        parse_rust::enum_variants(&rust_enum),
        "{name}'s IDL variants must match the Rust enum {name} in name and order"
    );

    // Position is all the IDL can say about a variant's wire value, so a Rust
    // variant that pins its own discriminant has to agree with its index.
    for (index, variant) in rust_enum.variants.iter().enumerate() {
        let index = u64::try_from(index).expect("variant index should fit in a u64");
        if let Some(declared) = parse_rust::declared_discriminant(variant) {
            assert_eq!(
                declared, index,
                "{name}::{} declares discriminant {declared} but sits at index {index}; the IDL \
                 can only express a variant's wire value as its position",
                variant.ident
            );
        }
    }
}

#[test]
fn idl_matches_rust_types() {
    // `(source, Rust name, IDL name)` of the struct types, and
    // `(source, name)` of the enum types. Everything the IDL defines has to be
    // listed here; the count is what catches a `types[]` entry the program has
    // no definition for.
    let structs = [
        (&parse_rust::INTENT_RS, "OrderIntent", "OrderIntent"),
        (&parse_rust::ORDER_RS, "OrderAccount", "OrderAccount"),
        (&parse_rust::STATE_RS, "StateAccount", "SettlementState"),
    ];
    let enums = [
        (&parse_rust::INTENT_RS, "OrderKind"),
        (&parse_rust::INTERFACE_LIB_RS, "Role"),
    ];

    for (rust_source, rust_type_name, idl_type_name) in structs {
        confirm_idl_types_entry(rust_source, rust_type_name, idl_type_name);
    }
    for (rust_source, name) in enums {
        confirm_idl_types_enum(rust_source, name);
    }

    confirm_no_extra_idl_entries(Section::Types, structs.len() + enums.len());
}

#[test]
fn idl_matches_rust_errors() {
    let rust_errors_type = parse_rust::INTERFACE_LIB_RS.find_enum("SettlementError");

    for rust_err in &rust_errors_type.variants {
        let idl_err = parse_json::find_item(Section::Errors, &rust_err.ident.to_string())
            .unwrap_or_else(|| {
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

    confirm_no_extra_idl_entries(Section::Errors, rust_errors_type.variants.len());
}

/// Cross-checks one IDL instruction's `args[]` against the builder struct whose
/// fields become the instruction data.
///
/// A builder carries the program id and the instruction's accounts too, so the
/// args are matched by name against a subset of its fields: each arg has to name
/// a real field, the two have to agree on type, and the args have to be ordered
/// the way the builder declares the fields, which is the order its
/// `From<Builder> for Instruction` writes them to the wire.
fn confirm_idl_args_match(
    rust_source: &parse_rust::Source,
    builder_name: &str,
    idl_instruction: &str,
) {
    let idl_element = parse_json::find_item(Section::Instructions, idl_instruction)
        .unwrap_or_else(|| panic!("IDL instructions[] must contain {idl_instruction}"));
    let builder = rust_source.find_struct(builder_name);
    let field_names = parse_rust::field_names(&builder, builder_name);

    let mut previous_arg: Option<(String, usize)> = None;
    for (arg_name, idl_type) in parse_json::args(idl_element, idl_instruction) {
        // panics naming the field if the builder has no such field at all
        assert_eq!(
            idl_type,
            parse_rust::field_type(&builder, &arg_name, builder_name),
            "{idl_instruction}'s arg {arg_name} must have the type {builder_name}.{arg_name} \
             declares"
        );

        let position = field_names
            .iter()
            .position(|field| *field == arg_name)
            .expect("field_type panics before this on a field the builder doesn't declare");
        if let Some((previous_name, previous_position)) = &previous_arg {
            assert!(
                *previous_position < position,
                "{idl_instruction} lists arg {arg_name} after {previous_name}, but {builder_name} \
                 declares them the other way around; args[] is in wire order"
            );
        }
        previous_arg = Some((arg_name, position));
    }
}

#[test]
fn idl_matches_instruction_args() {
    confirm_idl_args_match(&parse_rust::INITIALIZE_RS, "Initialize", "initialize");
    confirm_idl_args_match(&parse_rust::BEGIN_SETTLE_RS, "BeginSettle", "begin_settle");
    confirm_idl_args_match(
        &parse_rust::FINALIZE_SETTLE_RS,
        "FinalizeSettle",
        "finalize_settle",
    );
    confirm_idl_args_match(
        &parse_rust::TRANSFER_AUTHORITY_RS,
        "TransferAuthority",
        "transfer_authority",
    );

    // `create_order` is the one instruction whose builder doesn't mirror its
    // arg: `CreateOrder` takes the intent already encoded, as
    // `intent_bytes: [u8; EncodedOrderIntent::SIZE]`, where the IDL names the
    // decoded type. That the two describe the same bytes is what
    // `idl_matches_rust_types` checks of `OrderIntent` itself.
    let create_order = parse_json::find_item(Section::Instructions, "create_order")
        .expect("IDL instructions[] must contain create_order");
    assert_eq!(
        parse_json::args(create_order, "create_order"),
        vec![(
            "intent".to_string(),
            json!({ "defined": { "name": "OrderIntent" } })
        )],
        "create_order takes exactly one arg, the encoded OrderIntent"
    );
    let builder = parse_rust::CREATE_ORDER_RS.find_struct("CreateOrder");
    assert!(
        parse_rust::field_names(&builder, "CreateOrder").contains(&"intent_bytes".to_string()),
        "CreateOrder must still carry the encoded intent that create_order's `intent` arg describes"
    );

    // Instructions whose data is nothing but the discriminator byte.
    for idl_instruction in ["create_buffer", "reclaim_order", "reclaim_buffer"] {
        let idl_element = parse_json::find_item(Section::Instructions, idl_instruction)
            .unwrap_or_else(|| panic!("IDL instructions[] must contain {idl_instruction}"));
        assert!(
            parse_json::args(idl_element, idl_instruction).is_empty(),
            "{idl_instruction} carries no instruction data beyond its discriminator"
        );
    }
}

#[test]
fn idl_pda_seed_literals_match_pda_module() {
    let known: BTreeSet<&[u8]> = [SETTLEMENT_SEED, BUFFER_SEED, ORDER_SEED]
        .into_iter()
        .collect();

    let found = parse_json::const_seeds(parse_json::section(Section::Instructions));

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
