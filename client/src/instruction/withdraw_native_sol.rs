//! Builder for the `WithdrawNativeSol` instruction.

use cow_settlement_interface::{
    pda::{buffer::find_native_sol_buffer_pda, state::find_state_pda},
    Instruction, Pubkey,
};

/// Moves `amount` lamports out of the native SOL buffer to `recipient`.
/// Authorized by `authority`, the settlement-owned-order authority, which picks
/// `recipient` freely and may set it to itself.
pub struct WithdrawNativeSol {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub recipient: Pubkey,
    pub amount: u64,
}

impl From<WithdrawNativeSol> for Instruction {
    fn from(builder: WithdrawNativeSol) -> Self {
        cow_settlement_interface::instruction::withdraw_native_sol::WithdrawNativeSol {
            program_id: builder.program_id,
            state_pda: find_state_pda(&builder.program_id).0,
            authority: builder.authority,
            native_sol_buffer: find_native_sol_buffer_pda(&builder.program_id).0,
            recipient: builder.recipient,
            amount: builder.amount,
        }
        .into()
    }
}
