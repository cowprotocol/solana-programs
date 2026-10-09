//! `Initialize` instruction builder.
//!
//! Allocates the singleton settlement state PDA (see [`crate::pda::state`])
//! and the native SOL buffer (see [`crate::pda::buffer::NATIVE_SOL_BUFFER_PDA`]).

use core::mem::size_of;

use arrayref::array_refs;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

use super::InstructionInputParsing;
use crate::SettlementInstruction;

/// The only account allowed to fill `Initialize`'s `payer` slot, so the
/// state's initial authorities can only be set by a previously authorized address.
pub const DEPLOYER: Pubkey = solana_pubkey::pubkey!("B6acm3swJK9pJ7fe4i4GQgP7x5A3RndvsdV2bKhcA1i5");

/// Builder for an `Initialize` instruction.
///
/// `payer` funds the new accounts' rent and signs. It must be [`DEPLOYER`]. It
/// is meant to be the transaction's fee payer: the state is created once at
/// deployment and never deallocated, so there's no need for a dedicated
/// funding account separate from whoever pays for the deployment transaction.
///
/// `state_pda` must be [`crate::pda::state::STATE_PDA`] and
/// `native_sol_buffer` must be [`crate::pda::buffer::NATIVE_SOL_BUFFER_PDA`];
/// the program rejects any other address.
///
/// All data input accounts are recorded verbatim in the state PDA's data: the
/// account authorized to transfer any role, the account authorized to add and
/// remove solvers, the account authorized to reclaim rent for buffers, and the
/// account authorized to place settlement-owned orders. See
/// [`crate::data::state::StateAccount`].
///
/// Both accounts are owned by the settlement program; the native SOL buffer
/// holds no data. This instruction succeeds only once: a second call fails
/// because the accounts already exist.
///
/// Wire format: `[discriminator=3, manager (32 bytes), solver_authority (32
/// bytes), reclaim_authority (32 bytes), settlement_owned_order_authority (32
/// bytes)]`, 129 bytes. Required accounts: `[payer (W,S), state_pda (W),
/// native_sol_buffer (W), system_program (R)]`. The system program must be available for the
/// `CreateAccount` CPI but doesn't need to sit at that specific position.
pub struct Initialize {
    pub program_id: Pubkey,
    pub payer: Pubkey,
    pub state_pda: Pubkey,
    pub native_sol_buffer: Pubkey,
    pub manager: Pubkey,
    pub solver_authority: Pubkey,
    pub reclaim_authority: Pubkey,
    pub settlement_owned_order_authority: Pubkey,
}

impl From<Initialize> for Instruction {
    fn from(builder: Initialize) -> Self {
        let mut data = vec![SettlementInstruction::Initialize.discriminator()];
        data.extend_from_slice(&builder.manager.to_bytes());
        data.extend_from_slice(&builder.solver_authority.to_bytes());
        data.extend_from_slice(&builder.reclaim_authority.to_bytes());
        data.extend_from_slice(&builder.settlement_owned_order_authority.to_bytes());
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new(builder.payer, true),
                AccountMeta::new(builder.state_pda, false),
                AccountMeta::new(builder.native_sol_buffer, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM_ID, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of an `Initialize` instruction.
pub struct InitializeInput<'a, A> {
    pub payer: &'a A,
    pub state_pda: &'a A,
    pub native_sol_buffer: &'a A,
    pub manager: Pubkey,
    pub solver_authority: Pubkey,
    pub reclaim_authority: Pubkey,
    pub settlement_owned_order_authority: Pubkey,
}

impl<'a, A> InstructionInputParsing<'a, A> for InitializeInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::Initialize;

    fn parse_body(instruction_data: &[u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        let authorities: &[u8; 4 * size_of::<Pubkey>()] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;
        let (manager, solver_authority, reclaim_authority, settlement_owned_order_authority) = array_refs![
            authorities,
            size_of::<Pubkey>(),
            size_of::<Pubkey>(),
            size_of::<Pubkey>(),
            size_of::<Pubkey>()
        ];
        let manager = Pubkey::new_from_array(*manager);
        let solver_authority = Pubkey::new_from_array(*solver_authority);
        let reclaim_authority = Pubkey::new_from_array(*reclaim_authority);
        let settlement_owned_order_authority =
            Pubkey::new_from_array(*settlement_owned_order_authority);

        // Accounts: [payer (W,S), state_pda (W), native_sol_buffer (W),
        // system_program (R)]. The system program needs to be present for the
        // `CreateAccount` CPI but doesn't need to be referenced directly and can
        // be at any later position.
        let [payer, state_pda, native_sol_buffer, _system, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            payer,
            state_pda,
            native_sol_buffer,
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        })
    }
}

/// Test scaffolding for `Initialize` parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;

    use super::{Initialize, Instruction};

    /// Number of accounts `Initialize` expects: payer, state PDA, native SOL
    /// buffer, system program.
    pub const NUM_ACCOUNTS: usize = 4;

    /// `Initialize` instruction data with placeholder addresses, for failure
    /// cases where the actual addresses don't matter.
    pub fn initialize_data() -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        Instruction::from(Initialize {
            program_id: zero,
            payer: zero,
            state_pda: zero,
            native_sol_buffer: zero,
            manager: zero,
            solver_authority: zero,
            reclaim_authority: zero,
            settlement_owned_order_authority: zero,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{initialize_data, NUM_ACCOUNTS};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{
        assert_readonly_nonsigner, assert_writable_nonsigner, assert_writable_signer,
    };
    use solana_account_view::AccountView;
    use solana_address::Address;

    #[test]
    fn initialize_input_parses_valid_input() {
        let program_id = Address::new_unique();
        let payer = fake_account(pubkey_from_seed("payer"));
        let state_pda = fake_account(pubkey_from_seed("state pda"));
        let native_sol_buffer = fake_account(pubkey_from_seed("native sol buffer"));
        let manager = pubkey_from_seed("manager");
        let solver_authority = pubkey_from_seed("solver authority");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let settlement_owned_order_authority = pubkey_from_seed("settlement-owned-order authority");
        let data = Instruction::from(Initialize {
            program_id,
            payer: *payer.address(),
            state_pda: *state_pda.address(),
            native_sol_buffer: *native_sol_buffer.address(),
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        })
        .data;

        let system_program = fake_account(pubkey_from_seed("system program"));
        let accounts = [payer, state_pda, native_sol_buffer, system_program];

        let InitializeInput {
            payer: parsed_payer,
            state_pda: parsed_state_pda,
            native_sol_buffer: parsed_native_sol_buffer,
            manager: parsed_manager,
            solver_authority: parsed_solver_authority,
            reclaim_authority: parsed_reclaim_authority,
            settlement_owned_order_authority: parsed_settlement_owned_order_authority,
        } = InitializeInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(parsed_payer.address(), payer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
        assert_eq!(
            parsed_native_sol_buffer.address(),
            native_sol_buffer.address()
        );
        assert_eq!(parsed_manager, manager);
        assert_eq!(parsed_solver_authority, solver_authority);
        assert_eq!(parsed_reclaim_authority, reclaim_authority);
        assert_eq!(
            parsed_settlement_owned_order_authority,
            settlement_owned_order_authority
        );
    }

    #[test]
    fn initialize_input_rejects_long_data() {
        let mut data = initialize_data();
        data.push(0); // trailing byte
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            InitializeInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn initialize_input_rejects_short_data() {
        let mut data = initialize_data();
        data.pop(); // one byte short
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            InitializeInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn initialize_input_rejects_missing_accounts() {
        let data = initialize_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            InitializeInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = pubkey_from_seed("program id");
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let native_sol_buffer = pubkey_from_seed("native sol buffer");
        let manager = pubkey_from_seed("manager");
        let solver_authority = pubkey_from_seed("solver authority");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let settlement_owned_order_authority = pubkey_from_seed("settlement-owned-order authority");

        let Instruction { data, .. } = Initialize {
            program_id,
            payer,
            state_pda,
            native_sol_buffer,
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        }
        .into();
        assert_eq!(data.len(), 1 + 4 * core::mem::size_of::<Pubkey>());
        assert_eq!(data[0], SettlementInstruction::Initialize.discriminator());
        assert_eq!(&data[1..33], &manager.to_bytes());
        assert_eq!(&data[33..65], &solver_authority.to_bytes());
        assert_eq!(&data[65..97], &reclaim_authority.to_bytes());
        assert_eq!(&data[97..], &settlement_owned_order_authority.to_bytes());
    }

    #[test]
    fn instruction_data_regression() {
        let manager = Pubkey::new_from_array([0x11; 32]);
        let solver_authority = Pubkey::new_from_array([0x22; 32]);
        let reclaim_authority = Pubkey::new_from_array([0x33; 32]);
        let settlement_owned_order_authority = Pubkey::new_from_array([0x44; 32]);

        let Instruction { data, .. } = Initialize {
            program_id: pubkey_from_seed("program id"),
            payer: pubkey_from_seed("payer"),
            state_pda: pubkey_from_seed("state pda"),
            native_sol_buffer: pubkey_from_seed("native sol buffer"),
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        }
        .into();

        #[rustfmt::skip]
        let expected: [u8; 1 + 4 * core::mem::size_of::<Pubkey>()] = [
            // discriminator (Initialize = 3)
            0x03,
            // manager
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            // solver_authority
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            // reclaim_authority
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            // settlement_owned_order_authority
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        ];
        assert_eq!(data, expected);
    }

    #[test]
    fn instruction_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let payer = pubkey_from_seed("payer");
        let state_pda = pubkey_from_seed("state pda");
        let native_sol_buffer = pubkey_from_seed("native sol buffer");
        let manager = pubkey_from_seed("manager");
        let solver_authority = pubkey_from_seed("solver authority");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let settlement_owned_order_authority = pubkey_from_seed("settlement-owned-order authority");

        let Instruction { accounts, .. } = Initialize {
            program_id,
            payer,
            state_pda,
            native_sol_buffer,
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        }
        .into();

        assert_eq!(accounts.len(), 4);
        // payer funds the new accounts' rent; state_pda and native_sol_buffer
        // are signed for by the program via PDA seeds; the system program is
        // only referenced.
        assert_writable_signer(&accounts[0], payer);
        assert_writable_nonsigner(&accounts[1], state_pda);
        assert_writable_nonsigner(&accounts[2], native_sol_buffer);
        assert_readonly_nonsigner(&accounts[3], SYSTEM_PROGRAM_ID);
    }
}
