//! `FinalizeSettle` instruction handler.

use pinocchio::{sysvars::instructions::Instructions, AccountView, Address, ProgramResult};
use settlement_interface::{
    instruction::{settle::FinalizeSettleInput, InstructionInputParsing},
    SettlementError, SettlementInstruction,
};

use crate::processor::is_cpi_call;

use super::validate_counterpart;

pub fn process_finalize_settle(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    if is_cpi_call() {
        return Err(SettlementError::CalledViaCpi.into());
    }

    let input = FinalizeSettleInput::parse(instruction_data, accounts)?;
    let instructions = Instructions::try_from(input.instructions_sysvar_account)?;
    let current_index = instructions.load_current_index();

    // Reciprocity: the input index is a begin_settle instruction and that
    // instruction points to the current one.
    validate_counterpart(
        program_id,
        &instructions,
        current_index,
        input.begin_ix_index,
        SettlementInstruction::BeginSettle,
    )

    // Some checks are carried out by `BeginSettle` and we don't repeat them
    // under the assumption that the counterpart exists and, since it's a
    // `BeginSettle`, it performs the checks.
}
