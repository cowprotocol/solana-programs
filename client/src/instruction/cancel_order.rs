//! Builder for the `CancelOrder` instruction.

use cow_settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::order::find_order_pda,
    Instruction, Pubkey,
};

pub struct CancelOrder<'a> {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
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
            intent_bytes: Some(intent_bytes),
        }
        .into()
    }
}

/// Alternative to [`CancelOrder`]: more efficient but it only lets the owner
/// cancel the order if its PDA already exists on-chain, reverting otherwise,  
pub struct CancelOutstandingOrder<'a> {
    pub program_id: Pubkey,
    pub owner: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
}

impl From<CancelOutstandingOrder<'_>> for Instruction {
    fn from(builder: CancelOutstandingOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        cow_settlement_interface::instruction::cancel_order::CancelOrder {
            program_id: builder.program_id,
            owner: builder.owner,
            created_by: builder.created_by,
            order_pda,
            intent_bytes: None,
        }
        .into()
    }
}
