//! `FinalizeSettle` instruction handler.

use cow_settlement_interface::{
    instruction::{
        settle::{FinalizeSettleInput, Pushes},
        InstructionInputParsing,
    },
    pda::buffer::validate_buffer_pda,
    SettlementError, SettlementInstruction,
};
use pinocchio::{
    cpi::Signer, sysvars::instructions::Instructions, AccountView, Address, ProgramResult,
};
use pinocchio_token::instructions::Transfer;

use crate::{
    processor::{is_cpi_call, with_state_pda_signer},
    token::{read_token_account, validate_token_program},
};

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
    )?;

    // `BeginSettle` (which the counterpart check above guarantees ran) already
    // validated the push count and destinations. `push_funds` adds the only
    // remaining check: each push draws from the buffer for its mint.

    let token_program = validate_token_program(input.token_program_account)?;

    with_state_pda_signer(program_id, input.state_pda_account, |state_pda_signer| {
        push_funds(
            program_id,
            input.state_pda_account,
            state_pda_signer,
            input.pushes,
            token_program,
        )
    })
}

/// Push each order's proceeds out of the settlement's buffers, signing each
/// transfer as the canonical state PDA (the buffers' SPL authority). Each push's
/// source must be the derived buffer for its destination's mint; pairing the
/// destination to an order is `BeginSettle`'s job.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn push_funds<'a>(
    program_id: &Address,
    state_pda_account: &AccountView,
    state_pda_signer: &Signer,
    pushes: Pushes<'a, AccountView>,
    token_program: &Address,
) -> ProgramResult {
    for push in pushes.iter() {
        // The read is by value, so nothing is left borrowing the destination
        // when the transfer below reuses it.
        let mint = read_token_account(token_program, push.destination)
            .map_err(|_| SettlementError::InvalidBuyTokenAccount)?
            .mint;
        validate_buffer_pda(program_id, push.source_buffer, &mint, push.bump)?;

        Transfer::new(
            push.source_buffer,
            push.destination,
            state_pda_account,
            push.amount,
        )
        .invoke_signed_with_unverified_program(
            core::slice::from_ref(state_pda_signer),
            token_program,
        )?;
    }

    Ok(())
}
