//! `CreateBuffer` instruction builder.
//!
//! Creates the per-token buffer (see [`crate::pda::buffer`]) — the settlement
//! state PDA's associated token account for a mint — by forwarding to the
//! standard Associated Token Account program. The buffer is owned by the SPL
//! Token program with the state PDA as its token authority. The token is
//! identified by the `mint` account; the buffer address must be the canonical
//! ATA for that mint.

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::{SettlementInstruction, ASSOCIATED_TOKEN_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID};

/// Build a `CreateBuffer` instruction for the buffer holding `mint`.
///
/// `buffer_pda` must be the canonical ATA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for `mint`; the program rejects any
/// other address (the ATA program re-derives and checks it). `state_pda` is the
/// settlement state PDA — the buffer's owner/authority — which the program also
/// re-derives and checks. `payer` funds the new token account's rent.
///
/// Wire format: `[discriminator = 4]`, 1 byte. The token is implied by the
/// `mint` account, so no further data is needed; the handler encodes the ATA
/// program's `Create` variant itself.
///
/// Required accounts:
/// `[payer (W,S), buffer_pda (W), state_pda (R), mint (R), system_program (R),
/// token_program (R), associated_token_program (R)]`. The first six match the
/// account order the Associated Token Account program's `Create` expects; the
/// associated token program is present so the CPI can dispatch.
pub fn create_buffer(
    program_id: &Pubkey,
    payer: &Pubkey,
    buffer_pda: &Pubkey,
    state_pda: &Pubkey,
    mint: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*buffer_pda, false),
            AccountMeta::new_readonly(*state_pda, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(ASSOCIATED_TOKEN_PROGRAM_ID, false),
        ],
        data: vec![SettlementInstruction::CreateBuffer.discriminator()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let state_pda = Pubkey::new_from_array([4; 32]);
        let mint = Pubkey::new_from_array([5; 32]);
        let ix = create_buffer(&program_id, &payer, &buffer_pda, &state_pda, &mint);
        assert_eq!(
            ix.data,
            vec![SettlementInstruction::CreateBuffer.discriminator()]
        );
    }

    #[test]
    fn instruction_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let state_pda = Pubkey::new_from_array([4; 32]);
        let mint = Pubkey::new_from_array([5; 32]);
        let ix = create_buffer(&program_id, &payer, &buffer_pda, &state_pda, &mint);

        assert_eq!(ix.accounts.len(), 7);
        // payer: writable, signer
        assert_eq!(ix.accounts[0].pubkey, payer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // buffer_pda: writable, not signer (the ATA program derives the address)
        assert_eq!(ix.accounts[1].pubkey, buffer_pda);
        assert!(ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
        // state_pda: read-only (the buffer's owner/authority)
        assert_eq!(ix.accounts[2].pubkey, state_pda);
        assert!(!ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
        // mint: read-only
        assert_eq!(ix.accounts[3].pubkey, mint);
        assert!(!ix.accounts[3].is_writable);
        assert!(!ix.accounts[3].is_signer);
        // system program: read-only
        assert_eq!(ix.accounts[4].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!ix.accounts[4].is_writable);
        assert!(!ix.accounts[4].is_signer);
        // token program: read-only
        assert_eq!(ix.accounts[5].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(!ix.accounts[5].is_writable);
        assert!(!ix.accounts[5].is_signer);
        // associated token program: read-only
        assert_eq!(ix.accounts[6].pubkey, ASSOCIATED_TOKEN_PROGRAM_ID);
        assert!(!ix.accounts[6].is_writable);
        assert!(!ix.accounts[6].is_signer);
    }
}
