//! `Initialize` instruction builder.
//!
//! Allocates the singleton settlement state PDA (see [`crate::pda::state`]).

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::SettlementInstruction;

/// Build an `Initialize` instruction.
///
/// `payer` funds the new account's rent and signs. It is meant to be the
/// transaction's fee payer: the state is created once at deployment and never
/// deallocated, so there's no need for a dedicated funding account separate
/// from whoever pays for the deployment transaction.
///
/// `state_pda` must be the canonical PDA returned by
/// [`crate::pda::state::find_state_pda`]; the program derives the bump itself
/// and rejects any other address.
///
/// The state account is owned by the settlement program. This instruction takes
/// no parameters and succeeds only once: a second call fails because the
/// account already exists.
///
/// Wire format: just `[discriminator]`, 1 byte.
/// Required accounts: `[payer (W,S), state_pda (W), system_program (R)]`.
/// The system program must be available for the `CreateAccount` CPI but doesn't
/// need to sit at that specific position.
pub fn initialize(program_id: &Pubkey, payer: &Pubkey, state_pda: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*state_pda, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
        ],
        data: vec![SettlementInstruction::Initialize.discriminator()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let state_pda = Pubkey::new_from_array([3; 32]);

        let ix = initialize(&program_id, &payer, &state_pda);
        assert_eq!(
            ix.data,
            vec![SettlementInstruction::Initialize.discriminator()]
        );
    }

    #[test]
    fn instruction_has_expected_accounts() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let state_pda = Pubkey::new_from_array([3; 32]);

        let ix = initialize(&program_id, &payer, &state_pda);

        assert_eq!(ix.accounts.len(), 3);
        // payer: writable, signer (funds the new account's rent)
        assert_eq!(ix.accounts[0].pubkey, payer);
        assert!(ix.accounts[0].is_writable);
        assert!(ix.accounts[0].is_signer);
        // state_pda: writable, not signer (the program signs via PDA seeds)
        assert_eq!(ix.accounts[1].pubkey, state_pda);
        assert!(ix.accounts[1].is_writable);
        assert!(!ix.accounts[1].is_signer);
        // system program: read-only
        assert_eq!(ix.accounts[2].pubkey, SYSTEM_PROGRAM_ID);
        assert!(!ix.accounts[2].is_writable);
        assert!(!ix.accounts[2].is_signer);
    }
}
