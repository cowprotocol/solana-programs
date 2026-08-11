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
    use settlement_interface::instruction::fixtures::{
        fake_account, fake_account_with_data, fake_sequential_accounts, fake_signer,
    };
    use settlement_interface::instruction::reclaim_buffer::fixtures::{
        reclaim_buffer_data, NUM_SHARED_ACCOUNTS,
    };
    use settlement_interface::pda::state::state_pda_seeds;

    use super::*;

    const PROGRAM_ID: Address = Address::new_from_array([1; 32]);
    /// The `reclaim_authority` recorded in the state PDA these tests encode.
    const AUTHORITY: Address = Address::new_from_array([2; 32]);
    const RECIPIENT: Address = Address::new_from_array([8; 32]);
    const MINT: Address = Address::new_from_array([5; 32]);

    /// Number of accounts in a one-buffer reclaim: the shared ones plus a
    /// single `(buffer_pda, mint)` pair.
    const NUM_ACCOUNTS: usize = NUM_SHARED_ACCOUNTS + 2;

    // Positions within [`base_accounts`], for the tests that swap one entry.
    const STATE_PDA: usize = 0;
    const RECLAIM_AUTHORITY: usize = 1;
    const TOKEN_PROGRAM: usize = 3;
    const BUFFER_PDA: usize = 4;

    /// Accounts for reclaiming a single buffer, each one well-formed.
    fn base_accounts() -> [AccountView; NUM_ACCOUNTS] {
        [
            fake_account_with_data(
                Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0,
                &*EncodedStateAccount::from(StateAccount {
                    reclaim_authority: AUTHORITY,
                }),
            ), // state PDA
            fake_signer(AUTHORITY),             // reclaim authority
            fake_account(RECIPIENT),            // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID), // token program
            fake_account(find_buffer_pda(&PROGRAM_ID, &MINT).0), // buffer PDA
            fake_account(MINT),                 // mint
        ]
    }

    #[track_caller]
    fn assert_rejects(mut accounts: [AccountView; NUM_ACCOUNTS], expected: ProgramError) {
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &reclaim_buffer_data()),
            Err(expected),
        );
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
        let mut accounts = base_accounts();
        accounts[TOKEN_PROGRAM] = fake_account(Address::new_from_array([3; 32]));
        assert_rejects(accounts, ProgramError::IncorrectProgramId);
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_state_pda() {
        let mut accounts = base_accounts();
        accounts[STATE_PDA] = fake_account_with_data(
            Address::new_from_array([6; 32]),
            &*EncodedStateAccount::from(StateAccount {
                reclaim_authority: AUTHORITY,
            }),
        );
        assert_rejects(accounts, SettlementError::StateAccountMismatch.into());
    }

    #[test]
    fn process_reclaim_buffer_rejects_uninitialized_state_pda() {
        let mut accounts = base_accounts();

        // The canonical state PDA address, but nothing was ever written there:
        // the account carries no data at all. Its `reclaim_authority` is
        // unknowable, so no caller can be authorized.
        let state_pda = *accounts[STATE_PDA].address();
        accounts[STATE_PDA] = fake_account(state_pda);

        assert_rejects(accounts, ProgramError::InvalidAccountData);
    }

    #[test]
    fn process_reclaim_buffer_rejects_zeroed_state_pda() {
        let mut accounts = base_accounts();

        // Allocated to the right size but never initialized, so the
        // discriminator byte is still zero. Being the right size is not
        // enough: the account also has to be *ours*.
        let state_pda = *accounts[STATE_PDA].address();
        accounts[STATE_PDA] = fake_account_with_data(state_pda, &[0; EncodedStateAccount::SIZE]);

        assert_rejects(accounts, ProgramError::InvalidAccountData);
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_reclaim_authority() {
        let mut accounts = base_accounts();
        // A different, unauthorized signer.
        accounts[RECLAIM_AUTHORITY] = fake_signer(Address::new_from_array([7; 32]));
        assert_rejects(accounts, SettlementError::ReclaimAuthorityMismatch.into());
    }

    #[test]
    fn process_reclaim_buffer_rejects_nonsigner_reclaim_authority() {
        let mut accounts = base_accounts();
        // `fake_account`, unlike `fake_signer`, leaves the signer flag clear
        accounts[RECLAIM_AUTHORITY] = fake_account(AUTHORITY);
        assert_rejects(accounts, SettlementError::ReclaimAuthorityMismatch.into());
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_buffer_pda() {
        let mut accounts = base_accounts();
        // Not the buffer PDA derived from the paired mint.
        accounts[BUFFER_PDA] = fake_account(Address::new_from_array([4; 32]));
        assert_rejects(accounts, SettlementError::ReclaimBufferNotCanonical.into());
    }

    #[test]
    fn process_reclaim_buffer_rejects_uninitialized_buffer_pda() {
        // Nothing to swap in: the base buffer already sits at the canonical
        // address with no token account created there. A buffer that can't be
        // read as a token account is an error rather than a silent skip: only
        // a *funded* buffer is skipped, and there is no balance here to judge
        // that by.
        assert_rejects(base_accounts(), ProgramError::InvalidAccountData);
    }
}
