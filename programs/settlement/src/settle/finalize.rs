//! `FinalizeSettle` instruction handler.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::instructions::Instructions,
    AccountView, Address, ProgramResult,
};
use pinocchio_token::{instructions::Transfer, state::Account as TokenAccount};
use settlement_interface::{
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID,
        settle::{FinalizeSettleInput, Pushes},
        InstructionInputParsing,
    },
    pda::{buffer::buffer_pda_signer_seeds, state::state_pda_seeds},
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
    )?;

    // Order correspondence and destinations were validated by the paired
    // `BeginSettle` (which the counterpart check above guarantees ran); the one
    // push check left to us is that each push draws from the canonical buffer for
    // its destination's mint — we hold the destination account, so we can read
    // its mint. Then we execute the transfers.
    push_funds(
        program_id,
        input.token_program_account,
        input.state_pda_account,
        input.pushes,
    )
}

/// Validate and push each order's proceeds out of the settlement's buffers.
/// Only the legacy SPL Token program is accepted and the supplied state account
/// must be the canonical state PDA, since it's the buffers' SPL authority and so
/// must sign each transfer. For every push, the source must be the canonical
/// buffer for the destination's mint; the destination's correspondence to an
/// order is the paired `BeginSettle`'s responsibility.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn push_funds<'a>(
    program_id: &Address,
    token_program_account: &AccountView,
    state_pda_account: &AccountView,
    pushes: Pushes<'a>,
) -> ProgramResult {
    if token_program_account.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // The buffers' SPL authority is the state PDA, so it must sign each transfer.
    let seeds = state_pda_seeds();
    let (state_pda, state_bump) = Address::find_program_address(&seeds, program_id);
    if state_pda_account.address() != &state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }

    let [seed] = seeds;
    let state_bump = [state_bump];
    let signer_seeds = [seed, &state_bump].map(Seed::from);
    let state_pda_signer = Signer::from(&signer_seeds);

    for push in pushes.iter() {
        // The source must be the canonical buffer for the destination's mint.
        // The mint is read from the destination account; the borrow is released
        // at the end of this block, before the transfer touches it again.
        let mint = {
            let destination = TokenAccount::from_account_view(push.destination)
                .map_err(|_| SettlementError::BuyTokenAccountInvalid)?;
            *destination.mint().as_array()
        };
        // Re-derive the buffer with the bump the push carries (one hash) rather
        // than searching for the canonical bump. A buffer exists only at its
        // canonical address, so a wrong bump derives an address the transfer
        // below can't draw from.
        let derived = Address::create_program_address(
            &buffer_pda_signer_seeds(&mint, &[push.bump]),
            program_id,
        )
        .map_err(|_| SettlementError::PushSourceNotBuffer)?;
        if push.source_buffer.address() != &derived {
            return Err(SettlementError::PushSourceNotBuffer.into());
        }

        Transfer::new(
            push.source_buffer,
            push.destination,
            state_pda_account,
            u64::from_be_bytes(*push.amount),
        )
        .invoke_signed(core::slice::from_ref(&state_pda_signer))?;
    }

    Ok(())
}
