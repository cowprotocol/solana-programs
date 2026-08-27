//! `AddSolver` instruction handler.
//!
//! Inserts a solver into the sorted solver list that follows the state PDA
//! header, keeping it sorted so the list stays binary-searchable. Only the
//! manager may authorize it. The account grows by one solver, so the `payer`
//! funds the extra rent through a `Transfer` before the account is resized.

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{add_solver::AddSolverInput, InstructionInputParsing},
    Role, SettlementError,
};
use pinocchio::{
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult, Resize,
};
use pinocchio_system::instructions::Transfer;

use crate::processor::check_state_pda;

pub fn process_add_solver(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let AddSolverInput {
        manager,
        payer,
        state_pda,
        solver,
    } = AddSolverInput::parse(instruction_data, accounts)?;

    check_state_pda(program_id, state_pda)?;

    // Only the manager may change the solver list. Reading also validates the
    // account and tells us the length it must grow to. This handler decides only
    // whether the caller may add a solver; where the solver sorts in and whether
    // it's a duplicate is left to `StateAccount::insert_solver` below.
    let new_len = {
        let state = StateAccount::attach(state_pda.try_borrow()?)?;
        if !manager.is_signer() || manager.address() != &state.authority(Role::Manager) {
            return Err(SettlementError::UnauthorizedSolverManagement.into());
        }
        state
            .grown_len()
            .expect("grown account length fits in usize")
    };

    let shortfall = Rent::get()?
        .try_minimum_balance(new_len)?
        // why saturating: if there's more balance available than rent needed,
        // then there's no shortfall, that is, `shortfall == 0``.
        .saturating_sub(state_pda.lamports());
    if shortfall > 0 {
        Transfer {
            from: payer,
            to: state_pda,
            lamports: shortfall,
        }
        .invoke()?;
    }

    // Optimistically grow the account by one solver slot, then hand the insert
    // to the state accessor. If the solver turns out to already be present,
    // `insert_solver` writes nothing and returns `SolverAlreadyExists`; the `?`
    // aborts the instruction and the runtime rolls the growth (and the rent
    // transfer) back. This is deliberately inefficient — we fund and resize even
    // for a rejected duplicate — but keeps the list-mutation logic in one place.
    let mut state_pda = *state_pda;
    state_pda.resize(new_len)?;
    let mut state = StateAccount::attach(state_pda.try_borrow_mut()?)?;
    state.insert_solver(&solver)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::add_solver::fixtures::{
        add_solver_data, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use pinocchio::error::ProgramError;

    const PROGRAM_ID: Address = Address::new_from_array([0xc0; 32]);

    #[test]
    fn process_add_solver_propagates_parse_error() {
        let mut data = add_solver_data();
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_add_solver(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_add_solver_rejects_non_canonical_state_pda() {
        // `fake_sequential_accounts` puts the state PDA at `[3; 32]`, which is
        // not the canonical state PDA for this program.
        let data = add_solver_data();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_add_solver(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }

    // The duplicate-solver rejection used to be a proptest here, against the
    // pure handler. Under the optimistic flow the reject happens only after
    // `Rent::get()` and the rent-funding CPI, neither of which runs off-chain,
    // so that property now lives where the logic moved: it's a proptest over
    // `StateAccount::insert_solver` in `interface/src/data/state.rs`, and it's
    // still exercised end-to-end by the `add_solvers` integration tests.
}
