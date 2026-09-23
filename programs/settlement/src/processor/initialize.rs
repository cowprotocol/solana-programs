//! `Initialize` instruction handler.

use cow_settlement_interface::{
    data::state::{StateAccount, StateInitArgs, WIDTH_HEADER},
    instruction::{initialize::InitializeInput, InstructionInputParsing},
    pda::state::STATE_PDA_SEEDS,
};
use pinocchio::{AccountView, Address, ProgramResult};

use crate::processor::utils::{auth::check_state_pda, pda::CanonicalPda};

pub fn process_initialize(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let InitializeInput {
        payer,
        state_pda,
        manager,
        reclaim_authority,
        self_order_authority,
    } = InitializeInput::parse(instruction_data, accounts)?;

    // Sanity to verify that the program constant for STATE_PDA
    // matches up with the address we are about to allocate.
    check_state_pda(state_pda)?;

    // The system program is invoked by its fixed address, so the account in that
    // slot is never referenced directly.
    CanonicalPda {
        program_id,
        payer,
        pda: state_pda,
        size: WIDTH_HEADER as u64,
        owner: program_id,
        seeds: STATE_PDA_SEEDS,
    }
    .create_new()?;

    // A copied `AccountView` handle writes through to the same runtime account.
    let mut state_pda = *state_pda;
    StateAccount::initialize(
        state_pda.try_borrow_mut()?,
        &StateInitArgs {
            manager,
            reclaim_authority,
            self_order_authority,
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cow_settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use cow_settlement_interface::instruction::initialize::fixtures::{
        initialize_data, NUM_ACCOUNTS,
    };
    use pinocchio::error::ProgramError;

    #[test]
    fn process_initialize_propagates_parse_error() {
        let mut data = initialize_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_initialize(&Address::new_from_array([100; 32]), &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }
}
