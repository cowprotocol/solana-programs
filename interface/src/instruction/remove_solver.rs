//! `RemoveSolver` instruction builder and parser.
//!
//! It removes a solver from the sorted solver list stored in the state PDA (see
//! [`crate::data::state`]). Only the manager may authorize it. The state PDA
//! shrinks by one solver and the freed rent is paid to `rent_recipient`.

use core::mem::size_of;

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::SettlementInstruction;

/// Builder for a `RemoveSolver` instruction.
///
/// `manager` authorizes the change and must be the state PDA's current manager;
/// it signs but doesn't receive anything. `rent_recipient` receives the freed
/// rent. `solver` is removed from the sorted solver list; removing one that
/// isn't present fails.
///
/// Wire format: `[discriminator=9, solver (32 bytes)]`.
/// Required accounts: `[manager (S), rent_recipient (W), state_pda (W)]`.
pub struct RemoveSolver {
    pub program_id: Pubkey,
    pub manager: Pubkey,
    pub rent_recipient: Pubkey,
    pub state_pda: Pubkey,
    pub solver: Pubkey,
}

impl From<RemoveSolver> for Instruction {
    fn from(builder: RemoveSolver) -> Self {
        let mut data = vec![SettlementInstruction::RemoveSolver.discriminator()];
        data.extend_from_slice(&builder.solver.to_bytes());
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.manager, true),
                AccountMeta::new(builder.rent_recipient, false),
                AccountMeta::new(builder.state_pda, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of a `RemoveSolver` instruction.
pub struct RemoveSolverInput<'a, A> {
    pub manager: &'a A,
    pub rent_recipient: &'a A,
    pub state_pda: &'a A,
    pub solver: Pubkey,
}

impl<'a, A> InstructionInputParsing<'a, A> for RemoveSolverInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::RemoveSolver;

    fn parse_body(instruction_data: &[u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        let solver: &[u8; size_of::<Pubkey>()] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;
        let solver = Pubkey::new_from_array(*solver);

        // Accounts: [manager (S), rent_recipient (W), state_pda (W)].
        let [manager, rent_recipient, state_pda, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            manager,
            rent_recipient,
            state_pda,
            solver,
        })
    }
}

/// Test scaffolding for `RemoveSolver` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{Instruction, RemoveSolver};

    /// Number of accounts `RemoveSolver` expects: manager, rent recipient, and
    /// state PDA.
    pub const NUM_ACCOUNTS: usize = 3;

    /// `RemoveSolver` instruction data with placeholder addresses, for failure
    /// cases where the actual addresses don't matter.
    pub fn remove_solver_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(RemoveSolver {
            program_id: zero,
            manager: zero,
            rent_recipient: zero,
            state_pda: zero,
            solver: zero,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{remove_solver_data, NUM_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{assert_readonly_signer, assert_writable_nonsigner};
    use solana_account_view::AccountView;

    #[test]
    fn remove_solver_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let manager = fake_account(pubkey_from_seed("manager"));
        let rent_recipient = fake_account(pubkey_from_seed("rent recipient"));
        let state_pda = fake_account(pubkey_from_seed("state pda"));
        let solver = pubkey_from_seed("solver");

        let data = Instruction::from(RemoveSolver {
            program_id,
            manager: *manager.address(),
            rent_recipient: *rent_recipient.address(),
            state_pda: *state_pda.address(),
            solver,
        })
        .data;
        let accounts = [manager, rent_recipient, state_pda];

        let RemoveSolverInput {
            manager: parsed_manager,
            rent_recipient: parsed_rent_recipient,
            state_pda: parsed_state_pda,
            solver: parsed_solver,
        } = RemoveSolverInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(parsed_manager.address(), accounts[0].address());
        assert_eq!(parsed_rent_recipient.address(), accounts[1].address());
        assert_eq!(parsed_state_pda.address(), accounts[2].address());
        assert_eq!(parsed_solver, solver);
    }

    #[test]
    fn remove_solver_input_rejects_long_data() {
        let mut data = remove_solver_data();
        data.push(0); // trailing byte
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            RemoveSolverInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn remove_solver_input_rejects_short_data() {
        let mut data = remove_solver_data();
        data.pop(); // one byte short
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            RemoveSolverInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn remove_solver_input_rejects_missing_accounts() {
        let data = remove_solver_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            RemoveSolverInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let solver = pubkey_from_seed("solver");
        let Instruction { data, .. } = RemoveSolver {
            program_id: pubkey_from_seed("program id"),
            manager: pubkey_from_seed("manager"),
            rent_recipient: pubkey_from_seed("rent recipient"),
            state_pda: pubkey_from_seed("state pda"),
            solver,
        }
        .into();

        assert_eq!(data.len(), 1 + size_of::<Pubkey>());
        assert_eq!(data[0], SettlementInstruction::RemoveSolver.discriminator());
        assert_eq!(&data[1..], &solver.to_bytes());
    }

    #[test]
    fn instruction_data_regression() {
        let solver = Pubkey::new_from_array([0x11; 32]);
        let Instruction { data, .. } = RemoveSolver {
            program_id: pubkey_from_seed("program id"),
            manager: pubkey_from_seed("manager"),
            rent_recipient: pubkey_from_seed("rent recipient"),
            state_pda: pubkey_from_seed("state pda"),
            solver,
        }
        .into();

        #[rustfmt::skip]
        let expected: [u8; 1 + size_of::<Pubkey>()] = [
            // discriminator (RemoveSolver = 9)
            0x09,
            // solver
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
        ];
        assert_eq!(data, expected);
    }

    #[test]
    fn instruction_has_expected_accounts() {
        let manager = pubkey_from_seed("manager");
        let rent_recipient = pubkey_from_seed("rent recipient");
        let state_pda = pubkey_from_seed("state pda");
        let Instruction { accounts, .. } = RemoveSolver {
            program_id: pubkey_from_seed("program id"),
            manager,
            rent_recipient,
            state_pda,
            solver: pubkey_from_seed("solver"),
        }
        .into();

        assert_eq!(accounts.len(), 3);
        // The manager authorizes the change; the freed rent is paid to the
        // recipient; the state PDA is shrunk and written.
        assert_readonly_signer(&accounts[0], manager);
        assert_writable_nonsigner(&accounts[1], rent_recipient);
        assert_writable_nonsigner(&accounts[2], state_pda);
    }
}
