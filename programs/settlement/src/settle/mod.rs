//! `BeginSettle`/`FinalizeSettle` instruction handlers.

use std::ops::Deref;

use pinocchio::{
    cpi::Seed, error::ProgramError, sysvars::instructions::Instructions, AccountView, Address,
    ProgramResult,
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

/// The seed material for the settlement state PDA signer.
///
/// A `Signer` is only a view over the seed buffers it borrows, so those buffers
/// must outlive it. This owns the one runtime-derived seed (the bump) as plain
/// bytes, which are safe to move out of the validating constructor; a reference
/// turns into the `[Seed; 2]` array a `Signer` borrows via `.into()`.
struct StatePdaSigner {
    bump: [u8; 1],
}

impl<'a> From<&'a StatePdaSigner> for [Seed<'a>; 2] {
    fn from(state_pda_signer: &'a StatePdaSigner) -> Self {
        let [seed] = state_pda_seeds();
        [seed, &state_pda_signer.bump].map(Seed::from)
    }
}

/// Validate that `state_pda_account` is the canonical state PDA and capture its
/// bump. The state PDA must sign each settlement transfer, as the token
/// authority over the users' and buffers' accounts.
fn validated_state_pda_signer(
    program_id: &Address,
    state_pda_account: &AccountView,
) -> Result<StatePdaSigner, ProgramError> {
    let (state_pda, state_bump) = Address::find_program_address(&state_pda_seeds(), program_id);
    if state_pda_account.address() != &state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }
    Ok(StatePdaSigner { bump: [state_bump] })
}
