//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use settlement_interface::SettlementInstruction;

use crate::processor::InstructionInputParsing;

/// Parsed inputs (currently empty) of a `BeginSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
struct BeginSettleInput {}

impl InstructionInputParsing for BeginSettleInput {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::BeginSettle;

    fn parse_body(
        _instruction_data: &[u8],
        _accounts: &[AccountView],
    ) -> Result<Self, ProgramError> {
        Ok(Self {})
    }
}

/// Parsed inputs (currently empty) of a `FinalizeSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
struct FinalizeSettleInput {}

impl InstructionInputParsing for FinalizeSettleInput {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::FinalizeSettle;

    fn parse_body(
        _instruction_data: &[u8],
        _accounts: &[AccountView],
    ) -> Result<Self, ProgramError> {
        Ok(Self {})
    }
}

pub fn process_begin_settle(
    _program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let _input = BeginSettleInput::parse(instruction_data, accounts)?;
    Ok(())
}

pub fn process_finalize_settle(
    _program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let _input = FinalizeSettleInput::parse(instruction_data, accounts)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::fake_account;

    #[test]
    fn begin_settle_input_parses_valid_input() {
        let accounts = [];
        let data = [SettlementInstruction::BeginSettle.discriminator()];
        let _parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
    }

    #[test]
    fn finalize_settle_input_parses_valid_input() {
        let accounts = [];
        let data = [SettlementInstruction::FinalizeSettle.discriminator()];
        let _parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
    }

    #[test]
    fn begin_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::FinalizeSettle.discriminator()];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            BeginSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::BeginSettle.discriminator()];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            FinalizeSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let accounts = [fake_account(first_address)];
        let data = [
            SettlementInstruction::BeginSettle.discriminator(),
            67, // extra
        ];
        let _parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
    }

    #[test]
    fn finalize_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let accounts = [fake_account(first_address)];
        let data = [
            SettlementInstruction::FinalizeSettle.discriminator(),
            67, // extra
        ];
        let _parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
    }
}
