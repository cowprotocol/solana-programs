//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use std::ops::Deref;

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    sysvars::instructions::Instructions,
    AccountView, Address, ProgramResult,
};
use settlement_interface::{
    instruction::settle::recover_counterpart, pda::state::state_pda_seeds, recover_discriminator,
    SettlementError, SettlementInstruction,
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

/// Validate that `state_pda_account` is the canonical state PDA, returning a
/// signer for that PDA. Both settlement transfers move funds under the state
/// PDA's authority, so it must sign each of them.
///
/// A `Signer` only borrows its seed buffers, so they must outlive it. Building
/// them on this frame's stack would confine the signer to this function (they
/// would dangle once it returns), so they are allocated on the program heap
/// instead. That heap is a bump allocator the runtime discards wholesale when
/// the instruction ends, so these allocations are reclaimed with it and never
/// accumulate across invocations.
fn validated_state_pda_signer(
    program_id: &Address,
    state_pda_account: &AccountView,
) -> Result<Signer<'static, 'static>, ProgramError> {
    let (state_pda, state_bump) = Address::find_program_address(&state_pda_seeds(), program_id);
    if state_pda_account.address() != &state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }

    let bump: &'static [u8] = Box::leak(Box::new([state_bump]));
    let [seed] = state_pda_seeds();
    let signer_seeds: &'static [Seed<'static>; 2] =
        Box::leak(Box::new([seed, bump].map(Seed::from)));
    Ok(Signer::from(signer_seeds))
}
