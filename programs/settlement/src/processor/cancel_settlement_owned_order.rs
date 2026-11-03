//! `CancelSettlementOwnedOrder` instruction handler.
//!
//! Cancels an order owned by the settlement state PDA, so the fee-withdrawal
//! orders placed with `CreateSettlementOwnedOrder` can be taken off the market.
//! The processing is very similar to the existing-order path of `CancelOrder`,
//! but the state PDA can't sign, so the cancellation is gated by the
//! [`Role::SettlementOwnedOrderAuthority`] instead, and the order's owner must
//! be the state PDA.

use cow_settlement_interface::{
    data::{order::OrderAccount, state::StateAccount},
    instruction::{
        cancel_settlement_owned_order::CancelSettlementOwnedOrderInput, InstructionInputParsing,
    },
    Pubkey, Role, SettlementError,
};

use crate::pda::state::validate_is_state_pda;
use crate::processor::utils::intent::OrderIntentAccessor;
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

pub fn process_cancel_settlement_owned_order(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let CancelSettlementOwnedOrderInput {
        authority,
        state_pda,
        order_pda,
    } = CancelSettlementOwnedOrderInput::parse(instruction_data, accounts)?;

    validate_is_state_pda(state_pda.address().as_array())?;

    // Only the settlement-owned-order authority may cancel settlement-owned
    // orders.
    let settlement_owned_order_authority: Pubkey =
        StateAccount::from_account(state_pda)?.authority(Role::SettlementOwnedOrderAuthority);
    if !authority.is_signer() || authority.address() != &settlement_owned_order_authority {
        return Err(SettlementError::UnauthorizedSettlementOwnedOrder.into());
    }

    // Settlement-owned orders only come into existence through
    // `CreateSettlementOwnedOrder`, so there's nothing to cancel ahead of it.
    if !order_pda.owned_by(program_id) {
        return Err(ProgramError::UninitializedAccount);
    }
    let mut order_pda = *order_pda;
    let mut order = OrderAccount::load_from_pda_mut(&mut order_pda, program_id)?;
    let intent = OrderIntentAccessor::from_order(&order)?;
    if state_pda.address().as_array() != intent.owner() {
        return Err(SettlementError::OwnerMismatch.into());
    }
    order.set_cancelled();
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::pda::state::STATE_PDA;
    use crate::pda::ID as PROGRAM_ID;
    use cow_settlement_interface::data::intent::OrderIntent;
    use cow_settlement_interface::data::order::fixtures::{
        sample_order_fields, OrderFields, CANCELLED_OFFSET,
    };
    use cow_settlement_interface::data::state::fixtures::state_account_bytes;
    use cow_settlement_interface::data::state::StateInitArgs;
    use cow_settlement_interface::fixtures::pubkey_from_seed;
    use cow_settlement_interface::instruction::cancel_settlement_owned_order::fixtures::{
        cancel_settlement_owned_order_data, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_owned_by, fake_account_with_data, fake_sequential_accounts,
        fake_signer,
    };
    use cow_settlement_interface::pda::order::find_order_pda;
    use std::sync::LazyLock;

    use super::*;

    // Positions within [`accounts_for`], for the tests that swap an account.
    const AUTHORITY: usize = 0;
    const ORDER_PDA: usize = 2;

    static SETTLEMENT_OWNED_ORDER_AUTHORITY: LazyLock<Address> =
        LazyLock::new(|| pubkey_from_seed("settlement-owned-order authority"));

    /// The [`StateInitArgs`] planted by [`accounts_for`].
    fn base_init_args() -> StateInitArgs {
        StateInitArgs {
            manager: pubkey_from_seed("base_init_args's unused manager"),
            solver_authority: pubkey_from_seed("base_init_args's unused solver authority"),
            reclaim_authority: pubkey_from_seed("base_init_args's unused reclaim authority"),
            settlement_owned_order_authority: *SETTLEMENT_OWNED_ORDER_AUTHORITY,
        }
    }

    /// [`sample_order_fields`] owned by `owner` and carrying its own canonical
    /// bump, plus the address of the PDA it belongs at.
    fn canonical_fields(owner: Pubkey, cancelled: bool) -> (OrderFields, Address) {
        let fields = sample_order_fields(cancelled);
        let intent = OrderIntent {
            owner,
            ..fields.intent
        };
        let (pda_address, bump) = find_order_pda(&PROGRAM_ID, &intent.uid());
        (
            OrderFields {
                bump,
                intent,
                ..fields
            },
            pda_address,
        )
    }

    /// Accounts for cancelling the given existing order, authorized by
    /// [`SETTLEMENT_OWNED_ORDER_AUTHORITY`], each one well-formed.
    fn accounts_for(fields: &OrderFields, pda_address: Address) -> [AccountView; NUM_ACCOUNTS] {
        [
            fake_signer(*SETTLEMENT_OWNED_ORDER_AUTHORITY),
            fake_account_with_data(STATE_PDA, &state_account_bytes(&base_init_args(), &[])),
            fake_account_owned_by(pda_address, PROGRAM_ID, &fields.encode()[..]),
        ]
    }

    #[test]
    fn sanity_check_accounts_at_expected_position() {
        let (fields, pda_address) = canonical_fields(STATE_PDA, false);
        let accounts = accounts_for(&fields, pda_address);
        assert_eq!(
            *accounts[AUTHORITY].address(),
            *SETTLEMENT_OWNED_ORDER_AUTHORITY,
            "the AUTHORITY variable doesn't point to the settlement-owned-order authority"
        );
        assert_eq!(
            *accounts[ORDER_PDA].address(),
            pda_address,
            "the ORDER_PDA variable doesn't point to the order PDA"
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_propagates_parse_error() {
        let mut data = cancel_settlement_owned_order_data();
        data.push(0); // trailing byte triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_cancel_settlement_owned_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_rejects_non_canonical_state_pda() {
        // `fake_sequential_accounts` puts the state PDA at an arbitrary
        // address, which isn't the canonical state PDA for this program.
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();
        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_rejects_wrong_authority() {
        let (fields, pda_address) = canonical_fields(STATE_PDA, false);
        let mut accounts = accounts_for(&fields, pda_address);
        accounts[AUTHORITY] = fake_signer(pubkey_from_seed("unrelated"));
        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Err(SettlementError::UnauthorizedSettlementOwnedOrder.into()),
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_rejects_nonsigner_authority() {
        let (fields, pda_address) = canonical_fields(STATE_PDA, false);
        let mut accounts = accounts_for(&fields, pda_address);
        // `fake_account`, unlike `fake_signer`, leaves the signer flag clear.
        accounts[AUTHORITY] = fake_account(*SETTLEMENT_OWNED_ORDER_AUTHORITY);
        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Err(SettlementError::UnauthorizedSettlementOwnedOrder.into()),
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_rejects_order_not_owned_by_state_pda() {
        // A user's order: the authority must not be able to cancel it.
        let (fields, pda_address) = canonical_fields(pubkey_from_seed("user"), false);
        let mut accounts = accounts_for(&fields, pda_address);
        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Err(SettlementError::OwnerMismatch.into()),
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_rejects_missing_order() {
        let (fields, pda_address) = canonical_fields(STATE_PDA, false);
        let mut accounts = accounts_for(&fields, pda_address);
        // Not owned by the program: there's no order to cancel.
        accounts[ORDER_PDA] = fake_account(pda_address);
        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Err(ProgramError::UninitializedAccount),
        );
    }

    #[test]
    fn process_cancel_settlement_owned_order_marks_active_order_cancelled() {
        let (fields, pda_address) = canonical_fields(STATE_PDA, false);
        let mut accounts = accounts_for(&fields, pda_address);

        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Ok(()),
        );

        // Only the cancelled flag flips; the rest of the body is untouched.
        let (cancelled_fields, _) = canonical_fields(STATE_PDA, true);
        let data = accounts[ORDER_PDA]
            .try_borrow()
            .expect("order data readable");
        assert_eq!(data[CANCELLED_OFFSET], 1, "order must be marked cancelled");
        assert_eq!(&data[..], &cancelled_fields.encode()[..]);
    }

    #[test]
    fn process_cancel_settlement_owned_order_is_idempotent_on_cancelled_order() {
        let (fields, pda_address) = canonical_fields(STATE_PDA, true);
        let before = fields.encode();
        let mut accounts = accounts_for(&fields, pda_address);

        assert_eq!(
            process_cancel_settlement_owned_order(
                &PROGRAM_ID,
                &mut accounts,
                &cancel_settlement_owned_order_data()
            ),
            Ok(()),
        );
        let after = accounts[ORDER_PDA]
            .try_borrow()
            .expect("order data readable");
        assert_eq!(
            &after[..],
            &before[..],
            "cancelling again must not change the order"
        );
    }
}
