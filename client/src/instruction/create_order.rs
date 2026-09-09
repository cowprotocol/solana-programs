//! Builder for the `CreateOrder` instruction.

use cow_settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::order::find_order_pda,
    Instruction, Pubkey,
};

pub struct CreateOrder<'a> {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
}

impl From<CreateOrder<'_>> for Instruction {
    fn from(builder: CreateOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
        cow_settlement_interface::instruction::create_order::CreateOrder {
            program_id: builder.program_id,
            owner: builder.owner,
            created_by: builder.created_by,
            order_pda,
            intent_bytes,
        }
        .into()
    }
}
