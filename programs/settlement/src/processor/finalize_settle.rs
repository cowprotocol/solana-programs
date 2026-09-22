//! `FinalizeSettle` instruction handler.

use cow_settlement_interface::{
    instruction::{
        settle::{FinalizeSettleInput, Pushes},
        InstructionInputParsing,
    },
    SettlementError, SettlementInstruction,
};
use pinocchio::{
    cpi::Signer, error::ProgramError, sysvars::instructions::Instructions, AccountView, Address,
    ProgramResult,
};
use pinocchio_token::instructions::Transfer;

use crate::processor::utils::{
    auth::with_state_pda_signer, cpi::is_cpi_call, settle::validate_counterpart,
    token::owning_token_program,
};

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

    with_state_pda_signer(program_id, input.state_pda_account, |state_pda_signer| {
        push_funds(input.state_pda_account, state_pda_signer, input.pushes)
    })
}

/// Push each order's proceeds out of the settlement's buffers, signing each
/// transfer as the canonical state PDA (the buffers' SPL authority), or out of
/// the state PDA's own lamports for an order paid in native SOL.
///
/// Validating the pushes is done in `BeginSettle`. It does so by checking:
/// 1. the `destination` matches the `buy_token_account` in the OrderIntentAccessor
/// 2. the sending account in the instruction is the one holding the
///    settlement's funds for the `buy_mint` in the OrderIntentAccessor
///
/// So ultimately, for an SPL push we are relying that the SPL token program
/// rejects a transfer whose source and destination mints differ.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
#[allow(clippy::arithmetic_side_effects)]
fn push_funds<'a>(
    state_pda_account: &AccountView,
    state_pda_signer: &Signer,
    pushes: Pushes<'a, AccountView>,
) -> ProgramResult {
    let mut state_deducted_lamports = 0u64;
    for push in pushes.iter() {
        // Pay out native SOL?
        if push.source_buffer.address() == state_pda_account.address() {
            if push.destination.address() != state_pda_account.address() {
                let mut destination = *push.destination;
                destination.set_lamports(
                    destination
                        .lamports()
                        .checked_add(push.amount)
                        .ok_or(ProgramError::ArithmeticOverflow)?,
                );

                // Using unsafe add here to save a decent bit of CU
                // If the amount overflows here, it will ultemately lead to a UnbalancedInstruction, and amounts
                // everywhere else are being checked.
                state_deducted_lamports += push.amount;
            }
        } else {
            let token_program = owning_token_program(push.destination)
                .map_err(|_| SettlementError::InvalidTokenProgram)?;
            Transfer::new(
                push.source_buffer,
                push.destination,
                state_pda_account,
                push.amount,
            )
            .invoke_signed_with_unverified_program(
                core::slice::from_ref(state_pda_signer),
                &token_program.address(),
            )?;
        }
    }

    if state_deducted_lamports > 0 {
        let mut state_pda_account = *state_pda_account;
        state_pda_account.set_lamports(
            state_pda_account
                .lamports()
                .checked_sub(state_deducted_lamports)
                .ok_or(ProgramError::ArithmeticOverflow)?,
        );
    }

    Ok(())
}
