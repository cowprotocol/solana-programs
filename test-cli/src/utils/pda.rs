//! PDA derivation under the CLI's configurable program ID.

use cow_settlement_client::cow_settlement_interface::{pda::state, Pubkey};

/// The settlement state PDA under `program_id`.
pub fn find_state_pda(program_id: &Pubkey) -> Pubkey {
    state::find_state_pda(program_id).0
}
