//! Readers for the Rust sources the IDL describes.
//!
//! Everything here goes through `syn` and reports what it finds in the IDL
//! spec's own JSON grammar, so [`crate::generate`] can assemble it straight
//! into an IDL document. Lookups panic with the source path when the item the
//! IDL claims to describe isn't there to read.

use serde_json::{json, Value};

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

pub const CREATE_BUFFER_RS: Source = Source {
    display: "interface/src/instruction/create_buffer.rs",
    text: include_str!("../../../../interface/src/instruction/create_buffer.rs"),
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

pub const RECLAIM_ORDER_RS: Source = Source {
    display: "interface/src/instruction/reclaim_order.rs",
    text: include_str!("../../../../interface/src/instruction/reclaim_order.rs"),
};

pub const RECLAIM_BUFFER_RS: Source = Source {
    display: "interface/src/instruction/reclaim_buffer.rs",
    text: include_str!("../../../../interface/src/instruction/reclaim_buffer.rs"),
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

/// Translates a Rust type into the IDL spec's type grammar, or `None` for the
/// types that grammar can't name: borrowed accounts (`&'a A`), the repeated
/// groups the instructions carry as trailing accounts, and arrays whose length
/// is a named constant rather than a literal.
pub fn try_type_to_idl(ty: &syn::Type) -> Option<Value> {
    match ty {
        syn::Type::Path(path) => {
            let ident = path.path.get_ident()?.to_string();
            Some(match ident.as_str() {
                "Pubkey" => json!("pubkey"),
                "bool" | "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i32" | "i64"
                | "i128" => json!(ident),
                // Anything else is one of this crate's own types, which the IDL
                // carries as its own `types[]` entry and references by name.
                _ => json!({ "defined": { "name": ident } }),
            })
        }
        syn::Type::Array(array) => {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Int(len),
                ..
            }) = &array.len
            else {
                return None;
            };
            let len: u64 = len.base10_parse().ok()?;
            Some(json!({ "array": [try_type_to_idl(&array.elem)?, len] }))
        }
        _ => None,
    }
}

/// Translates a Rust type into the IDL spec's type grammar, panicking on
/// anything the grammar can't name. Used where the IDL is expected to describe
/// the type in full, so a translation that can't be made is a broken IDL rather
/// than a limit to work around.
pub fn type_to_idl(ty: &syn::Type, context: &str) -> Value {
    try_type_to_idl(ty)
        .unwrap_or_else(|| panic!("{context}: the IDL's type grammar can't name this type"))
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

/// A struct as an IDL `types[]` entry's `type`: `{"kind": "struct", "fields":
/// [...]}`, with the fields in declaration order, which is the order they're
/// laid out on the wire.
pub fn struct_type(rust_struct: &syn::ItemStruct, context: &str) -> Value {
    let fields: Vec<Value> = rust_struct
        .fields
        .iter()
        .map(|field| {
            let name = field_name(field, context);
            let ty = type_to_idl(&field.ty, &format!("{context}.{name}"));
            json!({ "name": name, "type": ty })
        })
        .collect();
    json!({ "kind": "struct", "fields": fields })
}

/// An enum as an IDL `types[]` entry's `type`: `{"kind": "enum", "variants":
/// [...]}`, with the variants in declaration order, which is the order the wire
/// discriminant counts in. The spec's `IdlEnumVariant` carries a name and
/// nothing else — no discriminant, since a variant's index is its wire byte,
/// and nowhere to put docs.
pub fn enum_type(rust_enum: &syn::ItemEnum) -> Value {
    let variants: Vec<Value> = rust_enum
        .variants
        .iter()
        .map(|variant| json!({ "name": variant.ident.to_string() }))
        .collect();
    json!({ "kind": "enum", "variants": variants })
}

/// One field's name.
pub fn field_name(field: &syn::Field, context: &str) -> String {
    field
        .ident
        .as_ref()
        .unwrap_or_else(|| panic!("{context} should have named fields"))
        .to_string()
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

/// Collapses doc lines into a single line, dropping the backtick and link
/// markup the IDL only carries in some places.
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
