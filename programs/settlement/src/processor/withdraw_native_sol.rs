//! `WithdrawNativeSol` instruction handler.
//!
//! The native SOL buffer is owned by this program, so its lamports are moved
//! by editing balances directly, as `FinalizeSettle` does. The requested amount
//! is capped to the lamports above the buffer's rent-exempt minimum, so the
//! buffer always stays alive and a balance that shrank since the transaction
//! was built lowers the payout instead of reverting it.

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{withdraw_native_sol::WithdrawNativeSolInput, InstructionInputParsing},
    pda::{buffer::NATIVE_SOL_BUFFER_PDA, state::validate_is_state_pda},
    Pubkey, Role, SettlementError,
};
use pinocchio::{
    error::ProgramError,
    sysvars::{rent::Rent, Sysvar},
    AccountView, ProgramResult,
};

use crate::processor::utils::lamports::move_lamports;

pub fn process_withdraw_native_sol(
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let WithdrawNativeSolInput {
        state_pda,
        authority,
        native_sol_buffer,
        recipient,
        amount,
    } = WithdrawNativeSolInput::parse(instruction_data, accounts)?;

    validate_is_state_pda(state_pda.address().as_array())?;

    let settlement_owned_order_authority: Pubkey =
        StateAccount::from_account(state_pda)?.authority(Role::SettlementOwnedOrderAuthority);
    if !authority.is_signer() || authority.address() != &settlement_owned_order_authority {
        return Err(SettlementError::UnauthorizedNativeSolWithdrawal.into());
    }

    if native_sol_buffer.address() != &NATIVE_SOL_BUFFER_PDA {
        return Err(SettlementError::NativeSolBufferMismatch.into());
    }

    // Cap the withdrawal to the lamports above the buffer's rent-exempt
    // minimum, so the buffer always survives and a balance smaller than the
    // caller expected shrinks the payout instead of reverting it.
    let withdrawable = native_sol_buffer
        .lamports()
        .checked_sub(Rent::get()?.try_minimum_balance(native_sol_buffer.data_len())?)
        // Basically unreachable: a live buffer is rent-exempt unless the rent
        // mechanism itself changed under it.
        .ok_or(ProgramError::AccountNotRentExempt)?;
    let amount = amount.min(withdrawable);

    // A copied `AccountView` writes through to the same runtime account.
    let mut native_sol_buffer = *native_sol_buffer;
    let mut recipient = *recipient;
    move_lamports(&mut native_sol_buffer, &mut recipient, amount)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::state::fixtures::state_account_bytes;
    use cow_settlement_interface::data::state::StateInitArgs;
    use cow_settlement_interface::fixtures::pubkey_from_seed;
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_with_data, fake_sequential_accounts, fake_signer,
    };
    use cow_settlement_interface::instruction::withdraw_native_sol::fixtures::{
        withdraw_native_sol_data, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::pda::state::STATE_PDA;
    use pinocchio::error::ProgramError;
    use std::sync::LazyLock;

    use super::*;

    // Positions within [`base_accounts`], for the tests that swap one entry.
    const STATE_ACCOUNT: usize = 0;
    const AUTHORITY: usize = 1;
    const NATIVE_SOL_BUFFER: usize = 2;
    const RECIPIENT: usize = 3;

    const BUFFER_LAMPORTS: u64 = 1_000;
    const RECIPIENT_LAMPORTS: u64 = 50;

    static SETTLEMENT_OWNED_ORDER_AUTHORITY: LazyLock<Pubkey> =
        LazyLock::new(|| pubkey_from_seed("settlement-owned-order authority"));

    /// The [`StateInitArgs`] planted by [`base_accounts`].
    fn base_init_args() -> StateInitArgs {
        StateInitArgs {
            manager: pubkey_from_seed("base_init_args's unused manager"),
            solver_authority: pubkey_from_seed("base_init_args's unused solver authority"),
            reclaim_authority: pubkey_from_seed("base_init_args's unused reclaim authority"),
            settlement_owned_order_authority: *SETTLEMENT_OWNED_ORDER_AUTHORITY,
        }
    }

    /// Accounts for a withdrawal authorized by
    /// [`SETTLEMENT_OWNED_ORDER_AUTHORITY`], each one well-formed, with the
    /// buffer holding [`BUFFER_LAMPORTS`] and the recipient
    /// [`RECIPIENT_LAMPORTS`].
    fn base_accounts() -> [AccountView; NUM_ACCOUNTS] {
        let mut accounts = [
            fake_account_with_data(STATE_PDA, &state_account_bytes(&base_init_args(), &[])),
            fake_signer(*SETTLEMENT_OWNED_ORDER_AUTHORITY),
            fake_account(NATIVE_SOL_BUFFER_PDA),
            fake_account(pubkey_from_seed("recipient")),
        ];
        accounts[NATIVE_SOL_BUFFER].set_lamports(BUFFER_LAMPORTS);
        accounts[RECIPIENT].set_lamports(RECIPIENT_LAMPORTS);
        accounts
    }

    #[track_caller]
    fn assert_rejects(
        mut accounts: [AccountView; NUM_ACCOUNTS],
        amount: u64,
        expected: ProgramError,
    ) {
        assert_eq!(
            process_withdraw_native_sol(&mut accounts, &withdraw_native_sol_data(amount)),
            Err(expected),
        );
    }

    #[test]
    fn process_withdraw_native_sol_propagates_parse_error() {
        let mut data = withdraw_native_sol_data(1);
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_withdraw_native_sol(&mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_withdraw_native_sol_rejects_wrong_state_pda() {
        let mut accounts = base_accounts();
        accounts[STATE_ACCOUNT] = fake_account_with_data(
            pubkey_from_seed("unrelated"),
            &state_account_bytes(&base_init_args(), &[]),
        );
        assert_rejects(
            accounts,
            31337,
            SettlementError::StateAccountMismatch.into(),
        );
    }

    #[test]
    fn process_withdraw_native_sol_rejects_uninitialized_state_pda() {
        let mut accounts = base_accounts();
        accounts[STATE_ACCOUNT] = fake_account(STATE_PDA);
        assert_rejects(accounts, 31337, ProgramError::InvalidAccountData);
    }

    #[test]
    fn process_withdraw_native_sol_rejects_wrong_authority() {
        let mut accounts = base_accounts();
        accounts[AUTHORITY] = fake_signer(pubkey_from_seed("unrelated"));
        assert_rejects(
            accounts,
            31337,
            SettlementError::UnauthorizedNativeSolWithdrawal.into(),
        );
    }

    #[test]
    fn process_withdraw_native_sol_rejects_nonsigner_authority() {
        let mut accounts = base_accounts();
        // `fake_account`, unlike `fake_signer`, leaves the signer flag clear.
        accounts[AUTHORITY] = fake_account(*SETTLEMENT_OWNED_ORDER_AUTHORITY);
        assert_rejects(
            accounts,
            31337,
            SettlementError::UnauthorizedNativeSolWithdrawal.into(),
        );
    }

    #[test]
    fn process_withdraw_native_sol_rejects_wrong_native_sol_buffer() {
        let mut accounts = base_accounts();
        accounts[NATIVE_SOL_BUFFER] = fake_account(pubkey_from_seed("unrelated"));
        accounts[NATIVE_SOL_BUFFER].set_lamports(BUFFER_LAMPORTS);
        assert_rejects(
            accounts,
            31337,
            SettlementError::NativeSolBufferMismatch.into(),
        );
    }
}
