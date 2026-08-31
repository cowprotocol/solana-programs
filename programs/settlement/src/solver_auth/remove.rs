//! `RemoveSolver` instruction handler.
//!
//! Removes a solver from the sorted solver list that follows the state PDA
//! header, shifting the tail left to close the gap and shrinking the account.
//! Only the manager may authorize it, and the freed rent is paid to
//! `rent_recipient`. The refund is a direct lamport move out of the
//! program-owned state PDA, so no system program is involved.

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{solver_auth::remove::RemoveSolverInput, InstructionInputParsing},
    Role, SettlementError,
};
use pinocchio::{
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    AccountView, Address, ProgramResult, Resize,
};

use crate::processor::{check_state_pda, move_lamports};

pub fn process_remove_solver(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let RemoveSolverInput {
        manager,
        rent_recipient,
        state_pda,
        solver,
    } = RemoveSolverInput::parse(instruction_data, accounts)?;

    check_state_pda(program_id, state_pda)?;

    let mut state_pda = *state_pda;
    let new_len = {
        let mut state = StateAccount::attach(state_pda.try_borrow_mut()?)?;
        if !manager.is_signer() || *manager.address() != state.authority(Role::Manager) {
            return Err(SettlementError::UnauthorizedSolverManagement.into());
        }
        state.remove_solver(&solver)?
    };
    state_pda.resize(new_len)?;

    // Refund the rent the smaller account no longer needs to `rent_recipient`.
    // The state PDA is program-owned, so the program may debit it directly.
    let surplus = state_pda
        .lamports()
        .checked_sub(Rent::get()?.try_minimum_balance(new_len)?)
        // The failure case is basically unreachable unless there are some
        // protocol changes to the rent mechanism.
        .ok_or(ProgramError::AccountNotRentExempt)?;
    let mut rent_recipient = *rent_recipient;
    move_lamports(&mut state_pda, &mut rent_recipient, surplus)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::fixtures::PROGRAM_ID;
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use cow_settlement_interface::instruction::solver_auth::remove::fixtures::{
        remove_solver_data, NUM_ACCOUNTS,
    };
    use pinocchio::error::ProgramError;

    #[test]
    fn process_remove_solver_propagates_parse_error() {
        let mut data = remove_solver_data();
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_remove_solver(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_remove_solver_rejects_non_canonical_state_pda() {
        // `fake_sequential_accounts` puts the state PDA at some arbitrary
        // address, which is not the canonical state PDA for this program.
        let data = remove_solver_data();
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_remove_solver(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;
        use cow_settlement_interface::data::state::fixtures::{
            arb_init_params, state_account_bytes,
        };
        use cow_settlement_interface::fixtures::pubkey_from_seed;
        use cow_settlement_interface::instruction::fixtures::{
            fake_account, fake_account_owned_by, fake_signer,
        };
        use cow_settlement_interface::instruction::solver_auth::remove::RemoveSolver;
        use cow_settlement_interface::pda::state::find_state_pda;
        use cow_settlement_interface::{Instruction, Pubkey};

        proptest! {
            #[test]
            fn process_remove_solver_rejects_an_absent_solver(
                header in arb_init_params(),
                // Unique and already sorted, being a `BTreeSet`.
                raw_solvers in ::proptest::collection::btree_set(any::<[u8; 32]>(), 0..50),
                raw_absent in any::<[u8; 32]>(),
            ) {
                prop_assume!(!raw_solvers.contains(&raw_absent));
                let manager = header.manager;
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                let absent = Pubkey::new_from_array(raw_absent);

                // Mock the three accounts the handler parses. Only the manager
                // signer and the state PDA carry meaning here; the rent recipient
                // is never touched, since the reject happens before the refund.
                let (state_pda_address, _bump) = find_state_pda(&PROGRAM_ID);
                let mut accounts = [
                    fake_signer(manager),
                    fake_account(pubkey_from_seed("rent recipient")),
                    fake_account_owned_by(
                        state_pda_address,
                        *PROGRAM_ID,
                        &state_account_bytes(&header, &stored),
                    ),
                ];

                let data = Instruction::from(RemoveSolver {
                    program_id: *PROGRAM_ID,
                    manager,
                    rent_recipient: pubkey_from_seed("rent recipient"),
                    state_pda: state_pda_address,
                    solver: absent,
                })
                .data;

                prop_assert_eq!(
                    process_remove_solver(&PROGRAM_ID, &mut accounts, &data),
                    Err(SettlementError::SolverNotFound.into()),
                );
            }
        }
    }
}
