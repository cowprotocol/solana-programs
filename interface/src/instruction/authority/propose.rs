//! `ProposeAuthority` instruction builder and parser.
//!
//! It proposes a new holder for a [`Role`] stored in the state PDA: the manager
//! or the role's current holder names the account to take over. The proposal is
//! recorded as the role's pending holder and takes effect only once that
//! account accepts in a separate acceptance step. See the [module docs](super)
//! for the full transfer flow.

use core::mem::size_of;

use arrayref::array_refs;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::{Role, SettlementInstruction};

/// Builder for a `ProposeAuthority` instruction.
///
/// `signer` is the proposer and must sign; it must be the manager or the
/// current holder of `role`. `new_authority` is the account proposed to take
/// over `role`; it takes effect only once it accepts in a separate acceptance
/// step.
///
/// Wire format: `[discriminator=7, role (1 byte), new_authority (32 bytes)]`.
/// Required accounts: `[signer (S), state_pda (W)]`.
pub struct ProposeAuthority {
    pub program_id: Pubkey,
    pub signer: Pubkey,
    pub state_pda: Pubkey,
    pub role: Role,
    pub new_authority: Pubkey,
}

impl From<ProposeAuthority> for Instruction {
    fn from(builder: ProposeAuthority) -> Self {
        let mut data = vec![
            SettlementInstruction::ProposeAuthority.discriminator(),
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

/// Parsed inputs of a `ProposeAuthority` instruction.
pub struct ProposeAuthorityInput<'a, A> {
    pub signer: &'a A,
    pub state_pda: &'a mut A,
    pub role: Role,
    pub new_authority: Pubkey,
}

impl<'a, A> InstructionInputParsing<'a, A> for ProposeAuthorityInput<'a, A> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::ProposeAuthority;

    fn parse_body(instruction_data: &[u8], accounts: &'a mut [A]) -> Result<Self, ProgramError> {
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

    use super::ProposeAuthority;
    use crate::Role;

    /// Number of accounts the transfer instructions expect: the signing
    /// authority and the state PDA.
    pub const NUM_ACCOUNTS: usize = 2;

    /// Byte offset of the role selector in the instruction data: it follows the
    /// one-byte discriminator.
    pub const ROLE_OFFSET: usize = 1;

    /// `ProposeAuthority` instruction data with placeholder addresses, for
    /// failure cases where the actual addresses don't matter.
    pub fn propose_authority_data() -> Vec<u8> {
        let zero = Address::default();
        Instruction::from(ProposeAuthority {
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
    use super::fixtures::{propose_authority_data, NUM_ACCOUNTS, ROLE_OFFSET};
    use super::*;
    use crate::instruction::fixtures::{fake_account, fake_sequential_accounts};
    use crate::instruction::tests::{assert_readonly_signer, assert_writable_nonsigner};
    use crate::tests::pubkey_from_seed;
    use solana_account_view::AccountView;

    #[test]
    fn propose_authority_input_parses_valid_input() {
        let program_id = pubkey_from_seed("program id");
        let signer = fake_account(pubkey_from_seed("signer"));
        let state_pda = fake_account(pubkey_from_seed("state pda"));
        let role = Role::ReclaimAuthority;
        let new_authority = pubkey_from_seed("new authority");

        let data = Instruction::from(ProposeAuthority {
            program_id,
            signer: *signer.address(),
            state_pda: *state_pda.address(),
            role,
            new_authority,
        })
        .data;
        let mut accounts = [signer, state_pda];

        let ProposeAuthorityInput {
            signer: parsed_signer,
            state_pda: parsed_state_pda,
            role: parsed_role,
            new_authority: parsed_new_authority,
        } = ProposeAuthorityInput::parse(&data, &mut accounts).expect("parse should succeed");

        assert_eq!(parsed_signer.address(), signer.address());
        assert_eq!(parsed_state_pda.address(), state_pda.address());
        assert_eq!(parsed_role, role);
        assert_eq!(parsed_new_authority, new_authority);
    }

    #[test]
    fn propose_authority_input_rejects_unknown_role() {
        let mut data = propose_authority_data();
        data[ROLE_OFFSET] = 0xff; // role byte out of range
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            ProposeAuthorityInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn propose_authority_input_rejects_too_long_data() {
        let mut data = propose_authority_data();
        data.push(0);
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            ProposeAuthorityInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn propose_authority_input_rejects_too_short_data() {
        let mut data = propose_authority_data();
        data.pop();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            ProposeAuthorityInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn propose_authority_input_rejects_missing_accounts() {
        let data = propose_authority_data();
        let mut accounts: Vec<AccountView> = fake_sequential_accounts::<NUM_ACCOUNTS>().into();
        accounts.pop();
        assert_eq!(
            ProposeAuthorityInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn instruction_data_has_expected_layout() {
        let program_id = pubkey_from_seed("program id");
        let signer = pubkey_from_seed("signer");
        let state_pda = pubkey_from_seed("state pda");
        let new_authority = pubkey_from_seed("new authority");

        let Instruction { data, .. } = ProposeAuthority {
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
            SettlementInstruction::ProposeAuthority.discriminator()
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

        let Instruction { accounts, .. } = ProposeAuthority {
            program_id,
            signer,
            state_pda,
            role: Role::Manager,
            new_authority,
        }
        .into();

        assert_eq!(accounts.len(), 2);
        // The proposer signs to authorize the transfer, the state PDA is
        // written.
        assert_readonly_signer(&accounts[0], signer);
        assert_writable_nonsigner(&accounts[1], state_pda);
    }
}
