//! `AddSolver` instruction builder and parser.
//!
//! It inserts a solver into the sorted solver list stored in the state PDA (see
//! [`crate::data::state`]). Only the manager may authorize it. The state PDA
//! grows by one solver, so a `payer` funds the extra rent through a `Transfer`
//! from the system program.

use core::mem::size_of;

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use crate::instruction::InstructionInputParsing;
use crate::SettlementInstruction;

/// Builder for an `AddSolver` instruction.
///
/// `manager` authorizes the change and must be the state PDA's current manager;
/// it signs but doesn't pay. `payer` funds the extra rent and signs the funding
/// transfer. `solver` is inserted into the sorted solver list; adding one
/// already present fails.
///
/// Wire format: `[discriminator=8, solver (32 bytes)]`.
/// Required accounts: `[manager (S), payer (W,S), state_pda (W),
/// system_program (R)]`. The system program must be available for the
/// rent-funding `Transfer` CPI but doesn't need to sit at a specific position.
pub struct AddSolver {
    pub program_id: Pubkey,
    pub manager: Pubkey,
    pub payer: Pubkey,
    pub state_pda: Pubkey,
    pub solver: Pubkey,
}

impl From<AddSolver> for Instruction {
    fn from(builder: AddSolver) -> Self {
        let mut data = vec![SettlementInstruction::AddSolver.discriminator()];
        data.extend_from_slice(&builder.solver.to_bytes());
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.manager, true),
                AccountMeta::new(builder.payer, true),
                AccountMeta::new(builder.state_pda, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of an `AddSolver` instruction.
pub struct AddSolverInput<'a, A> {
    pub manager: &'a A,
    pub payer: &'a A,
    pub state_pda: &'a A,
    pub solver: Pubkey,
}

impl<'a, A> InstructionInputParsing<'a, A> for AddSolverInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::AddSolver;

    fn parse_body(instruction_data: &[u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        let solver: &[u8; size_of::<Pubkey>()] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;
        let solver = Pubkey::new_from_array(*solver);

        // Accounts: [manager (S), payer (W,S), state_pda (W), system_program (R)].
        // The system program needs to be present for the `Transfer` CPI but
        // doesn't need to be referenced directly and can be at any later position.
        let [manager, payer, state_pda, _system, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            manager,
            payer,
            state_pda,
            solver,
        })
    }
}

/// Test scaffolding for `AddSolver` parsing and handling, shared by this crate's
/// tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;
    use solana_instruction::Instruction;

    use super::AddSolver;

    /// Number of accounts `AddSolver` expects: manager, payer, state PDA, system
    /// program.
    pub const NUM_ACCOUNTS: usize = 4;

    /// `AddSolver` instruction data with placeholder addresses, for failure cases
    /// where the actual addresses don't matter.
    pub fn add_solver_data() -> Vec<u8> {
        let zero = Address::default();
        Instruction::from(AddSolver {
            program_id: zero,
            manager: zero,
            payer: zero,
            state_pda: zero,
            solver: zero,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{add_solver_data, NUM_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_readonly_signer, assert_writable_nonsigner,
        assert_writable_signer,
    };
    use solana_account_view::AccountView;

    #[test]
    fn add_solver_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let manager = fake_account(pubkey_from_seed("manager"));
        let payer = fake_account(pubkey_from_seed("payer"));
        let state_pda = fake_account(pubkey_from_seed("state pda"));
        let system_program = fake_account(pubkey_from_seed("system program"));
        let solver = pubkey_from_seed("solver");

        let data = Instruction::from(AddSolver {
            program_id,
            manager: *manager.address(),
            payer: *payer.address(),
            state_pda: *state_pda.address(),
            solver,
        })
        .data;
        let accounts = [manager, payer, state_pda, system_program];

        let AddSolverInput {
            manager: parsed_manager,
            payer: parsed_payer,
            state_pda: parsed_state_pda,
            solver: parsed_solver,
        } = AddSolverInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(parsed_manager.address(), manager.address());
        assert_eq!(parsed_payer.address(), payer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
        assert_eq!(parsed_solver, solver);
    }

    #[test]
    fn add_solver_input_rejects_long_data() {
        let mut data = add_solver_data();
        data.push(42); // trailing byte
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            AddSolverInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn add_solver_input_rejects_short_data() {
        let mut data = add_solver_data();
        data.pop(); // one byte short
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            AddSolverInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn add_solver_input_rejects_missing_accounts() {
        let data = add_solver_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            AddSolverInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let solver = pubkey_from_seed("solver");
        let Instruction { data, .. } = AddSolver {
            program_id: pubkey_from_seed("program id"),
            manager: pubkey_from_seed("manager"),
            payer: pubkey_from_seed("payer"),
            state_pda: pubkey_from_seed("state pda"),
            solver,
        }
        .into();

        assert_eq!(data.len(), 1 + size_of::<Pubkey>());
        assert_eq!(data[0], SettlementInstruction::AddSolver.discriminator());
        assert_eq!(&data[1..], &solver.to_bytes());
    }

    #[test]
    fn instruction_data_regression() {
        let solver = Pubkey::new_from_array([0x11; 32]);
        let Instruction { data, .. } = AddSolver {
            program_id: pubkey_from_seed("program id"),
            manager: pubkey_from_seed("manager"),
            payer: pubkey_from_seed("payer"),
            state_pda: pubkey_from_seed("state pda"),
            solver,
        }
        .into();

        #[rustfmt::skip]
        let expected: [u8; 1 + size_of::<Pubkey>()] = [
            // discriminator (AddSolver = 8)
            0x08,
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
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let Instruction { accounts, .. } = AddSolver {
            program_id: pubkey_from_seed("program id"),
            manager,
            payer,
            state_pda,
            solver: pubkey_from_seed("solver"),
        }
        .into();

        assert_eq!(accounts.len(), 4);
        // The manager authorizes the change; the payer funds the extra rent; the
        // state PDA is grown and written; the system program is only referenced.
        assert_readonly_signer(&accounts[0], manager);
        assert_writable_signer(&accounts[1], payer);
        assert_writable_nonsigner(&accounts[2], state_pda);
        assert_readonly_nonsigner(&accounts[3], SYSTEM_PROGRAM_ID);
    }
}
