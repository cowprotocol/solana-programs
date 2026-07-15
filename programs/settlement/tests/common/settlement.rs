//! The canonical settlement instruction positions shared by the integration
//! tests.
//!
//! Every settlement is a `[BeginSettle, FinalizeSettle]` instruction pair with
//! begin at [`BEGIN_INDEX`] and finalize at [`FINALIZE_INDEX`], each instruction
//! referencing its counterpart by that position. Callers assemble the pair as
//! `vec![begin.into(), finalize.into()]` and submit it with
//! [`send`](super::send).

/// Position of the `BeginSettle` instruction in the transactions built by the
/// tests in this crate.
pub const BEGIN_INDEX: u8 = 0;
/// Position of the `FinalizeSettle` instruction in those transactions.
pub const FINALIZE_INDEX: u8 = 1;
