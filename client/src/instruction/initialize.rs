//! Builder for the `Initialize` instruction.

use cow_settlement_interface::{
    pda::{buffer::NATIVE_SOL_BUFFER_PDA, state::STATE_PDA},
    Instruction, Pubkey,
};
use solana_sdk_ids::bpf_loader_upgradeable;

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
            state_pda: STATE_PDA,
            native_sol_buffer: NATIVE_SOL_BUFFER_PDA,
            program_data: Pubkey::find_program_address(
                &[builder.program_id.as_ref()],
                &bpf_loader_upgradeable::ID,
            )
            .0,
            manager: builder.manager,
            solver_authority: builder.solver_authority,
            reclaim_authority: builder.reclaim_authority,
            settlement_owned_order_authority: builder.settlement_owned_order_authority,
        }
        .into()
    }
}
