//! Buffer address derivation.
//!
//! Each buffer is a per-token SPL token account that holds funds on behalf of
//! the settlement program. A buffer is the [associated token account][ata] of
//! the settlement state PDA (see [`crate::pda::state`]) for a given mint, so
//! there is exactly one buffer address per token and the state PDA — the single
//! authority over every buffer — is its SPL `owner` (token authority).
//!
//! The token account at this address is created through the standard
//! Associated Token Account program by the `CreateBuffer` instruction.
//!
//! [ata]: https://solana.com/docs/tokens/basics/associated-token-account

use solana_pubkey::Pubkey;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;

use crate::{pda::state::find_state_pda, SPL_TOKEN_PROGRAM_ID};

/// Derive the buffer address for the token `mint`: the settlement state PDA's
/// associated token account for that mint under the legacy SPL Token program.
pub fn find_buffer_pda(program_id: &Pubkey, mint: &Pubkey) -> Pubkey {
    let (state_pda, _) = find_state_pda(program_id);
    get_associated_token_address_with_program_id(&state_pda, mint, &SPL_TOKEN_PROGRAM_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_buffer_pda_is_state_pda_ata() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (state_pda, _) = find_state_pda(&program_id);

        assert_eq!(
            find_buffer_pda(&program_id, &mint),
            get_associated_token_address_with_program_id(&state_pda, &mint, &SPL_TOKEN_PROGRAM_ID),
            "buffer must be the state PDA's ATA for the mint",
        );
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn distinct_tokens_yield_distinct_pdas(
                program_id in any::<[u8; 32]>(),
                token1 in any::<[u8; 32]>(),
                token2 in any::<[u8; 32]>(),
            ) {
                prop_assume!(token1 != token2);
                let program_id = Pubkey::new_from_array(program_id);
                let pda1 = find_buffer_pda(&program_id, &Pubkey::new_from_array(token1));
                let pda2 = find_buffer_pda(&program_id, &Pubkey::new_from_array(token2));
                prop_assert_ne!(pda1, pda2);
            }
        }
    }
}
