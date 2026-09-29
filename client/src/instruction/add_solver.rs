//! Builder for the `AddSolver` instruction.

use cow_settlement_interface::{pda::state::STATE_PDA, Instruction, Pubkey};

/// Inserts `solver` into the state PDA's solver list. `manager` authorizes the
/// change and must be the current manager; `payer` funds the account's growth.
/// Both sign.
pub struct AddSolver {
    pub program_id: Pubkey,
    pub manager: Pubkey,
    pub payer: Pubkey,
    pub solver: Pubkey,
}

impl From<AddSolver> for Instruction {
    fn from(builder: AddSolver) -> Self {
        cow_settlement_interface::instruction::add_solver::AddSolver {
            program_id: builder.program_id,
            manager: builder.manager,
            payer: builder.payer,
            state_pda: STATE_PDA,
            solver: builder.solver,
        }
        .into()
    }
}
