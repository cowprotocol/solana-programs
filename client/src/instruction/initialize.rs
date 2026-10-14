//! Builder for the `Initialize` instruction.

use cow_settlement_interface::{
    pda::{buffer::find_native_sol_buffer_pda, state::find_state_pda},
    Instruction, Pubkey,
};

pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub manager: Pubkey,
    pub solver_authority: Pubkey,
    pub reclaim_authority: Pubkey,
    pub settlement_owned_order_authority: Pubkey,
}

impl From<Initialize> for Instruction {
    fn from(builder: Initialize) -> Self {
        cow_settlement_interface::instruction::initialize::Initialize {
            program_id: builder.program_id,
            payer: builder.payer,
            state_pda: find_state_pda(&builder.program_id).0,
            native_sol_buffer: find_native_sol_buffer_pda(&builder.program_id).0,
            manager: builder.manager,
            solver_authority: builder.solver_authority,
            reclaim_authority: builder.reclaim_authority,
            settlement_owned_order_authority: builder.settlement_owned_order_authority,
        }
        .into()
    }
}
