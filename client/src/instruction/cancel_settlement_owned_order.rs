//! Builder for the `CancelSettlementOwnedOrder` instruction.

use cow_settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::{order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

/// Cancels the existing settlement-owned order for `intent`, gated by the
/// settlement-owned-order `authority`. The order PDA must already exist, or
/// the program rejects it.
pub struct CancelSettlementOwnedOrder<'a> {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub intent: &'a OrderIntent,
}

impl From<CancelSettlementOwnedOrder<'_>> for Instruction {
    fn from(builder: CancelSettlementOwnedOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        cow_settlement_interface::instruction::cancel_settlement_owned_order::CancelSettlementOwnedOrder {
            program_id: builder.program_id,
            authority: builder.authority,
            state_pda: find_state_pda(&builder.program_id).0,
            order_pda,
        }
        .into()
    }
}
