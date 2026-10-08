//! `FinalizeSettle` instruction handler.

use cow_settlement_interface::{
    instruction::{
        settle::{FinalizeSettleInput, Pushes},
        InstructionInputParsing,
    },
    pda::{buffer::NATIVE_SOL_BUFFER_PDA, state::validate_is_state_pda},
    SettlementError, SettlementInstruction,
};
use pinocchio::{
    cpi::Signer,
    sysvars::{instructions::Instructions, rent::Rent, Sysvar},
    AccountView, Address, ProgramResult,
};

use crate::processor::utils::{
    auth::with_state_pda_signer,
    cpi::is_cpi_call,
    lamports::move_lamports,
    settle::validate_counterpart,
    token::{owning_token_program, read_mint_decimals, TransferMaybeChecked},
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

    validate_is_state_pda(input.state_pda_account.address().as_array())?;

    // `BeginSettle` (which the counterpart check above guarantees ran) already
    // validated every push: its count, its destination, and that its source is
    // the canonical buffer for the order's buy mint. Nothing is left to check
    // here, so `push_funds` only executes the transfers.

    with_state_pda_signer(|state_pda_signer| {
        push_funds(input.state_pda_account, state_pda_signer, input.pushes)
    })
}

/// Push each order's proceeds out of the settlement's buffers, signing each
/// transfer as the canonical state PDA (the buffers' SPL authority), or out of
/// the native SOL buffer's lamports for an order paid in native SOL.
///
/// Validating the pushes is done in `BeginSettle`. It does so by checking:
/// 1. the `destination` matches the `buy_token_account` in the OrderIntentAccessor
/// 2. the sending buffer in the instruction is the one holding the
///    settlement's funds for the relevant buy asset. For native SOL, this is
///    the native SOL buffer, and for tokens, its the buffer account associated
///    with the buy_mint.
///
/// So ultimately, for an SPL push we are relying that the SPL token program
/// rejects a transfer whose source and destination mints differ (and, on a
/// `TransferChecked`, whose mint account differs from theirs).
///
/// We use two separate loops to effectively separate the SPL Token payments
/// from the native payments. This is because the SVM doesn't allow CPIs (in our
/// case, the SPL Transfer call) if the lamport count for an account involved
/// in the CPI changed before it (it reverts with `UnbalancedInstruction`).
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn push_funds<'a>(
    state_pda_account: &AccountView,
    state_pda_signer: &Signer,
    pushes: Pushes<'a, AccountView>,
) -> ProgramResult {
    // Loop for orders not paying out native SOL
    for push in pushes.iter() {
        if push.source_buffer.address() != &NATIVE_SOL_BUFFER_PDA {
            let token_program = owning_token_program(push.destination)
                .map_err(|_| SettlementError::InvalidTokenProgram)?;
            TransferMaybeChecked {
                token_program,
                from: push.source_buffer,
                mint: &read_mint_decimals(token_program, push.mint)?,
                to: push.destination,
                authority: state_pda_account,
                amount: push.amount,
                signer: state_pda_signer,
            }
            .invoke()?;
        }
    }

    // Loop for orders paying out native SOL
    let mut native_sol_buffer = None;
    for push in pushes.iter() {
        if push.source_buffer.address() == &NATIVE_SOL_BUFFER_PDA {
            let mut source = *push.source_buffer;
            let mut destination = *push.destination;
            move_lamports(&mut source, &mut destination, push.amount)?;
            native_sol_buffer = Some(source);
        }
    }

    // The runtime would accept payouts that empty the buffer entirely, and then
    // delete it, leaving native SOL orders unsettleable. Its rent stays put.
    if let Some(buffer) = native_sol_buffer {
        if buffer.lamports() < Rent::get()?.try_minimum_balance(buffer.data_len())? {
            return Err(SettlementError::NativeSolBufferBelowRent.into());
        }
    }

    Ok(())
}
