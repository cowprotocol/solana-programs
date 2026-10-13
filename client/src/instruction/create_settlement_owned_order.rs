//! Builder for the `CreateSettlementOwnedOrder` instruction.

use cow_settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::{order::find_order_pda, state::find_state_pda},
    Instruction, Pubkey,
};

/// Places `intent` as an order owned by the settlement state PDA, gated by the
/// settlement-owned-order `authority`. `intent`'s owner must be the state PDA,
/// or the program rejects it; `created_by` funds the order PDA's rent.
pub struct CreateSettlementOwnedOrder<'a> {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
}

impl From<CreateSettlementOwnedOrder<'_>> for Instruction {
    fn from(builder: CreateSettlementOwnedOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
        cow_settlement_interface::instruction::create_settlement_owned_order::CreateSettlementOwnedOrder {
            program_id: builder.program_id,
            authority: builder.authority,
            created_by: builder.created_by,
            state_pda: find_state_pda(&builder.program_id).0,
            order_pda,
            intent_bytes,
        }
        .into()
    }
}
