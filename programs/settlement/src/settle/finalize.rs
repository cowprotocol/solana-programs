//! `FinalizeSettle` instruction handler.

use pinocchio::{
    cpi::Signer, sysvars::instructions::Instructions, AccountView, Address, ProgramResult,
};
use pinocchio_token::instructions::Transfer;
use settlement_interface::{
    instruction::{
        settle::{FinalizeSettleInput, Pushes},
        InstructionInputParsing,
    },
    SettlementError, SettlementInstruction,
};

use crate::processor::is_cpi_call;

use super::{validate_counterpart, validate_token_program_account, with_state_pda_signer};

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
    )?;

    // `BeginSettle` (which the counterpart check above guarantees ran) already
    // validated every push: its count, its destination, and that its source is
    // the canonical buffer for the order's buy mint. Nothing is left to check
    // here, so `push_funds` only executes the transfers.

    validate_token_program_account(input.token_program_account)?;

    with_state_pda_signer(program_id, input.state_pda_account, |state_pda_signer| {
        push_funds(input.state_pda_account, state_pda_signer, input.pushes)
    })
}

/// Push each order's proceeds out of the settlement's buffers, signing each
/// transfer as the canonical state PDA (the buffers' SPL authority).
///
/// Validating the pushes is done in `BeginSettle`. It does so by checking:
/// 1. the `destination` matches the `buy_token_account` in the OrderIntent
/// 2. the sending buffer in the instruction is derived from the `buy_mint` in
///    the OrderIntent
///
/// So ultimately, we are relying that the SPL token program rejects a transfer
/// whose source and destination mints differ
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn push_funds<'a>(
    state_pda_account: &AccountView,
    state_pda_signer: &Signer,
    pushes: Pushes<'a, AccountView>,
) -> ProgramResult {
    for push in pushes.iter() {
        Transfer::new(
            push.source_buffer,
            push.destination,
            state_pda_account,
            push.amount,
        )
        .invoke_signed(core::slice::from_ref(state_pda_signer))?;
    }

    Ok(())
}
