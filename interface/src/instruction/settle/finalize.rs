//! Off-chain builder and input parsing for the `FinalizeSettle` instruction.

use std::vec;

use solana_account_view::AccountView;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::instruction::InstructionInputParsing;
use crate::SettlementInstruction;

use super::{recover_counterpart, INSTRUCTIONS_SYSVAR_ID};

/// Builder for a `FinalizeSettle` instruction.
///
/// `begin_ix_index` is the index of the paired `BeginSettle` instruction in the
/// same transaction.
///
/// Wire format: `[discriminator=1, begin_ix_index: u16 LE]`, 3 bytes.
/// Required accounts: `[instructions_sysvar (R)]`.
pub struct FinalizeSettle {
    pub program_id: Pubkey,
    pub begin_ix_index: u16,
}

impl From<FinalizeSettle> for Instruction {
    fn from(builder: FinalizeSettle) -> Self {
        Instruction {
            program_id: builder.program_id,
            accounts: vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)],
            data: [
                &[SettlementInstruction::FinalizeSettle.discriminator()],
                &builder.begin_ix_index.to_le_bytes()[..],
            ]
            .concat(),
        }
    }
}

/// Parsed inputs (instruction-data fields + relevant accounts) of a
/// `FinalizeSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
pub struct FinalizeSettleInput<'a> {
    pub begin_ix_index: u16,
    pub instructions_sysvar_account: &'a AccountView,
}

impl<'a> InstructionInputParsing<'a> for FinalizeSettleInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::FinalizeSettle;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        let (begin_ix_index, _) = recover_counterpart(instruction_data)?;
        let instructions_sysvar_account =
            accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        Ok(Self {
            begin_ix_index,
            instructions_sysvar_account,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::fixtures::fake_account;
    use crate::instruction::settle::tests::ix_data;
    use hex_literal::hex;
    use solana_address::Address;

    #[test]
    fn expected_encoding_finalize_settle() {
        let program_id = Pubkey::new_unique();
        let Instruction { data, accounts, .. } = FinalizeSettle {
            program_id,
            begin_ix_index: 0x1337,
        }
        .into();
        assert_eq!(
            data,
            [
                &[SettlementInstruction::FinalizeSettle.discriminator()][..],
                &hex!("3713")[..], // counterpart index, little-endian
            ]
            .concat(),
        );

        // Only the instructions sysvar is referenced.
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].pubkey, INSTRUCTIONS_SYSVAR_ID);
        assert!(!accounts[0].is_writable);
        assert!(!accounts[0].is_signer);
    }

    #[test]
    fn finalize_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let mut accounts = [fake_account(address)];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x37, 0x13], // begin index, little-endian
        ];
        let FinalizeSettleInput {
            begin_ix_index,
            instructions_sysvar_account,
        } = FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(begin_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &address);
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = ix_data![
            [SettlementInstruction::BeginSettle.discriminator()],
            [0, 0], // begin index
        ];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_empty_accounts() {
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0, 0], // begin index
        ];
        let mut accounts: [AccountView; 0] = [];
        assert_eq!(
            FinalizeSettleInput::parse(&data, &mut accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn finalize_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let second_address = Address::new_from_array([2u8; 32]);
        let mut accounts = [fake_account(first_address), fake_account(second_address)];
        let data = ix_data![
            [SettlementInstruction::FinalizeSettle.discriminator()],
            [0x37, 0x13], // begin index, little-endian
            [42],         // extra
        ];
        let FinalizeSettleInput {
            begin_ix_index,
            instructions_sysvar_account,
        } = FinalizeSettleInput::parse(&data, &mut accounts).expect("parse should succeed");
        assert_eq!(begin_ix_index, 0x1337);
        assert_eq!(instructions_sysvar_account.address(), &first_address);
    }
}
