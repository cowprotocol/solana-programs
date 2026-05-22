//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use pinocchio::{
    error::ProgramError, sysvars::instructions::Instructions, AccountView, Address, ProgramResult,
};
use settlement_interface::{
    recover_discriminator, settle::recover_discriminator_and_partner_index, SettlementError,
    SettlementInstruction,
};

/// Parsed inputs (instruction-data fields + relevant accounts) of a
/// `BeginSettle` instruction.
///
/// Strictly the raw extracted form. Fields are read from `instruction_data` and
/// `accounts` but **not validated** against runtime context except confirming
/// that the discriminator matches the desired input.
struct BeginSettleInput<'a> {
    finalize_ix_index: u8,
    sysvar_account: &'a AccountView,
}

impl<'a> BeginSettleInput<'a> {
    fn parse(instruction_data: &[u8], accounts: &'a [AccountView]) -> Result<Self, ProgramError> {
        let (discriminator, finalize_ix_index) =
            recover_discriminator_and_partner_index(instruction_data)?;
        if discriminator != SettlementInstruction::BeginSettle {
            return Err(ProgramError::InvalidInstructionData);
        }
        let sysvar_account = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        Ok(Self {
            finalize_ix_index,
            sysvar_account,
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
    begin_ix_index: u8,
    sysvar_account: &'a AccountView,
}

impl<'a> FinalizeSettleInput<'a> {
    fn parse(instruction_data: &[u8], accounts: &'a [AccountView]) -> Result<Self, ProgramError> {
        let (discriminator, begin_ix_index) =
            recover_discriminator_and_partner_index(instruction_data)?;
        if discriminator != SettlementInstruction::FinalizeSettle {
            return Err(ProgramError::InvalidInstructionData);
        }
        let sysvar_account = accounts.first().ok_or(ProgramError::NotEnoughAccountKeys)?;
        Ok(Self {
            begin_ix_index,
            sysvar_account,
        })
    }
}

pub fn process_begin_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let input = BeginSettleInput::parse(instruction_data, accounts)?;

    // We use `sysvar_account` from the input but this could be any address
    // since parsing doesn't validate the input. We rely on the fact that the
    // Pinocchio library already checks that the input account is the expected
    // one.
    let instructions = Instructions::try_from(input.sysvar_account)?;
    let current_index = instructions.load_current_index();

    // Reciprocity: the input index is a finalize_settle instruction and that
    // instruction points to the current one.
    validate_reciprocal(
        program_id,
        &instructions,
        current_index,
        input.finalize_ix_index,
        SettlementInstruction::FinalizeSettle,
    )?;

    // The checks so far are the same as in `process_finalize_settle`.
    // The checks that follow are only performed for `process_begin_settle`.

    // Ordering: the partner `FinalizeSettle` must sit strictly after us.
    let finalize_ix_index_u16: u16 = u16::from(input.finalize_ix_index);
    if finalize_ix_index_u16 <= current_index {
        return Err(SettlementError::MismatchingSettlePair.into());
    }

    // Nesting check: no BeginSettle/FinalizeSettle of this program may appear
    // strictly between `current_index` and `finalize_ix_index`.
    let search_start = current_index
        .checked_add(1)
        .expect("the finalize index is tested to be larger, no overflow can happen");
    for i in search_start..finalize_ix_index_u16 {
        let inner = instructions.load_instruction_at(usize::from(i))?;
        if inner.get_program_id() == program_id
            && [
                SettlementInstruction::BeginSettle,
                SettlementInstruction::FinalizeSettle,
            ]
            .contains(&recover_discriminator(inner.get_instruction_data())?)
        {
            return Err(SettlementError::MismatchingSettlePair.into());
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
    let instructions = Instructions::try_from(input.sysvar_account)?;

    // Reciprocity: the input index is a finalize_settle instruction and that
    // instruction points to the current one.
    validate_reciprocal(
        program_id,
        &instructions,
        instructions.load_current_index(),
        input.begin_ix_index,
        SettlementInstruction::BeginSettle,
    )
}

/// Load the partner instruction at `reciprocal_index` and verify it belongs to
/// `program_id`, carries `expected_discriminator`, and points back at the
/// current instruction. Ordering (before/after) is the caller's
/// responsibility.
#[must_use = "skipping the reciprocal check silently accepts an invalid settle pair"]
fn validate_reciprocal<T: core::ops::Deref<Target = [u8]>>(
    program_id: &Address,
    instructions: &Instructions<T>,
    current_index: u16,
    reciprocal_index: u8,
    expected_discriminator: SettlementInstruction,
) -> ProgramResult {
    let partner = instructions
        .load_instruction_at(usize::from(reciprocal_index))
        .map_err(|_| SettlementError::MismatchingSettlePair)?;
    if partner.get_program_id() != program_id {
        return Err(SettlementError::MismatchingSettlePair.into());
    }
    let current_index_u8: u8 = current_index
        .try_into()
        .map_err(|_| SettlementError::MismatchingSettlePair)?;
    let (their_discriminator, their_reciprocal) =
        recover_discriminator_and_partner_index(partner.get_instruction_data())
            .map_err(|_| SettlementError::MismatchingSettlePair)?;
    if their_discriminator != expected_discriminator || their_reciprocal != current_index_u8 {
        return Err(SettlementError::MismatchingSettlePair.into());
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
        let data = [SettlementInstruction::BeginSettle.discriminator(), 42];
        let parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.finalize_ix_index, 42);
        assert_eq!(parsed.sysvar_account.address(), &address);
    }

    #[test]
    fn finalize_settle_input_parses_valid_input() {
        let address = Address::new_from_array([0x42u8; 32]);
        let accounts = [fake_account(address)];
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 42];
        let parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.begin_ix_index, 42);
        assert_eq!(parsed.sysvar_account.address(), &address);
    }

    #[test]
    fn begin_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            BeginSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_different_discriminator() {
        let data = [SettlementInstruction::BeginSettle.discriminator(), 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            FinalizeSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn begin_settle_input_rejects_empty_accounts() {
        let data = [SettlementInstruction::BeginSettle.discriminator(), 0];
        let accounts: &[AccountView] = &[];
        assert_eq!(
            BeginSettleInput::parse(&data, accounts).err(),
            Some(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn finalize_settle_input_rejects_empty_accounts() {
        let data = [SettlementInstruction::FinalizeSettle.discriminator(), 0];
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
            42, // used
            67, // extra
        ];
        let parsed = BeginSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.finalize_ix_index, 42);
        assert_eq!(parsed.sysvar_account.address(), &first_address);
    }

    #[test]
    fn finalize_settle_input_ignores_extra_parameters() {
        let first_address = Address::new_from_array([1u8; 32]);
        let second_address = Address::new_from_array([2u8; 32]);
        let accounts = [fake_account(first_address), fake_account(second_address)];
        let data = [
            SettlementInstruction::FinalizeSettle.discriminator(),
            42, // used
            67, // extra
        ];
        let parsed = FinalizeSettleInput::parse(&data, &accounts).expect("parse should succeed");
        assert_eq!(parsed.begin_ix_index, 42);
        assert_eq!(parsed.sysvar_account.address(), &first_address);
    }
}
