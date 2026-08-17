//! `Initialize` instruction builder.
//!
//! Allocates the singleton settlement state PDA (see [`crate::pda::state`]).

use core::mem::size_of;

use arrayref::array_refs;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
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
/// `manager` and `reclaim_authority` are recorded verbatim in the state PDA's
/// data: the account authorized to add and remove solvers, and the account
/// authorized to reclaim rent for buffers. See
/// [`crate::data::state::StateAccount`].
///
/// The state account is owned by the settlement program. This instruction
/// succeeds only once: a second call fails because the account already
/// exists.
///
/// Wire format: `[discriminator=3, manager (32 bytes), reclaim_authority (32
/// bytes)]`, 65 bytes. Required accounts: `[payer (W,S), state_pda (W),
/// system_program (R)]`. The system program must be available for the
/// `CreateAccount` CPI but doesn't need to sit at that specific position.
pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub state_pda: Pubkey,
    pub manager: Pubkey,
    pub reclaim_authority: Pubkey,
}

impl From<Initialize> for Instruction {
    fn from(builder: Initialize) -> Self {
        let mut data = vec![SettlementInstruction::Initialize.discriminator()];
        data.extend_from_slice(&builder.manager.to_bytes());
        data.extend_from_slice(&builder.reclaim_authority.to_bytes());
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new(builder.payer, true),
                AccountMeta::new(builder.state_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of an `Initialize` instruction.
pub struct InitializeInput<'a, A> {
    pub payer: &'a A,
    pub state_pda: &'a mut A,
    pub manager: Pubkey,
    pub reclaim_authority: Pubkey,
}

impl<'a, A> InstructionInputParsing<'a, A> for InitializeInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::Initialize;

    fn parse_body(instruction_data: &[u8], accounts: &'a mut [A]) -> Result<Self, ProgramError> {
        let authorities: &[u8; 2 * size_of::<Pubkey>()] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;
        let (manager, reclaim_authority) =
            array_refs![authorities, size_of::<Pubkey>(), size_of::<Pubkey>()];
        let manager = Pubkey::new_from_array(*manager);
        let reclaim_authority = Pubkey::new_from_array(*reclaim_authority);

        // Accounts: [payer (W,S), state_pda (W), system_program (R)]. The system
        // program needs to be present for the `CreateAccount` CPI but doesn't
        // need to be referenced directly and can be at any later position.
        let [payer, state_pda, _system, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            payer,
            state_pda,
            manager,
            reclaim_authority,
        })
    }
}

/// Test scaffolding for `Initialize` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{Initialize, Instruction};

    /// Number of accounts `Initialize` expects: payer, state PDA, system program.
    pub const NUM_ACCOUNTS: usize = 3;

    /// `Initialize` instruction data with placeholder addresses, for failure
    /// cases where the actual addresses don't matter.
    pub fn initialize_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(Initialize {
            program_id: zero,
            payer: zero,
            state_pda: zero,
            reclaim_authority: zero,
            manager: zero,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{initialize_data, NUM_ACCOUNTS};
    use super::*;
    use crate::data::state::EncodedStateAccount;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_writable_nonsigner, assert_writable_signer,
    };
    use crate::tests::pubkey_from_seed;
    use solana_account_view::AccountView;
    use solana_address::Address;

    #[test]
    fn initialize_input_parses_valid_input() {
        let program_id = Address::new_unique();
        let payer = fake_account(pubkey_from_seed("payer"));
        let state_pda = fake_account(pubkey_from_seed("state pda"));
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let manager = pubkey_from_seed("manager");
        let data = Instruction::from(Initialize {
            program_id,
            payer: *payer.address(),
            state_pda: *state_pda.address(),
            manager,
            reclaim_authority,
        })
        .data;

        let system_program = fake_account(pubkey_from_seed("system program"));
        let mut accounts = [payer, state_pda, system_program];

        let InitializeInput {
            payer: parsed_payer,
            state_pda: parsed_state_pda,
            manager: parsed_manager,
            reclaim_authority: parsed_reclaim_authority,
        } = InitializeInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(parsed_payer.address(), payer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
        assert_eq!(parsed_manager, manager);
        assert_eq!(parsed_reclaim_authority, reclaim_authority);
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
    fn initialize_input_rejects_short_data() {
        let mut data = initialize_data();
        data.pop(); // one byte short
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
        let program_id = pubkey_from_seed("program id");
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let manager = pubkey_from_seed("manager");

        let Instruction { data, .. } = Initialize {
            program_id,
            payer,
            state_pda,
            reclaim_authority,
            manager,
        }
        .into();
        assert_eq!(data.len(), EncodedStateAccount::SIZE);
        assert_eq!(data[0], SettlementInstruction::Initialize.discriminator());
        assert_eq!(&data[1..33], &manager.to_bytes());
        assert_eq!(&data[33..], &reclaim_authority.to_bytes());
    }

    #[test]
    fn instruction_data_regression() {
        let manager = Pubkey::new_from_array([0x11; 32]);
        let reclaim_authority = Pubkey::new_from_array([0x22; 32]);

        let Instruction { data, .. } = Initialize {
            program_id: pubkey_from_seed("program id"),
            payer: pubkey_from_seed("payer"),
            state_pda: pubkey_from_seed("state pda"),
            manager,
            reclaim_authority,
        }
        .into();

        #[rustfmt::skip]
        let expected: [u8; EncodedStateAccount::SIZE] = [
            // discriminator (Initialize = 3)
            0x03,
            // manager
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            // reclaim_authority
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
        ];
        assert_eq!(data, expected);
    }

    #[test]
    fn instruction_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let manager = pubkey_from_seed("manager");
        let reclaim_authority = pubkey_from_seed("reclaim authority");

        let Instruction { accounts, .. } = Initialize {
            program_id,
            payer,
            state_pda,
            reclaim_authority,
            manager,
        }
        .into();

        assert_eq!(accounts.len(), 3);
        // payer funds the new account's rent; state_pda is signed for by the
        // program via PDA seeds; the system program is only referenced.
        assert_writable_signer(&accounts[0], payer);
        assert_writable_nonsigner(&accounts[1], state_pda);
        assert_readonly_nonsigner(&accounts[2], SYSTEM_PROGRAM_ID);
    }
}
