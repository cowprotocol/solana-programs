//! Builder for the `Initialize` instruction.

use cow_settlement_interface::{pda::state::find_state_pda, Instruction, Pubkey};

pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub manager: Pubkey,
    pub reclaim_authority: Pubkey,
}

impl From<Initialize> for Instruction {
    fn from(builder: Initialize) -> Self {
        let (state_pda, _bump) = find_state_pda(&builder.program_id);
        cow_settlement_interface::instruction::initialize::Initialize {
            program_id: builder.program_id,
            payer: builder.payer,
            state_pda,
            manager: builder.manager,
            reclaim_authority: builder.reclaim_authority,
        }
        .into()
    }
}
