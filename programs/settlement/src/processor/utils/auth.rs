//! State-PDA validation, signing, and solver authentication shared across
//! instruction handlers.

use pinocchio::{
    cpi::{Seed, Signer},
    AccountView, ProgramResult,
};

use cow_settlement_interface::{
    data::state::StateAccount,
    pda::state::{STATE_PDA, STATE_PDA_SIGNER_SEEDS},
    SettlementError,
};

/// Confirm `state_pda_account` sits at the state PDA pinned in
/// [`STATE_PDA`], so no derivation runs on-chain.
#[must_use = "ignoring the result skips the canonical state-PDA check"]
pub fn check_state_pda(state_pda_account: &AccountView) -> ProgramResult {
    if state_pda_account.address() != &STATE_PDA {
        return Err(SettlementError::StateAccountMismatch.into());
    }
    Ok(())
}

/// Run `f` with a signer for the state PDA
pub fn with_state_pda_signer(f: impl FnOnce(&Signer) -> ProgramResult) -> ProgramResult {
    let signer_seeds = STATE_PDA_SIGNER_SEEDS.map(Seed::from);
    f(&Signer::from(&signer_seeds))
}

/// Confirm that `solver_account` signed the transaction and is in the solver
/// list held by `state_pda_account`.
///
/// Confirming the state account sits at the canonical state PDA is left to the
/// caller, via [`check_state_pda`].
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
