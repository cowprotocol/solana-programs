//! Direct lamport movement shared across instruction handlers.

use pinocchio::{error::ProgramError, AccountView, ProgramResult};

/// Move `amount` lamports from `from` to `to` by editing their balances
/// directly, with no system program involved.
///
/// `from` must be program-owned so the program may debit it. Both edits are
/// checked, so a balance that would under- or overflow reverts instead of
/// wrapping.
///
/// Moving lamports onto the account they came from is a no-op.
pub fn move_lamports(from: &mut AccountView, to: &mut AccountView, amount: u64) -> ProgramResult {
    if from.address() == to.address() {
        return Ok(());
    }

    let debited = from
        .lamports()
        .checked_sub(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let credited = to
        .lamports()
        .checked_add(amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    from.set_lamports(debited);
    to.set_lamports(credited);
    Ok(())
}
