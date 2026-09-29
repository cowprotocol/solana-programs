//! Builder for the `Initialize` instruction.

use cow_settlement_interface::{pda::state::STATE_PDA, Instruction, Pubkey};

pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub manager: Pubkey,
    pub reclaim_authority: Pubkey,
    pub settlement_owned_order_authority: Pubkey,
}

impl From<Initialize> for Instruction {
    fn from(builder: Initialize) -> Self {
        cow_settlement_interface::instruction::initialize::Initialize {
            program_id: builder.program_id,
            payer: builder.payer,
            state_pda: STATE_PDA,
            manager: builder.manager,
            reclaim_authority: builder.reclaim_authority,
            settlement_owned_order_authority: builder.settlement_owned_order_authority,
        }
        .into()
    }
}
