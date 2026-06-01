//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use pinocchio::{
    error::ProgramError, sysvars::instructions::Instructions, AccountView, Address, ProgramResult,
};
use settlement_interface::{
    recover_discriminator, settle::recover_counterpart, SettlementError, SettlementInstruction,
};

use crate::processor::InstructionInputParsing;

/// Parsed inputs of a `BeginSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
struct BeginSettleInput<'a> {
    finalize_ix_index: u16,
    instructions_sysvar_account: &'a AccountView,
}

/// This implementation defines how instruction bytes and accounts are laid out
/// in the transaction. It's the source of truth for deciding where the data
/// is stored.
impl<'a> InstructionInputParsing<'a> for BeginSettleInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::BeginSettle;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a [AccountView],
    ) -> Result<Self, ProgramError> {
        let finalize_ix_index = recover_counterpart(instruction_data)?;
        let instructions_sysvar_account =
            accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        Ok(Self {
            finalize_ix_index,
            instructions_sysvar_account,
        })
    }
}

/// Parsed inputs (instruction-data fields + relevant accounts) of a
/// `FinalizeSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
struct FinalizeSettleInput<'a> {
    begin_ix_index: u16,
    instructions_sysvar_account: &'a AccountView,
}

impl<'a> InstructionInputParsing<'a> for FinalizeSettleInput<'a> {
    const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::FinalizeSettle;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a [AccountView],
    ) -> Result<Self, ProgramError> {
        let begin_ix_index = recover_counterpart(instruction_data)?;
        let instructions_sysvar_account =
            accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        Ok(Self {
            begin_ix_index,
            instructions_sysvar_account,
        })
    }
}

pub fn process_begin_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let input = BeginSettleInput::parse(instruction_data, accounts)?;

    // We use `instructions_sysvar_account` from the input but this could be
    // any address since parsing doesn't validate the input. We rely on the
    // fact that the Pinocchio library already checks that the input account
    // is the expected one.
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;
    let current_index = instructions.load_current_index();

    // Reciprocity: the input index is a finalize_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        current_index,
        input.finalize_ix_index,
        SettlementInstruction::FinalizeSettle,
    )?;

    // The checks so far are the same as in `process_finalize_settle`.
    // The checks that follow are only performed for `process_begin_settle`.

    // Ordering: the counterpart `FinalizeSettle` must sit strictly after us.
    if input.finalize_ix_index <= current_index {
        return Err(SettlementError::MismatchingSettleCounterpart.into());
    }

    // Nesting check: no BeginSettle/FinalizeSettle of this program may appear
    // strictly between `current_index` and `finalize_ix_index`.
    let search_start = current_index
        .checked_add(1)
        .expect("the finalize index is tested to be larger, no overflow can happen");
    for i in search_start..input.finalize_ix_index {
        let inner = instructions.load_instruction_at(usize::from(i))?;
        // Nothing to see if the instruction belongs to a different program.
        if inner.get_program_id() != program_id {
            continue;
        }
        // If it can't recover the discriminator, it's fine: we expect that
        // instruction to fail, but this isn't something that matters here.
        // If the discriminator is valid, then it should not be the start
        // or end of a settlement.
        if let Ok((discriminator, _)) = recover_discriminator(inner.get_instruction_data()) {
            if [
                SettlementInstruction::BeginSettle,
                SettlementInstruction::FinalizeSettle,
            ]
            .contains(&discriminator)
            {
                return Err(SettlementError::MismatchingSettleCounterpart.into());
            }
        }
    }

    Ok(())
}

pub fn process_finalize_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let input = FinalizeSettleInput::parse(instruction_data, accounts)?;
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;

    // Reciprocity: the input index is a begin_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        instructions.load_current_index(),
        input.begin_ix_index,
        SettlementInstruction::BeginSettle,
    )
}

/// Load the counterpart instruction at `counterpart_index` and verify it
/// belongs to `program_id`, carries `expected_discriminator`, and points
/// back at the current instruction. Ordering (before/after) is the caller's
/// responsibility.
#[must_use = "skipping the counterpart check silently accepts an invalid settle pair"]
fn validate_counterpart<T: core::ops::Deref<Target = [u8]>>(
    program_id: &Address,
    instructions: &Instructions<T>,
    current_index: u16,
    counterpart_index: u16,
    expected_discriminator: SettlementInstruction,
) -> ProgramResult {
    let counterpart_ix = instructions
        .load_instruction_at(usize::from(counterpart_index))
        .map_err(|_| SettlementError::MismatchingSettleCounterpart)?;
    if counterpart_ix.get_program_id() != program_id {
        return Err(SettlementError::MismatchingSettleCounterpart.into());
    }
    let counterpart_ix_data = counterpart_ix.get_instruction_data();
    let (their_discriminator, remaining_data) = recover_discriminator(counterpart_ix_data)
        .map_err(|_| SettlementError::MismatchingSettleCounterpart)?;
    let their_counterpart_ix = recover_counterpart(remaining_data)
        .map_err(|_| SettlementError::MismatchingSettleCounterpart)?;
    if their_discriminator != expected_discriminator || their_counterpart_ix != current_index {
        return Err(SettlementError::MismatchingSettleCounterpart.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::fake_account;

    #[test]
    fn begin_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let accounts = [fake_account(address)];
        let data = [
            SettlementInstruction::BeginSettle.discriminator(),
            0x13,
            0x37,
        ];
        let parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.finalize_ix_index, 0x1337);
        assert_eq!(parsed.instructions_sysvar_account.address(), &address);
    }

    #[test]
    fn finalize_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let accounts = [fake_account(address)];
        let data = [
            SettlementInstruction::FinalizeSettle.discriminator(),
            0x13,
            0x37,
        ];
        let parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.begin_ix_index, 0x1337);
        assert_eq!(parsed.instructions_sysvar_account.address(), &address);
    }

    #[test]
    fn begin_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 0, 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            BeginSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::BeginSettle.discriminator(), 0, 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            FinalizeSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_empty_accounts() {
        let data = [SettlementInstruction::BeginSettle.discriminator(), 0, 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            BeginSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_empty_accounts() {
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 0, 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            FinalizeSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn begin_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let second_address = Address::new_from_array([2u8; 32]);
        let accounts = [fake_account(first_address), fake_account(second_address)];
        let data = [
            SettlementInstruction::BeginSettle.discriminator(),
            0x13, // used
            0x37, // used
            42,   // extra
        ];
        let parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.finalize_ix_index, 0x1337);
        assert_eq!(parsed.instructions_sysvar_account.address(), &first_address);
    }

    #[test]
    fn finalize_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let second_address = Address::new_from_array([2u8; 32]);
        let accounts = [fake_account(first_address), fake_account(second_address)];
        let data = [
            SettlementInstruction::FinalizeSettle.discriminator(),
            0x13, // used
            0x37, // used
            42,   // extra
        ];
        let parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.begin_ix_index, 0x1337);
        assert_eq!(parsed.instructions_sysvar_account.address(), &first_address);
    }
}
