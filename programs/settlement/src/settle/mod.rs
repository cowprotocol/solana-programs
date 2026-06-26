//! `BeginSettle`/`FinalizeSettle` instruction handlers.
//!
//! The handlers themselves live in the [`begin`] and [`finalize`] submodules;
//! this module holds the reciprocity check they share and re-exports their
//! entry points.

use std::ops::Deref;

use pinocchio::{sysvars::instructions::Instructions, Address, ProgramResult};
use settlement_interface::{
    instruction::settle::recover_counterpart, recover_discriminator, SettlementError,
    SettlementInstruction,
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
