//! State-PDA validation, signing, and solver authentication shared across
//! instruction handlers.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use cow_settlement_interface::{
    data::state::StateAccount,
    pda::state::{state_pda_seeds, state_pda_signer_seeds},
    SettlementError,
};

/// Confirm `state_pda_account` sits at the canonical state PDA for `program_id`,
/// returning its canonical bump.
#[must_use = "ignoring the result skips the canonical state-PDA check"]
pub fn check_state_pda(
    program_id: &Address,
    state_pda_account: &AccountView,
) -> Result<u8, ProgramError> {
    let (state_pda, state_bump) = Address::find_program_address(&state_pda_seeds(), program_id);
    if state_pda_account.address() != &state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }
    Ok(state_bump)
}

/// Run `f` with a signer for the state PDA, given its already-derived canonical
/// `state_bump`.
///
/// This function is to be used as an alternative for [`with_state_pda_signer`]
/// in the case where the state PDA has been checked in an earlier call.
/// The caller is responsible for having validated the bump against the state
/// PDA, via [`check_state_pda`].
///
/// If state PDA validation is needed, use [`with_state_pda_signer`].
pub fn with_state_pda_signer_from_bump(
    state_bump: u8,
    f: impl FnOnce(&Signer) -> ProgramResult,
) -> ProgramResult {
    let state_bump = [state_bump];
    let signer_seeds = state_pda_signer_seeds(&state_bump).map(Seed::from);
    f(&Signer::from(&signer_seeds))
}
/// Validate that `state_pda_account` is the canonical state PDA and run `f`
/// with a signer for it, in one step. Use [`with_state_pda_signer_from_bump`]
/// directly when the bump has already been derived (as settling does, via
/// [`check_state_pda`]) to avoid re-deriving the PDA.
pub fn with_state_pda_signer(
    program_id: &Address,
    state_pda_account: &AccountView,
    f: impl FnOnce(&Signer) -> ProgramResult,
) -> ProgramResult {
    with_state_pda_signer_from_bump(check_state_pda(program_id, state_pda_account)?, f)
}

/// Confirm that `solver_account` signed the transaction and is in the solver
/// list held by `state_pda_account`.
///
/// Confirming the state account sits at the canonical state PDA (and deriving
/// its bump for the signer) is left to the caller, via [`check_state_pda`].
#[must_use = "ignoring the result skips solver authentication"]
pub fn require_solver(
    state_pda_account: &AccountView,
    solver_account: &AccountView,
) -> ProgramResult {
    let state = StateAccount::attach(state_pda_account.try_borrow()?)?;
    if !solver_account.is_signer() || !state.is_solver(solver_account.address()) {
        return Err(SettlementError::UnauthorizedSolver.into());
    }
    Ok(())
}
