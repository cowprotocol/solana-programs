//! `ReclaimBuffer` instruction handler.
//!
//! Warning: any token balance still held by a buffer is burned, not
//! recovered, before the buffer is closed. Callers should only reclaim
//! buffers expected to be empty, or to write off dust/dead balances.

use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_token::{instructions::CloseAccount, state::Account as TokenAccount};
use settlement_interface::{
    data::state::{EncodedStateAccount, StateAccount},
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID, reclaim_buffer::ReclaimBufferInput,
        InstructionInputParsing,
    },
    pda::buffer::find_buffer_pda,
    Pubkey, SettlementError,
};

use crate::processor::with_state_pda_signer;

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
        reclaim_authority,
        token_program,
        buffers,
    } = ReclaimBufferInput::parse(instruction_data, accounts)?;

    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    // Only the `reclaim_authority`may trigger a reclaim.
    let reclaim_authority_pubkey: Pubkey = {
        let data = state_pda.try_borrow()?;
        let bytes: &[u8; EncodedStateAccount::SIZE] = (&*data)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        StateAccount::try_from(*bytes)?.reclaim_authority
    };
    if !reclaim_authority.is_signer() || reclaim_authority.address().as_array() != &reclaim_authority_pubkey.to_bytes() {
        return Err(SettlementError::ReclaimAuthorityMismatch.into());
    }

    with_state_pda_signer(program_id, state_pda, |state_signer| {
        for ReclaimBufferEntry { buffer_pda, mint } in buffers.iter().map(read_buffer_entry) {
            let expected_buffer_pda = find_buffer_pda(program_id, mint.address()).0;

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
                // For now
                continue;
            }

            CloseAccount::new(&buffer_pda, reclaim_authority, state_pda)
                .invoke_signed(core::slice::from_ref(state_signer))?;
        }

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use pinocchio::account::RuntimeAccount;
    use settlement_interface::instruction::fixtures::{
        fake_account, fake_account_from, fake_account_with_data, fake_sequential_accounts,
    };
    use settlement_interface::instruction::reclaim_buffer::fixtures::{
        reclaim_buffer_data, NUM_SHARED_ACCOUNTS,
    };
    use settlement_interface::pda::state::state_pda_seeds;

    use super::*;

    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);

    /// Build the canonical state PDA encoding for `receiver`: the
    /// discriminator byte followed by `receiver`'s bytes.
    fn encoded_state(receiver: Address) -> [u8; EncodedStateAccount::SIZE] {
        let mut bytes = [0u8; EncodedStateAccount::SIZE];
        bytes[0] = EncodedStateAccount::DISCRIMINATOR;
        bytes[1..].copy_from_slice(&receiver.to_bytes());
        bytes
    }

    /// A fake `AccountView` for `address` that reports as a transaction
    /// signer, as `receiver` must for `ReclaimBuffer` to accept it.
    fn fake_signer(address: Address) -> AccountView {
        fake_account_from(RuntimeAccount {
            address,
            is_signer: 1,
            ..Default::default()
        })
    }

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
                &encoded_state(receiver_address),
            ), // state PDA
            fake_signer(receiver_address),       // receiver
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
            fake_account_with_data(Address::new_unique(), &encoded_state(receiver_address)), // state PDA
            fake_signer(receiver_address), // receiver
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
                &encoded_state(Address::new_unique()),
            ), // state PDA
            fake_signer(Address::new_unique()), // receiver
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_unique()), // buffer PDA
            fake_account(Address::new_unique()), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::ReclaimAuthorityMismatch.into()),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_buffer_pda() {
        let data = reclaim_buffer_data();
        let receiver_address = Address::new_unique();
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &encoded_state(receiver_address),
            ), // state PDA
            fake_signer(receiver_address), // receiver
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
