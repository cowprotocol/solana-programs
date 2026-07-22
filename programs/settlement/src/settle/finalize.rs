//! `FinalizeSettle` instruction handler.

use pinocchio::{
    cpi::Signer, sysvars::instructions::Instructions, AccountView, Address, ProgramResult,
};
use pinocchio_token::{instructions::Transfer, state::Account as TokenAccount};
use settlement_interface::{
    instruction::{
        settle::{FinalizeSettleInput, Pushes},
        InstructionInputParsing,
    },
    pda::buffer::validate_buffer_pda,
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
    // validated the push count and destinations.

    validate_push_accounts(program_id, input.token_program_account, &input.pushes)?;

    with_state_pda_signer(program_id, input.state_pda_account, |state_pda_signer| {
        push_funds(input.state_pda_account, state_pda_signer, &input.pushes)
    })
}

/// Validate the accounts every push transfers against the SPL Token program,
/// and, for each push, that its source is the canonical buffer PDA for its
/// destination's mint. Pairing the destination to an order is `BeginSettle`'s job.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn validate_push_accounts(
    program_id: &Address,
    token_program_account: &AccountView,
    pushes: &Pushes,
) -> ProgramResult {
    validate_token_program_account(token_program_account)?;

    for push in pushes.iter() {
        // Read the destination's mint; the borrow ends with this block.
        let mint = {
            let destination = TokenAccount::from_account_view(push.destination)
                .map_err(|_| SettlementError::InvalidBuyTokenAccount)?;
            *destination.mint()
        };
        validate_buffer_pda(program_id, push.source_buffer, &mint, push.bump)?;
    }

    Ok(())
}

/// Push each order's proceeds out of the settlement's buffers, signing each
/// transfer as the canonical state PDA (the buffers' SPL authority).
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn push_funds(
    state_pda_account: &AccountView,
    state_pda_signer: &Signer,
    pushes: &Pushes,
) -> ProgramResult {
    for push in pushes.iter() {
        Transfer::new(
            push.source_buffer,
            push.destination,
            state_pda_account,
            u64::from_le_bytes(*push.amount),
        )
        .invoke_signed(core::slice::from_ref(state_pda_signer))?;
    }

    Ok(())
}
