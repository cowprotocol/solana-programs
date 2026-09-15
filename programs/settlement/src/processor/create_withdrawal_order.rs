//! `CreateWithdrawalOrder` instruction handler.
//!
//! Creates an order owned by the settlement state PDA, so the fees accumulated
//! in the buffer accounts can be sold through a regular settlement. The
//! processing is very similar to that of `CreateOrder`, but the order isn't
//! authenticated by its owner: it's gated by the [`Role::WithdrawalAuthority`],
//! and the program forces the intent owner to be the state PDA so the order
//! can only ever sell funds controlled by the state PDA (notably the buffers).

use cow_settlement_interface::{
    data::state::StateAccount,
    instruction::{create_withdrawal_order::CreateWithdrawalOrderInput, InstructionInputParsing},
    Pubkey, Role, SettlementError,
};
use pinocchio::{AccountView, Address, ProgramResult};

use crate::processor::create_order::process_new_onchain_order;
use crate::processor::utils::auth::check_state_pda;

pub fn process_create_withdrawal_order(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let CreateWithdrawalOrderInput {
        intent_bytes,
        authority,
        payer,
        state_pda,
        order_pda,
    } = CreateWithdrawalOrderInput::parse(instruction_data, accounts)?;

    check_state_pda(program_id, state_pda)?;

    // Only the withdrawal authority may create withdrawal orders.
    let withdrawal_authority: Pubkey =
        StateAccount::from_account(state_pda)?.authority(Role::WithdrawalAuthority);
    if !authority.is_signer() || authority.address() != &withdrawal_authority {
        return Err(SettlementError::UnauthorizedWithdrawalOrder.into());
    }

    process_new_onchain_order(
        program_id,
        (order_pda, &intent_bytes),
        state_pda.address(),
        payer,
    )
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::intent::fixtures::sample_intent;
    use cow_settlement_interface::data::intent::{Flags, OrderIntent};
    use cow_settlement_interface::data::state::fixtures::state_account_bytes;
    use cow_settlement_interface::data::state::StateInitArgs;
    use cow_settlement_interface::fixtures::{pubkey_from_seed, PROGRAM_ID, STATE_PDA};
    use cow_settlement_interface::instruction::create_withdrawal_order::fixtures::{
        withdrawal_order_data, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::create_withdrawal_order::SYSTEM_PROGRAM_ID;
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_with_data, fake_sequential_accounts, fake_signer,
    };
    use pinocchio::error::ProgramError;
    use std::sync::LazyLock;

    use super::*;

    // Position within [`base_accounts`], for the tests that swap the authority.
    const AUTHORITY: usize = 0;

    static WITHDRAWAL_AUTHORITY: LazyLock<Address> =
        LazyLock::new(|| pubkey_from_seed("withdrawal authority"));

    /// The [`StateInitArgs`] planted by [`base_accounts`].
    fn base_init_args() -> StateInitArgs {
        StateInitArgs {
            manager: pubkey_from_seed("base_init_args's unused manager"),
            reclaim_authority: pubkey_from_seed("base_init_args's unused reclaim authority"),
            withdrawal_authority: *WITHDRAWAL_AUTHORITY,
        }
    }

    /// Instruction data for an order owned by `owner` with the given
    /// `created_on_chain` flag; the other fields come from [`sample_intent`].
    fn intent_data(owner: Pubkey, created_on_chain: bool) -> Vec<u8> {
        let intent = OrderIntent {
            owner,
            ..sample_intent(Flags {
                created_on_chain,
                ..Default::default()
            })
        };
        withdrawal_order_data(&intent)
    }

    /// Accounts for a withdrawal order authorized by [`WITHDRAWAL_AUTHORITY`],
    /// each one well-formed. The order PDA is only a placeholder: every test here
    /// stops before the (CPI-based) allocation, so its address doesn't matter.
    fn base_accounts() -> [AccountView; NUM_ACCOUNTS] {
        [
            fake_signer(*WITHDRAWAL_AUTHORITY),
            fake_signer(pubkey_from_seed("base_accounts's payer")),
            fake_account_with_data(*STATE_PDA, &state_account_bytes(&base_init_args(), &[])),
            fake_account(pubkey_from_seed("base_accounts's order pda")),
            fake_account(SYSTEM_PROGRAM_ID),
        ]
    }

    #[test]
    fn sanity_check_authority_at_expected_position() {
        let accounts = base_accounts();
        assert_eq!(
            *accounts[AUTHORITY].address(),
            *WITHDRAWAL_AUTHORITY,
            "the AUTHORITY variable doesn't point to the withdrawal authority"
        );
    }

    #[test]
    fn process_create_withdrawal_order_propagates_parse_error() {
        let mut data = intent_data(*STATE_PDA, true);
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_create_withdrawal_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_create_withdrawal_order_rejects_non_canonical_state_pda() {
        // `fake_sequential_accounts` puts the state PDA at an arbitrary
        // address, which isn't the canonical state PDA for this program.
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_create_withdrawal_order(
                &PROGRAM_ID,
                &mut accounts,
                &intent_data(*STATE_PDA, true)
            ),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }

    #[test]
    fn process_create_withdrawal_order_rejects_wrong_authority() {
        let mut accounts = base_accounts();
        accounts[AUTHORITY] = fake_signer(pubkey_from_seed("unrelated"));
        assert_eq!(
            process_create_withdrawal_order(
                &PROGRAM_ID,
                &mut accounts,
                &intent_data(*STATE_PDA, true)
            ),
            Err(SettlementError::UnauthorizedWithdrawalOrder.into()),
        );
    }

    #[test]
    fn process_create_withdrawal_order_rejects_nonsigner_authority() {
        let mut accounts = base_accounts();
        // `fake_account`, unlike `fake_signer`, leaves the signer flag clear.
        accounts[AUTHORITY] = fake_account(*WITHDRAWAL_AUTHORITY);
        assert_eq!(
            process_create_withdrawal_order(
                &PROGRAM_ID,
                &mut accounts,
                &intent_data(*STATE_PDA, true)
            ),
            Err(SettlementError::UnauthorizedWithdrawalOrder.into()),
        );
    }

    #[test]
    fn process_create_withdrawal_order_rejects_owner_not_state_pda() {
        let mut accounts = base_accounts();
        let data = intent_data(pubkey_from_seed("unrelated owner"), true);
        assert_eq!(
            process_create_withdrawal_order(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::OwnerMismatch.into()),
        );
    }

    #[test]
    fn process_create_withdrawal_order_rejects_intent_not_created_on_chain() {
        let mut accounts = base_accounts();
        // Owned by the state PDA, but not flagged as an on-chain creation.
        let data = intent_data(*STATE_PDA, false);
        assert_eq!(
            process_create_withdrawal_order(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::OrderCreatedOnChainMismatch.into()),
        );
    }
}
