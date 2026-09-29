//! Builder for the `RemoveSolver` instruction.

use cow_settlement_interface::{pda::state::STATE_PDA, Instruction, Pubkey};

/// Removes `solver` from the state PDA's solver list. Authorized by
/// `authority`; the freed rent is paid to `rent_recipient`.
pub struct RemoveSolver {
    pub program_id: Pubkey,
    pub authority: Pubkey,
    pub rent_recipient: Pubkey,
    pub solver: Pubkey,
}

impl From<RemoveSolver> for Instruction {
    fn from(builder: RemoveSolver) -> Self {
        cow_settlement_interface::instruction::remove_solver::RemoveSolver {
            program_id: builder.program_id,
            authority: builder.authority,
            rent_recipient: builder.rent_recipient,
            state_pda: STATE_PDA,
            solver: builder.solver,
        }
        .into()
    }
}
