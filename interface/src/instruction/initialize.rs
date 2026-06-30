//! `Initialize` instruction builder.
//!
//! Allocates the singleton settlement state PDA (see [`crate::pda::state`]).

use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::{InstructionBuilding, InstructionInputParsing};
use crate::SettlementInstruction;

/// Builder for an `Initialize` instruction.
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
/// Wire format: just `[discriminator=3]`, 1 byte.
/// Required accounts: `[payer (W,S), state_pda (W), system_program (R)]`.
/// The system program must be available for the `CreateAccount` CPI but doesn't
/// need to sit at that specific position.
pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub state_pda: Pubkey,
}

impl InstructionBuilding for Initialize {
    fn instruction(self) -> Instruction {
        Instruction {
            program_id: self.program_id,
            accounts: vec![
                AccountMeta::new(self.payer, true),
                AccountMeta::new(self.state_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data: vec![SettlementInstruction::Initialize.discriminator()],
        }
    }
}

/// Parsed inputs of an `Initialize` instruction.
pub struct InitializeInput<'a> {
    pub payer: &'a AccountView,
    pub state_pda: &'a AccountView,
}

impl<'a> InstructionInputParsing<'a> for InitializeInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::Initialize;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        if !instruction_data.is_empty() {
            return Err(ProgramError::InvalidInstructionData);
        }
        // Accounts: [payer (W,S), state_pda (W), system_program (R)]. The system
        // program needs to be present for the `CreateAccount` CPI but doesn't
        // need to be referenced directly and can be at any later position.
        let [payer, state_pda, _system, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self { payer, state_pda })
    }
}

/// Test scaffolding for `Initialize` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::Initialize;
    use crate::instruction::InstructionBuilding;

    /// Number of accounts `Initialize` expects: payer, state PDA, system program.
    pub const NUM_ACCOUNTS: usize = 3;

    /// `Initialize` instruction data with placeholder addresses, for failure
    /// cases where the actual addresses don't matter.
    pub fn initialize_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Initialize {
            program_id: zero,
            payer: zero,
            state_pda: zero,
        }
        .instruction()
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{initialize_data, NUM_ACCOUNTS};
    use super::*;
    use crate::instruction::fixtures::{fake_account_from_array, fake_sequential_accounts};
    use crate::instruction::InstructionBuilding;
    use solana_address::Address;

    #[test]
    fn initialize_input_parses_valid_input() {
        let program_id = Address::new_unique();
        let payer = fake_account_from_array([1; 32]);
        let state_pda = fake_account_from_array([2; 32]);
        let data = Initialize {
            program_id,
            payer: *payer.address(),
            state_pda: *state_pda.address(),
        }
        .instruction()
        .data;

        let system_program = fake_account_from_array([3; 32]);
        let mut accounts = [payer, state_pda, system_program];

        let InitializeInput {
            payer: parsed_payer,
            state_pda: parsed_state_pda,
        } = InitializeInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(parsed_payer.address(), payer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
    }

    #[test]
    fn initialize_input_rejects_long_data() {
        let mut data = initialize_data();
        data.push(0); // trailing byte
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            InitializeInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn initialize_input_rejects_missing_accounts() {
        let data = initialize_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            InitializeInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = Pubkey::new_from_array([1; 32]);
        let payer = Pubkey::new_from_array([2; 32]);
        let state_pda = Pubkey::new_from_array([3; 32]);

        let ix = Initialize {
            program_id,
            payer,
            state_pda,
        }
        .instruction();
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

        let ix = Initialize {
            program_id,
            payer,
            state_pda,
        }
        .instruction();

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
