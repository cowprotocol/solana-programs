//! `Initialize` instruction handler.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use settlement_interface::{
    data::state,
    instruction::{initialize::InitializeInput, InstructionInputParsing},
    pda::state::state_pda_seeds,
};

use crate::processor::CanonicalPda;

pub fn process_initialize(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let InitializeInput { payer, state_pda } = InitializeInput::parse(instruction_data, accounts)?;

    // There are no explicit account guards here: `create_canonical_pda` rejects
    // any `state_pda`  other than the address those seeds derive and guards
    // against re-init.
    // The system program is invoked by its fixed address, so the account in that
    // system program is invoked by its slot is never referenced directly.

    CanonicalPda {
        program_id,
        payer,
        pda: state_pda,
        size: state::SIZE as u64,
        owner: program_id,
        seeds: state_pda_seeds(),
    }
    .create()?;

    let mut buffer = state_pda.try_borrow_mut()?;
    let buffer: &mut [u8; state::SIZE] = (&mut *buffer)
        .try_into()
        .map_err(|_| ProgramError::AccountDataTooSmall)?;
    *buffer = state::DISCRIMINATOR;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pinocchio::error::ProgramError;
    use settlement_interface::instruction::fixtures::fake_sequential_accounts;
    use settlement_interface::instruction::initialize::fixtures::{initialize_data, NUM_ACCOUNTS};

    #[test]
    fn process_initialize_propagates_parse_error() {
        let mut data = initialize_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_initialize(&Address::new_unique(), &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }
}
