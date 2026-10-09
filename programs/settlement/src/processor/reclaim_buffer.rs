//! `ReclaimBuffer` instruction handler.

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{
        reclaim_buffer::{Buffer, ReclaimBufferInput},
        InstructionInputParsing,
    },
    pda::{buffer::find_buffer_pda, state::validate_is_state_pda},
    Pubkey, Role, SettlementError,
};
use pinocchio::{AccountView, Address, ProgramResult};
use pinocchio_token::instructions::{Burn, CloseAccount};

use crate::processor::utils::{
    auth::with_state_pda_signer,
    token::{owning_token_program, read_token_account},
};

pub fn process_reclaim_buffer(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let ReclaimBufferInput {
        state_pda,
        reclaim_authority,
        reclaim_recipient,
        buffers,
    } = ReclaimBufferInput::parse(instruction_data, accounts)?;

    validate_is_state_pda(state_pda.address().as_array())?;

    with_state_pda_signer(|state_signer| {
        let reclaim_authority_pubkey: Pubkey =
            StateAccount::from_account(state_pda)?.authority(Role::ReclaimAuthority);
        if !reclaim_authority.is_signer()
            || reclaim_authority.address() != &reclaim_authority_pubkey
        {
            return Err(SettlementError::ReclaimAuthorityMismatch.into());
        }

        // A batch reclaims every buffer it can: a buffer whose balance exceeds
        // its burn limit is skipped rather than failing the rest, and the
        // instruction reverts only when no buffer could be reclaimed. Every
        // other problem still reverts immediately.
        let mut any_reclaimed = false;

        for Buffer {
            buffer_pda,
            mint,
            burn_limit,
        } in buffers.iter()
        {
            let expected_buffer_pda = find_buffer_pda(program_id, mint.address()).0;

            if buffer_pda.address() != &expected_buffer_pda {
                return Err(SettlementError::ReclaimBufferNotCanonical.into());
            }

            // A buffer is burned and closed by the program that owns it, which
            // is the one that created it in the first place.
            let token_program = owning_token_program(buffer_pda)?;
            let amount = read_token_account(token_program, buffer_pda)?.amount;

            // A balance over its limit is more than the caller authorized to
            // destroy, so skip this buffer. Will only revert if no buffer was
            // reclaimed.
            if amount <= burn_limit {
                // Burn the whole balance so the account reaches zero and can be
                // closed; an already-empty buffer needs no burn.
                if amount > 0 {
                    Burn::new(buffer_pda, mint, state_pda, amount)
                        .invoke_signed_with_unverified_program(
                            core::slice::from_ref(state_signer),
                            &token_program.address(),
                        )?;
                }

                CloseAccount::new(buffer_pda, reclaim_recipient, state_pda)
                    .invoke_signed_with_unverified_program(
                        core::slice::from_ref(state_signer),
                        &token_program.address(),
                    )?;

                any_reclaimed = true;
            }
        }

        if any_reclaimed {
            Ok(())
        } else {
            Err(SettlementError::ReclaimBufferBurnLimitExceeded.into())
        }
    })
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::state::fixtures::state_account_bytes;
    use cow_settlement_interface::data::state::{StateInitArgs, WIDTH_HEADER};
    use cow_settlement_interface::fixtures::pubkey_from_seed;
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_owned_by, fake_account_with_data, fake_sequential_accounts,
        fake_signer,
    };
    use cow_settlement_interface::instruction::reclaim_buffer::fixtures::{
        reclaim_buffer_data, NUM_SHARED_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::reclaim_buffer::{
        ReclaimBuffer as ReclaimBufferIx, ACCOUNTS_PER_BUFFER,
    };
    use cow_settlement_interface::pda::state::STATE_PDA;
    use cow_settlement_interface::token_program::TokenProgram;
    use cow_settlement_interface::Instruction;
    use cow_settlement_interface::ID as PROGRAM_ID;
    use litesvm_token::spl_token::state::{Account as SplTokenAccount, AccountState};
    use pinocchio::error::ProgramError;
    use solana_program_pack::Pack;

    use super::*;
    const AUTHORITY: Address = Address::new_from_array([101; 32]);
    const UNRELATED: Address = Address::new_from_array([254; 32]);
    const SPL_TOKEN_PROGRAM_ID: Address = TokenProgram::SplToken.address();

    // Positions within [`base_accounts`], for the tests that swap one entry.
    const STATE_ACCOUNT: usize = 0;
    const RECLAIM_AUTHORITY: usize = 1;
    const BUFFER_PDA: usize = 4;

    /// The [`StateInitArgs`] planted by [`base_accounts`].
    fn base_init_args() -> StateInitArgs {
        StateInitArgs {
            manager: pubkey_from_seed("base_init_args's unused manager"),
            solver_authority: pubkey_from_seed("base_init_args's unused solver authority"),
            reclaim_authority: AUTHORITY,
            settlement_owned_order_authority: pubkey_from_seed(
                "base_init_args's unused settlement-owned-order authority",
            ),
        }
    }

    /// The base layout of a token account holding `amount` of `mint` for
    /// `state_pda`.
    fn buffer_data(mint: Address, state_pda: Address, amount: u64) -> Vec<u8> {
        let mut data = vec![0; SplTokenAccount::LEN];
        SplTokenAccount {
            mint,
            owner: state_pda,
            amount,
            state: AccountState::Initialized,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        data
    }

    /// Accounts for reclaiming one buffer per entry in `amounts`, each holding
    /// its entry's balance and each well-formed and canonical for its own mint.
    fn base_accounts_holding(amounts: &[u64]) -> Vec<AccountView> {
        let recipient = pubkey_from_seed("base_accounts_holding recipient");

        let mut accounts = vec![
            fake_account_with_data(STATE_PDA, &state_account_bytes(&base_init_args(), &[])), // state PDA
            fake_signer(AUTHORITY),             // reclaim authority
            fake_account(recipient),            // reclaim recipient
            fake_account(SPL_TOKEN_PROGRAM_ID), // token program
        ];

        for (index, &amount) in amounts.iter().enumerate() {
            let mint = pubkey_from_seed(&format!("base_accounts_holding mint {index}"));
            accounts.push(fake_account_owned_by(
                find_buffer_pda(&PROGRAM_ID, &mint).0,
                SPL_TOKEN_PROGRAM_ID,
                &buffer_data(mint, STATE_PDA, amount),
            )); // buffer PDA
            accounts.push(fake_account(mint)); // mint
        }

        accounts
    }

    /// Accounts for reclaiming a single empty buffer, each one well-formed.
    fn base_accounts() -> Vec<AccountView> {
        base_accounts_holding(&[0])
    }

    /// `ReclaimBuffer` data carrying one burn limit per entry in `limits`. Only
    /// the limits reach the handler's balance check; the addresses are
    /// placeholders.
    fn reclaim_data_with_limits(limits: &[u64]) -> Vec<u8> {
        let zero = Address::new_from_array([0; 32]);
        let buffers: Vec<(Pubkey, Pubkey, u64)> =
            limits.iter().map(|&limit| (zero, zero, limit)).collect();
        Instruction::from(ReclaimBufferIx {
            program_id: zero,
            state_pda: zero,
            reclaim_authority: zero,
            reclaim_recipient: zero,
            token_program: zero,
            buffers: &buffers,
        })
        .data
    }

    #[track_caller]
    fn assert_rejects_with_data(
        mut accounts: Vec<AccountView>,
        data: &[u8],
        expected: ProgramError,
    ) {
        assert_eq!(
            process_reclaim_buffer(&PROGRAM_ID, &mut accounts, data),
            Err(expected),
        );
    }

    #[track_caller]
    fn assert_rejects(accounts: Vec<AccountView>, expected: ProgramError) {
        assert_rejects_with_data(accounts, &reclaim_buffer_data(), expected);
    }

    #[test]
    fn process_reclaim_buffer_propagates_parse_error() {
        let mut data = reclaim_buffer_data();
        data.push(0); // make the data too long to trigger a parse error
        let mut accounts =
            fake_sequential_accounts::<{ NUM_SHARED_ACCOUNTS + ACCOUNTS_PER_BUFFER }>();
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

    /// A balance over its limit is more than the caller allowed to destroy, so
    /// the whole instruction reverts.
    #[test]
    fn process_reclaim_buffer_rejects_a_balance_above_the_limit() {
        let accounts = base_accounts_holding(&[1_001]);

        assert_rejects_with_data(
            accounts,
            &reclaim_data_with_limits(&[1_000]),
            SettlementError::ReclaimBufferBurnLimitExceeded.into(),
        );
    }

    /// A zero limit forbids burning, so a non-empty buffer reverts rather than
    /// being cleared.
    #[test]
    fn process_reclaim_buffer_rejects_a_nonempty_buffer_under_a_zero_limit() {
        let accounts = base_accounts_holding(&[1]);

        assert_rejects_with_data(
            accounts,
            &reclaim_data_with_limits(&[0]),
            SettlementError::ReclaimBufferBurnLimitExceeded.into(),
        );
    }

    /// A buffer over its limit can't be reclaimed, but a reclaimable buffer later
    /// in the batch still closes, so the instruction succeeds instead of
    /// reverting on the first bad buffer. (A host CPI is a no-op, so the second
    /// buffer's close "succeeds" here; what this proves is that the over-limit
    /// buffer is skipped rather than aborting the batch.)
    #[test]
    fn process_reclaim_buffer_skips_an_over_limit_buffer_when_another_succeeds() {
        // First buffer is over its zero limit (skipped); second is empty and closes.
        let mut accounts = base_accounts_holding(&[1337, 0]);

        process_reclaim_buffer(
            &PROGRAM_ID,
            &mut accounts,
            &reclaim_data_with_limits(&[0, 0]),
        )
        .unwrap_or_else(|err| panic!("a batch with one reclaimable buffer should succeed: {err}"));
    }

    /// When no buffer in the batch can be reclaimed, the instruction reverts with
    /// the first skip reason.
    #[test]
    fn process_reclaim_buffer_reverts_when_every_buffer_is_over_its_limit() {
        let mut accounts = base_accounts_holding(&[1337, 42]);

        assert_eq!(
            process_reclaim_buffer(
                &PROGRAM_ID,
                &mut accounts,
                &reclaim_data_with_limits(&[0, 0])
            ),
            Err(SettlementError::ReclaimBufferBurnLimitExceeded.into()),
        );
    }

    /// The buffer's own owner is what says which program closes it, so one
    /// owned by neither token program is refused: there is nothing to close it
    /// with.
    #[test]
    fn process_reclaim_buffer_rejects_a_buffer_under_an_unrelated_program() {
        let mut accounts = base_accounts();
        let buffer_pda = *accounts[BUFFER_PDA].address();
        accounts[BUFFER_PDA] = fake_account_owned_by(buffer_pda, UNRELATED, &[]);
        assert_rejects(accounts, ProgramError::IncorrectProgramId);
    }

    #[test]
    fn process_reclaim_buffer_rejects_wrong_state_pda() {
        let mut accounts = base_accounts();
        accounts[STATE_ACCOUNT] =
            fake_account_with_data(UNRELATED, &state_account_bytes(&base_init_args(), &[]));
        assert_rejects(accounts, SettlementError::StateAccountMismatch.into());
    }

    #[test]
    fn process_reclaim_buffer_rejects_uninitialized_state_pda() {
        let mut accounts = base_accounts();

        // The canonical state PDA address, but nothing was ever written there:
        // the account carries no data at all. Its `reclaim_authority` is
        // unknowable, so no caller can be authorized.
        let state_pda = *accounts[STATE_ACCOUNT].address();
        accounts[STATE_ACCOUNT] = fake_account(state_pda);

        assert_rejects(accounts, ProgramError::InvalidAccountData);
    }

    #[test]
    fn process_reclaim_buffer_rejects_zeroed_state_pda() {
        let mut accounts = base_accounts();

        // Allocated to the right size but never initialized: its leading byte
        // isn't the state discriminator, so it isn't a valid state account.
        let state_pda = *accounts[STATE_ACCOUNT].address();
        accounts[STATE_ACCOUNT] = fake_account_with_data(state_pda, &[0; WIDTH_HEADER]);

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

    /// A buffer that was never created is owned by the system program, so it
    /// is refused as an account no token program can close rather than read as
    /// a malformed token account.
    #[test]
    fn process_reclaim_buffer_rejects_uninitialized_buffer_pda() {
        let mut accounts = base_accounts();

        let buffer_pda = *accounts[BUFFER_PDA].address();
        accounts[BUFFER_PDA] = fake_account(buffer_pda);

        assert_rejects(accounts, ProgramError::IncorrectProgramId);
    }
}
