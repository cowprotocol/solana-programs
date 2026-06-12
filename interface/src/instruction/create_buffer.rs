//! `CreateBuffer` instruction builder.
//!
//! Allocates one or more per-token buffer PDAs (see [`crate::pda::buffer`]) as
//! SPL token accounts and initializes each with the settlement state PDA as its
//! token authority. Each token is identified by its `mint` account; the buffer
//! address must be the canonical PDA for that mint.

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::SettlementInstruction;

/// The SPL Token program. Buffers are created as token accounts owned by this
/// program.
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

/// Build a `CreateBuffer` instruction that creates one buffer per
/// `(buffer_pda, mint)` pair in `buffers`.
///
/// Each `buffer_pda` must be the canonical PDA returned by
/// [`crate::pda::buffer::find_buffer_pda`] for its `mint`; the program derives
/// the bump itself and rejects any other address. `payer` funds every new token
/// account's rent.
///
/// Wire format: `[discriminator]`, 1 byte. The tokens are implied by the
/// `mint` accounts, so no further data is needed.
/// Required accounts:
/// `[payer (W,S), token_program (R), system_program (R), (buffer_pda (W), mint (R))...]`.
/// The three shared accounts come first and are read positionally; the
/// per-buffer pairs follow. The system program only has to be present so the
/// `CreateAccount` CPI can dispatch; it isn't read by index.
pub fn create_buffers(
    program_id: &Pubkey,
    payer: &Pubkey,
    buffers: &[(Pubkey, Pubkey)],
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
        AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
    ];
    for (buffer_pda, mint) in buffers {
        accounts.push(AccountMeta::new(*buffer_pda, false));
        accounts.push(AccountMeta::new_readonly(*mint, false));
    }
    Instruction {
        program_id: *program_id,
        accounts,
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
        let ix = create_buffers(&program_id, &payer, &[(buffer_pda, mint)]);
        assert_eq!(
            ix.data,
            vec![SettlementInstruction::CreateBuffer.discriminator()]
        );
    }

    #[test]
    fn single_buffer_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_pda = Pubkey::new_from_array([3; 32]);
        let mint = Pubkey::new_from_array([4; 32]);
        let ix = create_buffers(&program_id, &payer, &[(buffer_pda, mint)]);

        assert_eq!(ix.accounts.len(), 5);
        // payer: writable, signer
        assert_eq!(ix.accounts[0].pubkey, payer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // token program: read-only
        assert_eq!(ix.accounts[1].pubkey, SPL_TOKEN_PROGRAM_ID);
        assert!(!ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
        // system program: read-only
        assert_eq!(ix.accounts[2].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
        // buffer_pda: writable, not signer (the program signs via PDA seeds)
        assert_eq!(ix.accounts[3].pubkey, buffer_pda);
        assert!(ix.accounts[3].is_writable);
        assert!(!ix.accounts[3].is_signer);
        // mint: read-only
        assert_eq!(ix.accounts[4].pubkey, mint);
        assert!(!ix.accounts[4].is_writable);
        assert!(!ix.accounts[4].is_signer);
    }

    #[test]
    fn multiple_buffers_append_pairs_after_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let buffer_a = Pubkey::new_from_array([3; 32]);
        let mint_a = Pubkey::new_from_array([4; 32]);
        let buffer_b = Pubkey::new_from_array([5; 32]);
        let mint_b = Pubkey::new_from_array([6; 32]);
        let ix = create_buffers(
            &program_id,
            &payer,
            &[(buffer_a, mint_a), (buffer_b, mint_b)],
        );

        // Three shared accounts followed by two (buffer, mint) pairs.
        assert_eq!(ix.accounts.len(), 3 + 2 * 2);
        assert_eq!(ix.accounts[3].pubkey, buffer_a);
        assert!(ix.accounts[3].is_writable);
        assert_eq!(ix.accounts[4].pubkey, mint_a);
        assert!(!ix.accounts[4].is_writable);
        assert_eq!(ix.accounts[5].pubkey, buffer_b);
        assert!(ix.accounts[5].is_writable);
        assert_eq!(ix.accounts[6].pubkey, mint_b);
        assert!(!ix.accounts[6].is_writable);
    }

    #[test]
    fn empty_buffers_has_only_shared_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let ix = create_buffers(&program_id, &payer, &[]);
        assert_eq!(ix.accounts.len(), 3);
    }
}
