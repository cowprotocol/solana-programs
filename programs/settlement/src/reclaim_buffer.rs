//! `ReclaimBuffer` instruction handler.
//!
//! A buffer that still holds tokens is skipped rather than closed: an SPL
//! token account can only be closed once its balance is zero, and this
//! instruction never moves or destroys that balance. Skipping is silent, so
//! reclaiming a set of buffers succeeds even when none of them were closed.

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{
        create_buffer::SPL_TOKEN_PROGRAM_ID, reclaim_buffer::ReclaimBufferInput,
        InstructionInputParsing,
    },
    pda::buffer::find_buffer_pda,
    Pubkey, Role, SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};
use pinocchio_token::{instructions::CloseAccount, state::Account as TokenAccount};

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
        let reclaim_authority_pubkey: Pubkey =
            StateAccount::from_account(state_pda)?.authority(Role::ReclaimAuthority);
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
    use cow_settlement_interface::data::state::{StateAccount, StateInitArgs, WIDTH_HEADER};
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_owned_by, fake_account_with_data, fake_sequential_accounts,
        fake_signer,
    };
    use cow_settlement_interface::instruction::reclaim_buffer::fixtures::{
        reclaim_buffer_data, NUM_SHARED_ACCOUNTS,
    };
    use cow_settlement_interface::pda::state::state_pda_seeds;
    use litesvm_token::spl_token::state::{Account as SplTokenAccount, AccountState};
    use solana_program_pack::Pack;

    use super::*;

    const PROGRAM_ID: Address = Address::new_from_array([100; 32]);
    const AUTHORITY: Address = Address::new_from_array([101; 32]);
    const MANAGER: Address = Address::new_from_array([102; 32]);
    const UNRELATED: Address = Address::new_from_array([254; 32]);

    /// Number of accounts in a one-buffer reclaim: the shared ones plus a
    /// single `(buffer_pda, mint)` pair.
    const NUM_ACCOUNTS: usize = NUM_SHARED_ACCOUNTS + 2;

    // Positions within [`base_accounts`], for the tests that swap one entry.
    const STATE_PDA: usize = 0;
    const RECLAIM_AUTHORITY: usize = 1;
    const TOKEN_PROGRAM: usize = 3;
    const BUFFER_PDA: usize = 4;

    /// State account bytes for planting a well-formed state PDA in tests.
    fn state_account_bytes(init_args: &StateInitArgs) -> [u8; WIDTH_HEADER] {
        let mut bytes = [0u8; WIDTH_HEADER];
        StateAccount::initialize(&mut bytes[..], init_args).expect("header fits");
        bytes
    }

    /// The [`StateInitArgs`] planted by [`base_accounts`].
    fn base_init_args() -> StateInitArgs {
        StateInitArgs {
            manager: MANAGER,
            reclaim_authority: AUTHORITY,
        }
    }

    fn empty_buffer_data(mint: Address, state_pda: Address) -> Vec<u8> {
        let mut data = vec![0; SplTokenAccount::LEN];
        SplTokenAccount {
            mint,
            owner: state_pda,
            amount: 0,
            state: AccountState::Initialized,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        data
    }

    /// Accounts for reclaiming a single buffer, each one well-formed.
    fn base_accounts() -> [AccountView; NUM_ACCOUNTS] {
        let recipient: Address = Address::new_from_array([1; 32]);
        let mint: Address = Address::new_from_array([2; 32]);
        let state_pda = Address::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0;

        [
            fake_account_with_data(state_pda, &state_account_bytes(&base_init_args())), // state PDA
            fake_signer(AUTHORITY),             // reclaim authority
            fake_account(recipient),            // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID), // token program
            fake_account_owned_by(
                find_buffer_pda(&PROGRAM_ID, &mint).0,
                SPL_TOKEN_PROGRAM_ID,
                &empty_buffer_data(mint, state_pda),
            ), // buffer PDA
            fake_account(mint),                 // mint
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
    fn process_reclaim_buffer_happy_path() {
        let mut accounts = base_accounts();

        process_reclaim_buffer(&PROGRAM_ID, &mut accounts, &reclaim_buffer_data())
            .unwrap_or_else(|err| panic!("reclaim buffer happy path should succeed: {err}"));
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_token_program() {
        let mut accounts = base_accounts();
        accounts[TOKEN_PROGRAM] = fake_account(UNRELATED);
        assert_rejects(accounts, ProgramError::IncorrectProgramId);
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_state_pda() {
        let mut accounts = base_accounts();
        accounts[STATE_PDA] =
            fake_account_with_data(UNRELATED, &state_account_bytes(&base_init_args()));
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

        // Allocated to the right size but never initialized: its leading byte
        // isn't the state discriminator, so it isn't a valid state account.
        let state_pda = *accounts[STATE_PDA].address();
        accounts[STATE_PDA] = fake_account_with_data(state_pda, &[0; WIDTH_HEADER]);

        assert_rejects(accounts, ProgramError::InvalidAccountData);
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_reclaim_authority() {
        let mut accounts = base_accounts();
        // A different, unauthorized signer.
        accounts[RECLAIM_AUTHORITY] = fake_signer(UNRELATED);
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
        accounts[BUFFER_PDA] = fake_account(UNRELATED);
        assert_rejects(accounts, SettlementError::ReclaimBufferNotCanonical.into());
    }

    #[test]
    fn process_reclaim_buffer_rejects_uninitialized_buffer_pda() {
        let mut accounts = base_accounts();

        let buffer_pda = *accounts[BUFFER_PDA].address();
        accounts[BUFFER_PDA] = fake_account(buffer_pda);

        assert_rejects(accounts, ProgramError::InvalidAccountData);
    }
}
