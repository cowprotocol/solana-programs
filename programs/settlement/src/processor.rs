//! Shared program plumbing: canonical PDA creation.

use pinocchio::{
    address::MAX_SEEDS,
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};

use pinocchio_system::instructions::CreateAccountAllowPrefund;

use cow_settlement_interface::{
    data::state::StateAccount,
    pda::state::{state_pda_seeds, state_pda_signer_seeds},
    SettlementError,
};
use solana_instruction::{syscalls::get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};

/// Description of a canonical PDA to create: the account at `pda`, assigned to
/// `owner` and funded by `payer`.
///
/// `seeds` are the canonical PDA seeds *without* the bump; the canonical bump
/// is derived under `program_id` and appended in [`Self::create_idempotent`].
/// Signing `CreateAccount` with these seeds implicitly checks that `pda` is the
/// canonical address: the runtime grants the PDA signature only for the address
/// the seeds derive, so any other `pda` fails the CPI.
///
/// `owner` is the program the new account is assigned to. It is usually
/// `program_id` (a program-owned PDA), but differs when the account must be
/// owned by another program, as for example a buffer token account owned by the
/// SPL Token program.
pub struct CanonicalPda<'a, const N: usize> {
    pub program_id: &'a Address,
    pub payer: &'a AccountView,
    pub pda: &'a AccountView,
    pub size: u64,
    pub owner: &'a Address,
    pub seeds: [&'a [u8]; N],
}

impl<const N: usize> CanonicalPda<'_, N> {
    /// Create the canonical account, or do nothing if it already exists.
    /// Returns `true` if it created the account, `false` if it already existed.
    ///
    /// Idempotent on the same input so two parties racing to create the same
    /// buffer or order both succeed instead of one failing with
    /// `AccountAlreadyInUse`.
    ///
    /// Here we assume that we will never create an account for the same seeds
    /// but for a different owner. If the caller does, then the function
    /// still tries to create the account, which then reverts with the system
    /// program's `AccountAlreadyInUse` because the account already exists.
    #[must_use = "the flag says whether follow-up initialization is still needed"]
    pub fn create_idempotent(self) -> Result<(bool, u8), ProgramError> {
        let (canonical, bump) = Address::find_program_address(&self.seeds, self.program_id);

        // Verify whether the PDA is initialized.
        // We do that through `owned_by` because only this program can set this
        // property. This is more reliable than, for example, checking that the
        // data is empty since some PDA may be initialized with zero size.
        // In principle we could just check that owner != system_program.
        // Here instead we compare it with the input owner. This is so that if
        // the same account is initialized twice with different owners then
        // execution eventually revert instead of silently accepting an existing
        // PDA using a different owner than expected.
        // In doing this this, we also assume that self.owner isn't the System
        // Program, which is the default owner of all accounts that weren't
        //  initialized. We take the risk as this isn't user-specified input and
        // there's no reason to actually assign a PDA to the System Program.
        if self.pda.address() == &canonical && self.pda.owned_by(self.owner) {
            return Ok((false, bump));
        }

        // A PDA has at most `MAX_SEEDS` seeds, so `N` stays well below
        // `usize::MAX` and the `N + 1` below cannot overflow. Asserting it in a
        // `const` block makes that a compile-time guarantee rather than a
        // runtime risk.
        const { assert!(N < MAX_SEEDS, "a PDA has at most MAX_SEEDS seeds") };

        // The signer needs the base seeds followed by the bump. Stable Rust
        // can't name `[Seed; N + 1]`, so collect into a `Vec` sized for exactly
        // that: the `N` base seeds plus the trailing bump.
        let mut signer_seeds = Vec::with_capacity(const { N + 1 });
        signer_seeds.extend(self.seeds.iter().map(|seed| Seed::from(*seed)));
        let bump_ptr = [bump];
        signer_seeds.push(Seed::from(&bump_ptr));
        let signer = Signer::from(&signer_seeds[..]);

        // `CreateAccountAllowPrefund` mirrors `CreateAccount` but does not
        // assert that the target starts with zero lamports. A PDA address is
        // publicly derivable, so without this anyone could permanently block
        // creation by first sending the address a single lamport, which makes
        // the System program's plain `CreateAccount` revert with
        // `AccountAlreadyInUse`.
        CreateAccountAllowPrefund::with_minimum_balance(
            self.payer, self.pda, self.size, self.owner, None,
        )?
        .invoke_signed(&[signer])?;
        Ok((true, bump))
    }

    /// Create the canonical account, reverting with
    /// [`ProgramError::AccountAlreadyInitialized`] if it already exists.
    #[must_use = "ignoring the output means the PDA could be incorrectly set up"]
    pub fn create_new(self) -> Result<u8, ProgramError> {
        let (created, bump) = self.create_idempotent()?;
        if created {
            Ok(bump)
        } else {
            Err(ProgramError::AccountAlreadyInitialized)
        }
    }
}

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
/// PDA (e.g. via [`check_state_pda`] or [`require_solver`]).
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
/// [`require_solver`]) to avoid re-deriving the PDA.
pub fn with_state_pda_signer(
    program_id: &Address,
    state_pda_account: &AccountView,
    f: impl FnOnce(&Signer) -> ProgramResult,
) -> ProgramResult {
    with_state_pda_signer_from_bump(check_state_pda(program_id, state_pda_account)?, f)
}

/// Confirm that the given account is a valid solver.
///
/// Returns the state PDA's canonical bump, so the caller can build its signer via
/// [`with_state_pda_signer_from_bump`] without deriving the PDA a second time.
#[must_use = "ignoring the result skips solver authentication"]
pub fn require_solver(
    program_id: &Address,
    state_pda_account: &AccountView,
    solver_account: &AccountView,
) -> Result<u8, ProgramError> {
    let state_bump = check_state_pda(program_id, state_pda_account)?;
    let state = StateAccount::attach(state_pda_account.try_borrow()?)?;
    if !solver_account.is_signer() || !state.is_solver(solver_account.address()) {
        return Err(SettlementError::UnauthorizedSolver.into());
    }
    Ok(state_bump)
}

pub fn is_cpi_call() -> bool {
    get_stack_height() > TRANSACTION_LEVEL_STACK_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cpi_false_outside_solana_lib() {
        assert!(!is_cpi_call());
    }
}
