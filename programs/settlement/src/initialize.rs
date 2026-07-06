//! `Initialize` instruction handler.

use pinocchio::{AccountView, Address, ProgramResult};
use settlement_interface::{
    data::state::EncodedStateAccount,
    instruction::{initialize::InitializeInput, InstructionInputParsing},
    pda::state::state_pda_seeds,
};

use crate::processor::CanonicalPda;

pub fn process_initialize(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let InitializeInput {
        payer,
        state_pda,
        receiver,
    } = InitializeInput::parse(instruction_data, accounts)?;

    // There are no explicit account guards here: `create_canonical_pda` rejects
    // any `state_pda`  other than the address those seeds derive and guards
    // against re-init.
    // The system program is invoked by its fixed address, so the account in that
    // system program is invoked by its slot is never referenced directly.

    CanonicalPda {
        program_id,
        payer,
        pda: state_pda,
        size: EncodedStateAccount::SIZE as u64,
        owner: program_id,
        seeds: state_pda_seeds(),
    }
    .create()?;

    state_pda
        .try_borrow_mut()?
        .copy_from_slice(&receiver.to_bytes());

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
