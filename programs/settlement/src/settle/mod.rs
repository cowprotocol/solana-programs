//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use std::ops::Deref;

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::instructions::Instructions,
    AccountView, Address, ProgramResult,
};
use settlement_interface::{
    instruction::{create_buffer::SPL_TOKEN_PROGRAM_ID, settle::recover_counterpart},
    pda::state::{state_pda_seeds, state_pda_signer_seeds},
    recover_discriminator, SettlementError, SettlementInstruction,
};

mod begin;
mod finalize;

pub use begin::process_begin_settle;
pub use finalize::process_finalize_settle;

/// Load the counterpart instruction at `counterpart_index` and verify it
/// belongs to `program_id`, carries `expected_discriminator`, and points
/// back at the current instruction. Ordering (before/after) is the caller's
/// responsibility.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn validate_counterpart<T: Deref<Target = [u8]>>(
    program_id: &Address,
    instructions: &Instructions<T>,
    current_index: u16,
    counterpart_index: u16,
    expected_discriminator: SettlementInstruction,
) -> ProgramResult {
    let counterpart_ix = instructions
        .load_instruction_at(usize::from(counterpart_index))
        .map_err(|_| SettlementError::MissingCounterpartInstruction)?;
    if counterpart_ix.get_program_id() != program_id {
        return Err(SettlementError::CounterpartIsExternal.into());
    }
    let counterpart_ix_data = counterpart_ix.get_instruction_data();
    let (their_discriminator, remaining_data) = recover_discriminator(counterpart_ix_data)
        .map_err(|_| SettlementError::InvalidCounterpartDiscriminator)?;
    let (their_counterpart_ix, _) = recover_counterpart(remaining_data)
        .map_err(|_| SettlementError::InvalidCounterpartCounterpart)?;
    if their_discriminator != expected_discriminator || their_counterpart_ix != current_index {
        return Err(SettlementError::MismatchedCounterpartDiscriminator.into());
    }
    Ok(())
}

/// Validate that `token_program_account` is the legacy SPL Token program, which
/// every settlement transfer is issued against.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
fn validate_token_program_account(token_program_account: &AccountView) -> ProgramResult {
    if token_program_account.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}

/// Validate that `state_pda_account` is the canonical state PDA and run `f` with
/// a signer for it. Both settlement transfers move funds under the state PDA's
/// authority, so it must sign each of them.
///
/// The signer only borrows its seed buffers, which are local to this frame;
/// running `f` here rather than returning the signer keeps them alive for as
/// long as `f` needs it.
fn with_state_pda_signer(
    program_id: &Address,
    state_pda_account: &AccountView,
    f: impl FnOnce(&Signer) -> ProgramResult,
) -> ProgramResult {
    let (state_pda, state_bump) = Address::find_program_address(&state_pda_seeds(), program_id);
    if state_pda_account.address() != &state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }

    let state_bump = [state_bump];
    let signer_seeds = state_pda_signer_seeds(&state_bump).map(Seed::from);
    f(&Signer::from(&signer_seeds))
}
