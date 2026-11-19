//! Builder for the `CancelOrder` instruction.

use cow_settlement_interface::{
    data::intent::{EncodedOrderIntent, OrderIntent},
    pda::{order::find_order_pda, state::find_state_pda},
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
            state_pda: None,
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
            state_pda: None,
        }
        .into()
    }
}

/// [`CancelOrder`] for an order owned by the settlement state PDA, signed by
/// the settlement-owned-order `authority` in place of the owner. Like
/// [`CancelOrder`], it creates the order PDA already cancelled, funded by
/// `created_by`, if it doesn't exist yet.
pub struct CancelSettlementOwnedOrder<'a> {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub created_by: Pubkey,
    pub intent: &'a OrderIntent,
}

impl From<CancelSettlementOwnedOrder<'_>> for Instruction {
    fn from(builder: CancelSettlementOwnedOrder<'_>) -> Self {
        let encoded = EncodedOrderIntent::from(builder.intent);
        let (order_pda, _bump) = find_order_pda(&builder.program_id, &encoded.hash());
        let intent_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
        cow_settlement_interface::instruction::cancel_order::CancelOrder {
            program_id: builder.program_id,
            owner: builder.authority,
            created_by: builder.created_by,
            order_pda,
            intent_bytes: Some(intent_bytes),
            state_pda: Some(find_state_pda(&builder.program_id).0),
        }
        .into()
    }
}
