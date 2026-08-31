//! `AddSolver` instruction handler.
//!
//! Inserts a solver into the sorted solver list that follows the state PDA
//! header, keeping it sorted so the list stays binary-searchable, and bumps the
//! header's live-solver count. Only the manager may authorize it. The account
//! reuses a spare slot left by a past removal when one exists; otherwise it
//! grows by one solver and the `payer` funds the extra rent through a `Transfer`
//! before the account is resized.

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

    // Only the manager may change the solver list. Attaching validates the
    // account. If the list has a spare slot from a past removal, reuse it;
    // otherwise `grown_len` is the size the account must reach for one more.
    let grown_len = {
        let state = StateAccount::attach(state_pda.try_borrow()?)?;
        if !manager.is_signer() || manager.address() != &state.authority(Role::Manager) {
            return Err(SettlementError::UnauthorizedSolverManagement.into());
        }
        if state.has_spare_slot() {
            None
        } else {
            Some(
                state
                    .grown_len()
                    .expect("grown account length fits in usize"),
            )
        }
    };

    // When growing, top up the rent for the larger size before resizing.
    if let Some(new_len) = grown_len {
        let shortfall = Rent::get()?
            .try_minimum_balance(new_len)?
            // why saturating: if there's more balance available than rent
            // needed, then there's no shortfall, that is, `shortfall == 0`.
            .saturating_sub(state_pda.lamports());
        if shortfall > 0 {
            Transfer {
                from: payer,
                to: state_pda,
                lamports: shortfall,
            }
            .invoke()?;
        }
    }

    // Grow the account by one solver slot if needed, then insert. A duplicate
    // solver is rejected before anything is written; the error reverts the
    // growth and the rent transfer along with the rest of the instruction.
    let mut state_pda = *state_pda;
    if let Some(new_len) = grown_len {
        state_pda.resize(new_len)?;
    }
    let mut state = StateAccount::attach(state_pda.try_borrow_mut()?)?;
    state.insert_solver(&solver)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::fixtures::PROGRAM_ID;
    use cow_settlement_interface::instruction::add_solver::fixtures::{
        add_solver_data, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use pinocchio::error::ProgramError;

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
        // `fake_sequential_accounts` puts the state PDA at some arbitrary
        // address, which is not the canonical state PDA for this program.
        let data = add_solver_data();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_add_solver(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }
}
