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
    // account, the search finds where the new solver sorts in, and the state
    // knows the length it must grow to.
    let (index, new_len) = {
        let state = StateAccount::attach(state_pda.try_borrow()?)?;
        if !manager.is_signer() || manager.address() != &state.authority(Role::Manager) {
            return Err(SettlementError::UnauthorizedSolverManagement.into());
        }
        let index = match state.solver_search(&solver) {
            Ok(_) => return Err(SettlementError::SolverAlreadyExists.into()),
            Err(index) => index,
        };
        (
            index,
            state
                .grown_len()
                .expect("grown account length fits in usize"),
        )
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

    let mut state_pda = *state_pda;
    state_pda.resize(new_len)?;
    let mut state = StateAccount::attach(state_pda.try_borrow_mut()?)?;
    state.insert_solver_at(index, &solver);

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

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;
        use cow_settlement_interface::data::state::fixtures::{arb_header, state_account_bytes};
        use cow_settlement_interface::fixtures::pubkey_from_seed;
        use cow_settlement_interface::instruction::add_solver::AddSolver;
        use cow_settlement_interface::instruction::fixtures::{
            fake_account, fake_account_owned_by, fake_signer,
        };
        use cow_settlement_interface::pda::state::find_state_pda;
        use cow_settlement_interface::{Instruction, Pubkey};

        proptest! {
            #[test]
            fn process_add_solver_rejects_an_existing_solver(
                header in arb_header(),
                // BTreeSet: `.iter()` returns the elements already sorted, and,
                // since it's a set, they are also unique. At least one so there's
                // an existing solver to re-add.
                raw_solvers in ::proptest::collection::btree_set(any::<[u8; 32]>(), 1..50),
                pick in any::<::proptest::sample::Index>(),
            ) {
                let manager = header.manager;
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                // Re-add one of the solvers that's already stored.
                let existing = stored[pick.index(stored.len())];

                // Mock the four accounts the handler parses. Only the manager signer
                // and the state PDA carry meaning here; the payer and system program
                // are never touched, since the reject happens before the
                // rent-funding transfer.
                let (state_pda_address, _bump) = find_state_pda(&PROGRAM_ID);
                let mut accounts = [
                    fake_signer(manager),
                    fake_account(pubkey_from_seed("payer")),
                    fake_account_owned_by(
                        state_pda_address,
                        PROGRAM_ID,
                        &state_account_bytes(&header, &stored),
                    ),
                    fake_account(pubkey_from_seed("system program")),
                ];

                let data = Instruction::from(AddSolver {
                    program_id: PROGRAM_ID,
                    manager,
                    payer: pubkey_from_seed("payer"),
                    state_pda: state_pda_address,
                    solver: existing,
                })
                .data;

                prop_assert_eq!(
                    process_add_solver(&PROGRAM_ID, &mut accounts, &data),
                    Err(SettlementError::SolverAlreadyExists.into()),
                );
            }
        }
    }
}
