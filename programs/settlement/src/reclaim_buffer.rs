//! `ReclaimBuffer` instruction handler.
//!
//! A buffer that still holds tokens is skipped rather than closed: an SPL
//! token account can only be closed once its balance is zero, and this
//! instruction never moves or destroys that balance. Skipping is silent, so
//! reclaiming a set of buffers succeeds even when none of them were closed.

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

pub fn process_reclaim_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimBufferInput {
        state_pda,
        reclaim_authority,
        reclaim_recipient,
        token_program,
        buffers,
    } = ReclaimBufferInput::parse(instruction_data, accounts)?;

    if token_program.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    with_state_pda_signer(program_id, state_pda, |state_signer| {
        let reclaim_authority_pubkey: Pubkey = {
            let data = state_pda.try_borrow()?;
            let bytes: &[u8; EncodedStateAccount::SIZE] = (&*data)
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?;
            StateAccount::try_from(*bytes)?.reclaim_authority
        };
        if !reclaim_authority.is_signer()
            || reclaim_authority.address() != &reclaim_authority_pubkey
        {
            return Err(SettlementError::ReclaimAuthorityMismatch.into());
        }

        for [buffer_pda, mint] in buffers {
            let expected_buffer_pda = find_buffer_pda(program_id, mint.address()).0;

            if buffer_pda.address() != &expected_buffer_pda {
                return Err(SettlementError::ReclaimBufferNotCanonical.into());
            }

            let amount = TokenAccount::from_account_view(buffer_pda)
                .map_err(|_| ProgramError::InvalidAccountData)?
                .amount();

            // A token account can't be closed while it still holds a balance, and this
            // instruction has no mandate to move those tokens elsewhere or destroy them.
            // Leave the buffer standing and reclaim whatever else was asked for.
            if amount > 0 {
                continue;
            }

            CloseAccount::new(buffer_pda, reclaim_recipient, state_pda)
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
    /// The `reclaim_authority` recorded in the state PDA these tests encode.
    const AUTHORITY: Address = Address::new_from_array([2; 32]);

    /// Build the canonical state PDA encoding for `reclaim_authority`: the
    /// discriminator byte followed by `reclaim_authority`'s bytes.
    fn encoded_state(reclaim_authority: Address) -> [u8; EncodedStateAccount::SIZE] {
        let mut bytes = [0u8; EncodedStateAccount::SIZE];
        bytes[0] = EncodedStateAccount::DISCRIMINATOR;
        bytes[1..].copy_from_slice(&reclaim_authority.to_bytes());
        bytes
    }

    /// A fake `AccountView` for `address` that reports as a transaction
    /// signer, as `reclaim_authority` must for `ReclaimBuffer` to accept it.
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
        let authority_address = AUTHORITY;
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &encoded_state(authority_address),
            ), // state PDA
            fake_signer(authority_address), // reclaim authority
            fake_account(Address::new_from_array([8; 32])), // reclaim recipient
            fake_account(Address::new_from_array([3; 32])), // **wrong** token program
            fake_account(Address::new_from_array([4; 32])), // buffer PDA
            fake_account(Address::new_from_array([5; 32])), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_state_pda() {
        let data = reclaim_buffer_data();
        let authority_address = AUTHORITY;
        let mut accounts = [
            fake_account_with_data(
                Address::new_from_array([6; 32]),
                &encoded_state(authority_address),
            ), // state PDA
            fake_signer(authority_address), // reclaim authority
            fake_account(Address::new_from_array([8; 32])), // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_from_array([4; 32])), // buffer PDA
            fake_account(Address::new_from_array([5; 32])), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_reclaim_authority() {
        let data = reclaim_buffer_data();
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &encoded_state(AUTHORITY),
            ), // state PDA
            fake_signer(Address::new_from_array([7; 32])), // a different, unauthorized signer
            fake_account(Address::new_from_array([8; 32])), // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_from_array([4; 32])), // buffer PDA
            fake_account(Address::new_from_array([5; 32])), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::ReclaimAuthorityMismatch.into()),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_nonsigner_reclaim_authority() {
        let data = reclaim_buffer_data();
        let authority_account = fake_account(AUTHORITY);

        // Test setup: the address is the configured authority, but it doesn't
        // sign. Naming the authority must not be enough to authorize a close.
        assert!(!authority_account.is_signer());

        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &encoded_state(AUTHORITY),
            ), // state PDA
            authority_account, // reclaim authority, **not** a signer
            fake_account(Address::new_from_array([8; 32])), // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_from_array([4; 32])), // buffer PDA
            fake_account(Address::new_from_array([5; 32])), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::ReclaimAuthorityMismatch.into()),
        );
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_buffer_pda() {
        let data = reclaim_buffer_data();
        let authority_address = AUTHORITY;
        let mut accounts = [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &encoded_state(authority_address),
            ), // state PDA
            fake_signer(authority_address), // reclaim authority
            fake_account(Address::new_from_array([8; 32])), // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID),
            fake_account(Address::new_from_array([4; 32])), // buffer PDA
            fake_account(Address::new_from_array([5; 32])), // mint
        ];
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::ReclaimBufferNotCanonical.into()),
        );
    }
}
