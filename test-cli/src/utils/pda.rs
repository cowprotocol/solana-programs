//! PDA derivation under the CLI's configurable program ID.

use cow_settlement_client::cow_settlement_interface::{pda::state::STATE_PDA_SEEDS, Pubkey};

/// Derive the settlement state PDA under `program_id`. The interface's
/// `STATE_PDA` constant only holds for the declared program ID, while the CLI
/// can target a program deployed anywhere.
pub fn find_state_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&STATE_PDA_SEEDS, program_id).0
}
