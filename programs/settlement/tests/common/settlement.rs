//! Scaffolding for building `[BeginSettle, FinalizeSettle]` settlement pairs.

use settlement_client::instructions::{BeginSettle, FinalizedIntent, InitializedIntent};
use settlement_interface::Instruction;
use solana_sdk::pubkey::Pubkey;

/// Positions of the two instructions in the `[BeginSettle, FinalizeSettle]` pair
/// the settlement tests build: begin first, finalize right after it. Each
/// instruction points at the other through its `begin_ix_index`/`finalize_ix_index`.
pub const BEGIN_INDEX: u8 = 0;
pub const FINALIZE_INDEX: u8 = 1;

/// Build the `[begin, finalize]` instructions where `finalize` is a pre-built
/// `FinalizeSettle` at [`FINALIZE_INDEX`] and `begin` settles `orders` (with no
/// pulls) at [`BEGIN_INDEX`], the same orders the finalize is expected to push
/// to. Submit the result with [`send`](super::send).
pub fn build_settlement(
    program_id: &Pubkey,
    orders: &[FinalizedIntent],
    finalize: impl Into<Instruction>,
) -> Vec<Instruction> {
    let begin_orders: Vec<InitializedIntent> = orders
        .iter()
        .map(|order| InitializedIntent {
            intent: order.intent,
            pulls: &[],
        })
        .collect();
    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders: &begin_orders,
    };
    vec![begin.into(), finalize.into()]
}
