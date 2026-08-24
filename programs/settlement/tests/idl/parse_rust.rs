//! Readers for the Rust sources the IDL describes.
//!
//! Everything here goes through `syn`, panicking with the source path when the
//! item the IDL claims to describe isn't there to compare against.

use serde_json::{json, Value};

use crate::parse_json::Section;

/// A Rust source file, compiled in so the tests parse the same text the
/// program does.
pub struct Source {
    /// Repo-relative path, for panic messages.
    display: &'static str,
    text: &'static str,
}

pub const INTERFACE_LIB_RS: Source = Source {
    display: "interface/src/lib.rs",
    text: include_str!("../../../../interface/src/lib.rs"),
};

pub const INTENT_RS: Source = Source {
    display: "interface/src/data/intent.rs",
    text: include_str!("../../../../interface/src/data/intent.rs"),
};

pub const ORDER_RS: Source = Source {
    display: "interface/src/data/order.rs",
    text: include_str!("../../../../interface/src/data/order.rs"),
};

pub const STATE_RS: Source = Source {
    display: "interface/src/data/state.rs",
    text: include_str!("../../../../interface/src/data/state.rs"),
};

pub const INITIALIZE_RS: Source = Source {
    display: "interface/src/instruction/initialize.rs",
    text: include_str!("../../../../interface/src/instruction/initialize.rs"),
};

pub const CREATE_ORDER_RS: Source = Source {
    display: "interface/src/instruction/create_order.rs",
    text: include_str!("../../../../interface/src/instruction/create_order.rs"),
};

pub const BEGIN_SETTLE_RS: Source = Source {
    display: "interface/src/instruction/settle/begin.rs",
    text: include_str!("../../../../interface/src/instruction/settle/begin.rs"),
};

pub const FINALIZE_SETTLE_RS: Source = Source {
    display: "interface/src/instruction/settle/finalize.rs",
    text: include_str!("../../../../interface/src/instruction/settle/finalize.rs"),
};

pub const TRANSFER_AUTHORITY_RS: Source = Source {
    display: "interface/src/instruction/transfer_authority.rs",
    text: include_str!("../../../../interface/src/instruction/transfer_authority.rs"),
};

impl Source {
    fn parse(&self) -> syn::File {
        syn::parse_file(self.text)
            .unwrap_or_else(|err| panic!("{} must parse: {err}", self.display))
    }

    /// The `enum name` this file declares.
    pub fn find_enum(&self, name: &str) -> syn::ItemEnum {
        self.parse()
            .items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Enum(e) if e.ident == name => Some(e),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name} enum must exist in {}", self.display))
    }

    /// The `struct name` this file declares.
    pub fn find_struct(&self, name: &str) -> syn::ItemStruct {
        self.parse()
            .items
            .into_iter()
            .find_map(|item| match item {
                syn::Item::Struct(s) if s.ident == name => Some(s),
                _ => None,
            })
            .unwrap_or_else(|| panic!("struct {name} not found in {}", self.display))
    }
}

/// The variant of `rust_enum` whose discriminant is `byte`.
pub fn variant_by_discriminant(rust_enum: &syn::ItemEnum, byte: u8) -> &syn::Variant {
    rust_enum
        .variants
        .iter()
        .find(|variant| discriminant(variant) == u64::from(byte))
        .unwrap_or_else(|| {
            panic!(
                "{} must have a variant with discriminant {byte}",
                rust_enum.ident
            )
        })
}

/// Translates a Rust field type into the IDL spec's type grammar, so field
/// types can be compared as JSON. Panics on anything the program's data types
/// don't currently use.
pub fn type_to_idl(ty: &syn::Type, context: &str) -> Value {
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
            json!({ "array": [type_to_idl(&array.elem, context), len] })
        }
        _ => panic!("{context}: unsupported field type"),
    }
}

/// The `= N` discriminant an enum variant declares, or `None` where it leans on
/// the implicit "one past the previous variant" value.
pub fn declared_discriminant(variant: &syn::Variant) -> Option<u64> {
    match &variant.discriminant {
        Some((
            _,
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(i),
                ..
            }),
        )) => Some(i.base10_parse().unwrap_or_else(|err| {
            panic!(
                "discriminant on {} must be an unsigned integer literal: {err}",
                variant.ident
            )
        })),
        Some(_) => panic!("unexpected non-literal discriminant on {}", variant.ident),
        None => None,
    }
}

/// The `= N` discriminant an enum variant declares. The discriminator enums the
/// IDL mirrors pin their wire values explicitly, so a missing one is a bug.
pub fn discriminant(variant: &syn::Variant) -> u64 {
    declared_discriminant(variant)
        .unwrap_or_else(|| panic!("discriminant should be defined on {}", variant.ident))
}

/// The `(name, type)` pairs of a struct's fields, in declaration order, with
/// each type already translated into the IDL's type grammar.
pub fn struct_fields(rust_struct: &syn::ItemStruct, context: &str) -> Vec<(String, Value)> {
    rust_struct
        .fields
        .iter()
        .map(|field| {
            let name = field
                .ident
                .as_ref()
                .unwrap_or_else(|| panic!("{context} should have named fields"))
                .to_string();
            let idl_type = type_to_idl(&field.ty, &format!("{context}.{name}"));
            (name, idl_type)
        })
        .collect()
}

/// The names of a struct's fields, in declaration order. Unlike
/// [`struct_fields`] this translates no types, so it reads structs carrying
/// fields the IDL's type grammar has no equivalent for.
pub fn field_names(rust_struct: &syn::ItemStruct, context: &str) -> Vec<String> {
    rust_struct
        .fields
        .iter()
        .map(|field| {
            field
                .ident
                .as_ref()
                .unwrap_or_else(|| panic!("{context} should have named fields"))
                .to_string()
        })
        .collect()
}

/// One named field's type, translated into the IDL's type grammar.
pub fn field_type(rust_struct: &syn::ItemStruct, name: &str, context: &str) -> Value {
    let field = rust_struct
        .fields
        .iter()
        .find(|field| field.ident.as_ref().is_some_and(|ident| ident == name))
        .unwrap_or_else(|| panic!("{context} should have a field named {name}"));
    type_to_idl(&field.ty, &format!("{context}.{name}"))
}

/// The variant names of an enum, in declaration order.
pub fn enum_variants(rust_enum: &syn::ItemEnum) -> Vec<String> {
    rust_enum
        .variants
        .iter()
        .map(|variant| variant.ident.to_string())
        .collect()
}

/// Unwraps rustdoc intra-doc links to the text they display: `[`Role`](Role)`
/// reads as `Role`. The IDL has no notion of a link target, so carrying one
/// there would only be Rust markup leaking into the published interface.
fn strip_doc_links(text: &str) -> String {
    /// The display text and the remainder past `[display](target)`, when `text`
    /// starts with a link whose two halves nest no brackets of their own.
    /// Anything else isn't a link and is left exactly as written.
    fn split_link(text: &str) -> Option<(&str, &str)> {
        let (display, after_display) = text.strip_prefix('[')?.split_once("](")?;
        let (target, after_link) = after_display.split_once(')')?;
        (!display.contains('[') && !target.contains('(')).then_some((display, after_link))
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        match split_link(rest) {
            Some((display, after_link)) => {
                out.push_str(display);
                rest = after_link;
            }
            None => {
                out.push('[');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapses doc lines into the single line the IDL carries them as, dropping
/// the backtick and link markup the IDL only carries in some places. Both this
/// module and [`crate::parse_json`] hand their doc text through here, so the two
/// sides are always compared in the same form.
pub fn normalize_doc<S: AsRef<str>>(lines: &[S]) -> String {
    strip_doc_links(
        &lines
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(" ")
            .replace('`', ""),
    )
}

/// Extract the doc text from a single attribute line in Rust
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
    // Doc comments start with spaces. We want to remove that,
    // but we want to keep extra alignment spacing if present.
    let doc = s.value();
    Some(doc.strip_prefix(' ').unwrap_or(&doc).to_string())
}

/// An item's doc comment, one [`normalize_doc`]d string per paragraph, with
/// blank doc lines separating the paragraphs.
pub fn docs(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    for line in attrs.iter().filter_map(doc_attr_text) {
        if line.is_empty() {
            if !paragraph.is_empty() {
                paragraphs.push(normalize_doc(&std::mem::take(&mut paragraph)));
            }
        } else {
            paragraph.push(line);
        }
    }
    if !paragraph.is_empty() {
        paragraphs.push(normalize_doc(&paragraph));
    }
    paragraphs
}

/// [`docs`] of the variant with discriminant `byte` in whichever discriminator
/// enum backs the given IDL `section`.
pub fn discriminator_variant_docs(section: Section, byte: u8) -> Vec<String> {
    let enum_name = match section {
        Section::Instructions => "SettlementInstruction",
        Section::Accounts => "SettlementAccount",
        other => panic!("no discriminator enum backs IDL {other}[]"),
    };

    let rust_enum = INTERFACE_LIB_RS.find_enum(enum_name);

    docs(&variant_by_discriminant(&rust_enum, byte).attrs)
}
