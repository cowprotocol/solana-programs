//! Assembles the partial IDL the Rust source implies.
//!
//! Everything here reads the program's own source through [`crate::parse_rust`]
//! and emits it in the IDL spec's JSON grammar, producing a document shaped
//! exactly like `idl/cow_settlement.json` but carrying only the facts the
//! source pins. Whatever the source can't state — an instruction's account
//! list, the prose describing each argument, the `metadata` blurbs — is simply
//! left out, and [`crate::superset`] is what says the checked-in IDL has to
//! agree with everything that _is_ here.
//!
//! The tables below are the one place a name has to be written twice. They
//! exist because nothing in the Rust source says which file holds an
//! instruction's parsed input, which struct backs a `types[]` entry, or which
//! of an instruction's accounts the IDL derives as a PDA.

use cow_settlement_interface::{
    pda::{buffer::BUFFER_SEED, SETTLEMENT_SEED},
    SettlementInstruction,
};
use serde_json::{json, Map, Value};

use crate::parse_rust::{self, Source};

/// One seed of a PDA account, as the IDL spells it.
enum Seed {
    /// Bytes pinned by a constant in `interface::pda`.
    Const(&'static [u8]),
    /// Another of the instruction's accounts, named the way the IDL names it.
    Account(&'static str),
}

impl Seed {
    fn to_idl(&self) -> Value {
        match self {
            Self::Const(bytes) => json!({ "kind": "const", "value": bytes }),
            Self::Account(path) => json!({ "kind": "account", "path": path }),
        }
    }
}

/// The canonical settlement state PDA, seeded by the version-stamped prefix
/// alone.
const STATE_PDA: &[Seed] = &[Seed::Const(SETTLEMENT_SEED)];

/// A per-token buffer PDA. The IDL can only declare the guaranteed index-0
/// buffer of the unbounded run an instruction actually accepts, so the mint it
/// derives from is `mint_0`.
const BUFFER_PDA_0: &[Seed] = &[
    Seed::Const(SETTLEMENT_SEED),
    Seed::Account("mint_0"),
    Seed::Const(BUFFER_SEED),
];

/// What the Rust source doesn't say about one instruction.
struct Instruction {
    /// The discriminator variant naming it. The IDL calls the instruction by
    /// this name in `snake_case`.
    variant: SettlementInstruction,
    /// The file declaring `<variant>Input`, the struct [`args`] reads.
    input: &'static Source,
    /// The accounts the IDL declares a `pda` for, and the seeds that PDA is
    /// derived from. Accounts without one aren't listed: nothing in the Rust
    /// source pins the name the IDL gives them.
    pda_accounts: &'static [(&'static str, &'static [Seed])],
}

const INSTRUCTIONS: &[Instruction] = &[
    Instruction {
        variant: SettlementInstruction::Initialize,
        input: &parse_rust::INITIALIZE_RS,
        pda_accounts: &[("state_pda", STATE_PDA)],
    },
    Instruction {
        variant: SettlementInstruction::CreateBuffer,
        input: &parse_rust::CREATE_BUFFER_RS,
        pda_accounts: &[("buffer_pda_0", BUFFER_PDA_0)],
    },
    Instruction {
        variant: SettlementInstruction::CreateOrder,
        input: &parse_rust::CREATE_ORDER_RS,
        // `order_pda`'s canonical seeds include `sha256(intent)`, which the IDL
        // has no `seeds` kind for; create_order's docs say so instead.
        pda_accounts: &[],
    },
    Instruction {
        variant: SettlementInstruction::BeginSettle,
        input: &parse_rust::BEGIN_SETTLE_RS,
        // `state_pda` is passed as a plain account here rather than derived:
        // BeginSettle checks it against the canonical address itself.
        pda_accounts: &[],
    },
    Instruction {
        variant: SettlementInstruction::FinalizeSettle,
        input: &parse_rust::FINALIZE_SETTLE_RS,
        pda_accounts: &[],
    },
    Instruction {
        variant: SettlementInstruction::ReclaimOrder,
        input: &parse_rust::RECLAIM_ORDER_RS,
        pda_accounts: &[],
    },
    Instruction {
        variant: SettlementInstruction::ReclaimBuffer,
        input: &parse_rust::RECLAIM_BUFFER_RS,
        pda_accounts: &[("state_pda", STATE_PDA), ("buffer_pda_0", BUFFER_PDA_0)],
    },
    Instruction {
        variant: SettlementInstruction::TransferAuthority,
        input: &parse_rust::TRANSFER_AUTHORITY_RS,
        pda_accounts: &[("state_pda", STATE_PDA)],
    },
];

/// The struct types the IDL defines, as `(source, Rust name, IDL name)`. The
/// two names don't always agree: `StateAccount` is called `SettlementState` in
/// the IDL, matching the `SettlementAccount` variant that names the account.
const STRUCT_TYPES: &[(&Source, &str, &str)] = &[
    (&parse_rust::ORDER_RS, "OrderAccount", "OrderAccount"),
    (&parse_rust::STATE_RS, "StateAccount", "SettlementState"),
    (&parse_rust::INTENT_RS, "OrderIntent", "OrderIntent"),
];

/// The enum types the IDL defines, as `(source, name)`.
pub const ENUM_TYPES: &[(&Source, &str)] = &[
    (&parse_rust::INTENT_RS, "OrderKind"),
    (&parse_rust::INTERFACE_LIB_RS, "Role"),
];

/// The enum whose variants are the IDL's `errors[]`.
const ERRORS: &str = "SettlementError";

/// `BeginSettle` reads as `begin_settle`: the IDL names instructions the way
/// Rust names functions, where the discriminator enum names them as variants.
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

/// Generate an incomplete IDL document based on the Rust source.
pub fn partial_idl() -> Value {
    json!({
        "address": cow_settlement_interface::ID.to_string(),
        "metadata": {
            // Bumping the minor version also moves every PDA, so a stale value
            // here hides that.
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": instructions(),
        "accounts": accounts(),
        "types": types(),
        "errors": errors(),
    })
}

/// One entry per `SettlementInstruction` variant, in discriminator order.
fn instructions() -> Vec<Value> {
    discriminator_variants(&parse_rust::INTERFACE_LIB_RS.find_enum("SettlementInstruction"))
        .map(|(byte, variant)| {
            let instruction = INSTRUCTIONS
                .iter()
                .find(|instruction| instruction.variant.discriminator() == byte)
                .unwrap_or_else(|| {
                    panic!(
                        "SettlementInstruction::{} is missing from INSTRUCTIONS",
                        variant.ident
                    )
                });

            let mut entry = Map::new();
            entry.insert(
                "name".into(),
                json!(pascal_to_snake(&variant.ident.to_string())),
            );
            insert_docs(&mut entry, parse_rust::docs(&variant.attrs));
            entry.insert("discriminator".into(), json!([byte]));
            if !instruction.pda_accounts.is_empty() {
                entry.insert("accounts".into(), pda_accounts(instruction));
            }
            entry.insert("args".into(), args(instruction, &variant.ident.to_string()));
            Value::Object(entry)
        })
        .collect()
}

/// The `accounts[]` entries an instruction derives as PDAs, each carrying only
/// its name and its seeds.
fn pda_accounts(instruction: &Instruction) -> Value {
    let accounts: Vec<Value> = instruction
        .pda_accounts
        .iter()
        .map(|(name, seeds)| {
            let seeds: Vec<Value> = seeds.iter().map(Seed::to_idl).collect();
            json!({ "name": name, "pda": { "seeds": seeds } })
        })
        .collect();
    Value::Array(accounts)
}

/// An instruction's `args[]`, read off its `<Variant>Input` struct.
///
/// That struct is the closest thing the source has to `args[]`: its fields are
/// what a handler gets after parsing, holding the borrowed accounts (`&'a A`)
/// and the trailing repeated groups next to the values the instruction data
/// carries, in the order the data carries them. Dropping every field whose type
/// the IDL's grammar can't name leaves exactly the arguments — with one
/// exception, [`arg_alias`].
fn args(instruction: &Instruction, variant: &str) -> Value {
    let input_name = format!("{variant}Input");
    let input = instruction.input.find_struct(&input_name);

    let args: Vec<Value> = input
        .fields
        .iter()
        .filter_map(|field| {
            let name = parse_rust::field_name(field, &input_name);
            let (name, ty) = match arg_override(instruction.variant, &name) {
                Some(aliased) => aliased,
                None => (name, parse_rust::try_type_to_idl(&field.ty)?),
            };
            Some(json!({ "name": name, "type": ty }))
        })
        .collect();
    Value::Array(args)
}

/// In cases where the IDL needs to differ from the rust code, an override can be set here.
fn arg_override(variant: SettlementInstruction, field: &str) -> Option<(String, Value)> {
    match (variant, field) {
        (SettlementInstruction::CreateOrder, "intent_bytes") => Some((
            "intent".to_string(),
            json!({ "defined": { "name": "OrderIntent" } }),
        )),
        _ => None,
    }
}

/// One entry per `SettlementAccount` variant, in discriminator order.
fn accounts() -> Vec<Value> {
    discriminator_variants(&parse_rust::INTERFACE_LIB_RS.find_enum("SettlementAccount"))
        .map(|(byte, variant)| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(variant.ident.to_string()));
            insert_docs(&mut entry, parse_rust::docs(&variant.attrs));
            entry.insert("discriminator".into(), json!([byte]));
            Value::Object(entry)
        })
        .collect()
}

/// One `types[]` entry per struct and enum in the tables above.
fn types() -> Vec<Value> {
    let structs = STRUCT_TYPES.iter().map(|(source, rust_name, idl_name)| {
        let rust_struct = source.find_struct(rust_name);
        type_entry(
            idl_name,
            parse_rust::docs(&rust_struct.attrs),
            parse_rust::struct_type(&rust_struct, rust_name),
        )
    });
    let enums = ENUM_TYPES.iter().map(|(source, name)| {
        let rust_enum = source.find_enum(name);
        type_entry(
            name,
            parse_rust::docs(&rust_enum.attrs),
            parse_rust::enum_type(&rust_enum),
        )
    });
    structs.chain(enums).collect()
}

fn type_entry(idl_name: &str, docs: Vec<String>, ty: Value) -> Value {
    let mut entry = Map::new();
    entry.insert("name".into(), json!(idl_name));
    insert_docs(&mut entry, docs);
    entry.insert("type".into(), ty);
    Value::Object(entry)
}

/// One `errors[]` entry per [`ERRORS`] variant. A variant's discriminant is the
/// `ProgramError::Custom` code the program returns, and its doc comment is the
/// message the IDL publishes for that code.
fn errors() -> Vec<Value> {
    parse_rust::INTERFACE_LIB_RS
        .find_enum(ERRORS)
        .variants
        .iter()
        .map(|variant| {
            json!({
                "code": parse_rust::discriminant(variant),
                "name": variant.ident.to_string(),
                "msg": parse_rust::normalize_doc(&parse_rust::docs(&variant.attrs)),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The variants of a discriminator enum paired with their wire byte, in
/// discriminator order.
fn discriminator_variants(
    rust_enum: &syn::ItemEnum,
) -> impl Iterator<Item = (u8, &syn::Variant)> + '_ {
    let mut variants: Vec<(u8, &syn::Variant)> = rust_enum
        .variants
        .iter()
        .map(|variant| {
            let byte = parse_rust::discriminant(variant);
            let byte =
                u8::try_from(byte).unwrap_or_else(|_| panic!("{} must fit in a u8", variant.ident));
            (byte, variant)
        })
        .collect();
    variants.sort_by_key(|(byte, _)| *byte);
    variants.into_iter()
}

/// Records `docs` on an entry, leaving the key out entirely when the Rust
/// source documents nothing. An empty `docs` would claim the IDL must say
/// nothing either, which is the opposite of what a missing doc comment means.
fn insert_docs(entry: &mut Map<String, Value>, docs: Vec<String>) {
    if !docs.is_empty() {
        entry.insert("docs".into(), json!(docs));
    }
}
