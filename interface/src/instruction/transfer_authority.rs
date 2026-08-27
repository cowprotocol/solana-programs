//! `TransferAuthority` instruction builder and parser.
//!
//! It transfers a [`Role`] stored in the state PDA to another account in a
//! single step: the manager or the role's current holder names the new holder,
//! and the role's holder is updated immediately.
//!
//! The instruction is role-agnostic: the [`Role`] is part of the instruction
//! data, so the same instruction transfers any authority.

use core::mem::size_of;

use arrayref::array_refs;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::{Role, SettlementInstruction};

/// Builder for a `TransferAuthority` instruction.
///
/// `signer` authorizes the transfer and must sign; it must be either the
/// manager or the current holder of `role`. After execution, `new_authority`
/// becomes the holder of `role`.
///
/// Wire format: `[discriminator=7, role (1 byte), new_authority (32 bytes)]`.
/// Required accounts: `[signer (S), state_pda (W)]`.
pub struct TransferAuthority {
    pub program_id: Pubkey,
    pub signer: Pubkey,
    pub state_pda: Pubkey,
    pub role: Role,
    pub new_authority: Pubkey,
}

impl From<TransferAuthority> for Instruction {
    fn from(builder: TransferAuthority) -> Self {
        let mut data = vec![
            SettlementInstruction::TransferAuthority.discriminator(),
            builder.role.discriminator(),
        ];
        data.extend_from_slice(&builder.new_authority.to_bytes());
        Instruction {
            program_id: builder.program_id,
            accounts: vec![
                AccountMeta::new_readonly(builder.signer, true),
                AccountMeta::new(builder.state_pda, false),
            ],
            data,
        }
    }
}

/// Parsed inputs of a `TransferAuthority` instruction.
pub struct TransferAuthorityInput<'a, A> {
    pub signer: &'a A,
    pub state_pda: &'a A,
    pub role: Role,
    pub new_authority: Pubkey,
}

impl<'a, A> InstructionInputParsing<'a, A> for TransferAuthorityInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::TransferAuthority;

    fn parse_body(instruction_data: &'a [u8], accounts: &'a [A]) -> Result<Self, ProgramError> {
        let body: &[u8; 1 + size_of::<Pubkey>()] = instruction_data
            .try_into()
            .map_err(|_| ProgramError::InvalidInstructionData)?;
        let (role, new_authority) = array_refs![body, 1, size_of::<Pubkey>()];
        let role = Role::try_from(role[0])?;
        let new_authority = Pubkey::new_from_array(*new_authority);

        // Accounts: [signer (S), state_pda (W)].
        let [signer, state_pda, ..] = accounts else {
            return Err(ProgramError::NotEnoughAccountKeys);
        };

        Ok(Self {
            signer,
            state_pda,
            role,
            new_authority,
        })
    }
}

/// Test scaffolding for authority-transfer parsing and handling, shared by this
/// crate's tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_address::Address;
    use solana_instruction::Instruction;

    use super::TransferAuthority;
    use crate::Role;

    /// Number of accounts the transfer instruction expects: the signing
    /// authority and the state PDA.
    pub const NUM_ACCOUNTS: usize = 2;

    /// Byte offset of the role selector in the instruction data: it follows the
    /// one-byte discriminator.
    pub const ROLE_OFFSET: usize = 1;

    /// `TransferAuthority` instruction data with placeholder addresses, for
    /// failure cases where the actual addresses don't matter.
    pub fn transfer_authority_data() -> Vec<u8> {
        let zero = Address::default();
        Instruction::from(TransferAuthority {
            program_id: zero,
            signer: zero,
            state_pda: zero,
            role: Role::Manager,
            new_authority: zero,
        })
        .data
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{transfer_authority_data, NUM_ACCOUNTS, ROLE_OFFSET};
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{assert_readonly_signer, assert_writable_nonsigner};
    use solana_account_view::AccountView;

    #[test]
    fn transfer_authority_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let signer = fake_account(pubkey_from_seed("signer"));
        let state_pda = fake_account(pubkey_from_seed("state pda"));
        let role = Role::ReclaimAuthority;
        let new_authority = pubkey_from_seed("new authority");

        let data = Instruction::from(TransferAuthority {
            program_id,
            signer: *signer.address(),
            state_pda: *state_pda.address(),
            role,
            new_authority,
        })
        .data;
        let accounts = [signer, state_pda];

        let TransferAuthorityInput {
            signer: parsed_signer,
            state_pda: parsed_state_pda,
            role: parsed_role,
            new_authority: parsed_new_authority,
        } = TransferAuthorityInput::parse(&data, &accounts).expect("parse should succeed");

        assert_eq!(parsed_signer.address(), signer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
        assert_eq!(parsed_role, role);
        assert_eq!(parsed_new_authority, new_authority);
    }

    #[test]
    fn transfer_authority_input_rejects_unknown_role() {
        let mut data = transfer_authority_data();
        data[ROLE_OFFSET] = 0xff; // role byte out of range
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            TransferAuthorityInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn transfer_authority_input_rejects_too_long_data() {
        let mut data = transfer_authority_data();
        data.push(0);
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            TransferAuthorityInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn transfer_authority_input_rejects_too_short_data() {
        let mut data = transfer_authority_data();
        data.pop();
        let accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            TransferAuthorityInput::parse(&data, &accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn transfer_authority_input_rejects_missing_accounts() {
        let data = transfer_authority_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            TransferAuthorityInput::parse(&data, &accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = pubkey_from_seed("program id");
        let signer = pubkey_from_seed("signer");
        let state_pda = pubkey_from_seed("state pda");
        let new_authority = pubkey_from_seed("new authority");

        let Instruction { data, .. } = TransferAuthority {
            program_id,
            signer,
            state_pda,
            role: Role::Manager,
            new_authority,
        }
        .into();

        assert_eq!(data.len(), 1 + 1 + core::mem::size_of::<Pubkey>());
        assert_eq!(
            data[0],
            SettlementInstruction::TransferAuthority.discriminator()
        );
        assert_eq!(data[ROLE_OFFSET], Role::Manager.discriminator());
        assert_eq!(&data[2..], &new_authority.to_bytes());
    }

    #[test]
    fn instruction_data_has_expected_accounts() {
        let program_id = pubkey_from_seed("program id");
        let signer = pubkey_from_seed("signer");
        let state_pda = pubkey_from_seed("state pda");
        let new_authority = pubkey_from_seed("new authority");

        let Instruction { accounts, .. } = TransferAuthority {
            program_id,
            signer,
            state_pda,
            role: Role::Manager,
            new_authority,
        }
        .into();

        assert_eq!(accounts.len(), 2);
        // The signer authorizes the transfer, the state PDA is written.
        assert_readonly_signer(&accounts[0], signer);
        assert_writable_nonsigner(&accounts[1], state_pda);
    }
}
