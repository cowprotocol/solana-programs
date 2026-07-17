//! `ReclaimBuffer` instruction handler.
//!
//! Warning: any token balance still held by a buffer is burned, not
//! recovered, before the buffer is closed. Callers should only reclaim
//! buffers expected to be empty, or to write off dust/dead balances.

use pinocchio::{
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};
use pinocchio_token::{
    instructions::{Burn, CloseAccount},
    state::Account as TokenAccount,
};
use settlement_interface::{
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID, reclaim_buffer::ReclaimBufferInput,
        InstructionInputParsing,
    },
    pda::{buffer::buffer_pda_seeds, state::state_pda_seeds},
    SettlementError,
};

struct ReclaimBufferEntry {
    buffer_pda: AccountView,
    mint: AccountView,
}

/// Read one slice element into a [`ReclaimBufferEntry`].
fn read_buffer_entry(&[buffer_pda, mint]: &[AccountView; 2]) -> ReclaimBufferEntry {
    ReclaimBufferEntry { buffer_pda, mint }
}

pub fn process_reclaim_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimBufferInput {
        state_pda,
        receiver,
        token_program,
        buffers,
    } = ReclaimBufferInput::parse(instruction_data, accounts)?;

    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // The buffers' token authority is the settlement state PDA; it signs both
    // the sweep transfer and the close of each buffer below.
    let seeds = state_pda_seeds();
    let (expected_state_pda, state_bump) = Address::find_program_address(&seeds, program_id);
    if state_pda.address() != &expected_state_pda {
        return Err(SettlementError::StateAccountMismatch.into());
    }
    let [seed] = seeds;
    let state_bump = [state_bump];
    let signer_seeds = [seed, &state_bump].map(Seed::from);
    let state_signer = Signer::from(&signer_seeds);

    // Only the `receiver` may trigger a buffer reclaim.
    let stored_receiver: [u8; 32] = {
        let data = state_pda.try_borrow()?;
        (&*data)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?
    };
    if !receiver.is_signer() || receiver.address().as_array() != &stored_receiver {
        return Err(SettlementError::ReceiverMismatch.into());
    }

    for ReclaimBufferEntry { buffer_pda, mint } in buffers.iter().map(read_buffer_entry) {
        let expected_buffer_pda =
            Address::find_program_address(&buffer_pda_seeds(mint.address().as_array()), program_id)
                .0;
        if buffer_pda.address() != &expected_buffer_pda {
            return Err(SettlementError::BufferNotCanonical.into());
        }

        let amount = TokenAccount::from_account_view(&buffer_pda)
            .map_err(|_| ProgramError::InvalidAccountData)?
            .amount();

        // We can't close the account unless the balance is zero, so we burn any tokens we find.
        // Sending the tokens to another account is much more complicated because the receiving
        // account needs to be loaded and likely initialized with rent--all to handle what is likely
        // microdust. So burning is the easiest way to get around this issue.
        if amount > 0 {
            // Using the unchecked burn variant because we would just be reading the decimals
            // off of the mint anyway and we are only looking to erase all tokens.
            Burn::new(&buffer_pda, &mint, state_pda, amount)
                .invoke_signed(core::slice::from_ref(&state_signer))?;
        }

        CloseAccount::new(&buffer_pda, receiver, state_pda)
            .invoke_signed(core::slice::from_ref(&state_signer))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use settlement_interface::instruction::fixtures::{
        fake_account, fake_account_with_data, fake_sequential_accounts,
    };
    use settlement_interface::instruction::reclaim_buffer::fixtures::{
        reclaim_buffer_data, NUM_SHARED_ACCOUNTS,
    };

    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);

    #[test]
    fn process_reclaim_buffer_propagates_parse_error() {
        let mut data = reclaim_buffer_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts = fake_sequential_accounts::<NUM_SHARED_ACCOUNTS>();
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_token_program() {
        let data = reclaim_buffer_data();
        let receiver_address = Address::new_unique();
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &receiver_address.to_bytes(),
            ), // state PDA
            fake_account(receiver_address),      // receiver
            fake_account(Address::new_unique()), // **wrong** token program
            fake_account(Address::new_unique()), // buffer PDA
            fake_account(Address::new_unique()), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_state_pda() {
        let data = reclaim_buffer_data();
        let receiver_address = Address::new_unique();
        let mut accounts = [
            fake_account_with_data(Address::new_unique(), &receiver_address.to_bytes()), // state PDA
            fake_account(receiver_address),                                              // receiver
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_unique()), // buffer PDA
            fake_account(Address::new_unique()), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_receiver() {
        let data = reclaim_buffer_data();
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &Address::new_unique().to_bytes(),
            ), // state PDA
            fake_account(Address::new_unique()), // receiver
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_unique()), // buffer PDA
            fake_account(Address::new_unique()), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::ReceiverMismatch.into()),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_buffer_pda() {
        let data = reclaim_buffer_data();
        let receiver_address = Address::new_unique();
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &receiver_address.to_bytes(),
            ), // state PDA
            fake_account(receiver_address), // receiver
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_unique()), // buffer PDA
            fake_account(Address::new_unique()), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::BufferNotCanonical.into()),
        );
    }
}
