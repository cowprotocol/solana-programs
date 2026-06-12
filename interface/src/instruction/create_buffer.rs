//! `CreateBuffer` instruction builder.
//!
//! Allocates the per-token buffer PDA (see [`crate::pda::buffer`]) as an SPL
//! token account and initializes it with the settlement state PDA as its token
//! authority. The token is identified by the `mint` account; the buffer address
//! must be the canonical PDA for that mint.

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::SettlementInstruction;

/// The SPL Token program. Buffers are created as token accounts owned by this
/// program.
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

/// Build a `CreateBuffer` instruction for the buffer holding `mint`.
///
/// `buffer_pda` must be the canonical PDA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for `mint`; the program derives the
/// bump itself and rejects any other address. `payer` funds the new token
/// account's rent.
///
/// Wire format: `[discriminator = 4]`, 1 byte. The token is implied by the
/// `mint` account, so no further data is needed.
/// Required accounts:
/// `[payer (W,S), buffer_pda (W), mint (R), token_program (R), system_program (R)]`.
/// The first four are read positionally. The system program only has to be
/// present so the `CreateAccount` CPI can dispatch; it isn't read by index.
pub fn create_buffer(
    program_id: &Pubkey,
    payer: &Pubkey,
    buffer_pda: &Pubkey,
    mint: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*buffer_pda, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
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
        let mint = Pubkey::new_from_array([4; 32]);
        let ix = create_buffer(&program_id, &payer, &buffer_pda, &mint);
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
        let mint = Pubkey::new_from_array([4; 32]);
        let ix = create_buffer(&program_id, &payer, &buffer_pda, &mint);

        assert_eq!(ix.accounts.len(), 5);
        // payer: writable, signer
        assert_eq!(ix.accounts[0].pubkey, payer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // buffer_pda: writable, not signer (the program signs via PDA seeds)
        assert_eq!(ix.accounts[1].pubkey, buffer_pda);
        assert!(ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
        // mint: read-only
        assert_eq!(ix.accounts[2].pubkey, mint);
        assert!(!ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
        // token program: read-only
        assert_eq!(ix.accounts[3].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(!ix.accounts[3].is_writable);
        assert!(!ix.accounts[3].is_signer);
        // system program: read-only
        assert_eq!(ix.accounts[4].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!ix.accounts[4].is_writable);
        assert!(!ix.accounts[4].is_signer);
    }
}
