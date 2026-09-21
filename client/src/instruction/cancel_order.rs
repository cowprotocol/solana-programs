//! Builder for the `CancelOrder` instruction.

use cow_settlement_interface::{
    data::intent::{BuyAsset, EncodedOrderIntent, OrderIntent},
    pda::order::find_order_pda,
    Instruction, Pubkey,
};

pub struct CancelOrder<'a> {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    /// The order to cancel. Its buy side is a [`BuyAsset`], matching how the
    /// order was described at creation; the encoding collapses both spellings
    /// to one mint.
    pub intent: &'a OrderIntent<BuyAsset>,
}

impl From<CancelOrder<'_>> for Instruction {
    fn from(builder: CancelOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
        cow_settlement_interface::instruction::cancel_order::CancelOrder {
            program_id: builder.program_id,
            owner: builder.owner,
            created_by: builder.created_by,
            order_pda,
            intent_bytes,
        }
        .into()
    }
}
