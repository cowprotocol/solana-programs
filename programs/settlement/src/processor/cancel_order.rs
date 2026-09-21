//! `CancelOrder` instruction handler.

use cow_settlement_interface::{
    data::order::{EncodedOrderAccount, OrderAccount},
    instruction::{cancel_order::CancelOrderInput, InstructionInputParsing},
    SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::processor::create_order::process_new_onchain_order;

pub fn process_cancel_order(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let CancelOrderInput {
        intent_bytes,
        owner,
        created_by,
        order_pda,
    } = CancelOrderInput::parse(instruction_data, accounts)?;

    // Only the owner can cancel their order; the owner's signature authenticates
    // the cancellation in both branches below.
    if !owner.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if order_pda.owned_by(program_id) {
        // The order already exists: mark it cancelled. `load_from_pda` proves
        // `order_pda` is the canonical PDA for the intent it stores, so binding
        // authorization to that stored owner means a signer can't cancel someone
        // else's order by pairing its PDA with an intent they happen to own. The
        // passed intent isn't consulted here.
        let account = OrderAccount::load_from_pda(order_pda, program_id)?;
        if owner.address() != &account.intent.owner {
            return Err(SettlementError::OwnerMismatch.into());
        }
        if !account.cancelled {
            let updated: [u8; EncodedOrderAccount::SIZE] =
                EncodedOrderAccount::from(OrderAccount {
                    cancelled: true,
                    ..account
                })
                .into();
            // A copied `AccountView` handle writes through to the same runtime account.
            let mut order_pda = *order_pda;
            order_pda.try_borrow_mut()?.copy_from_slice(&updated);
        }
        // An already-cancelled order is left untouched: cancelling is idempotent.
        Ok(())
    } else {
        // The order doesn't exist yet: create it already cancelled rather than
        // leaving nothing behind. `process_new_onchain_order` validates the owner
        // signature against the intent, the on-chain authentication flag, and the
        // canonical PDA, then funds the rent from `created_by`.
        process_new_onchain_order(
            program_id,
            (order_pda, &intent_bytes),
            owner.address(),
            created_by,
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::order::fixtures::{sample_account, CANCELLED_OFFSET};
    use cow_settlement_interface::data::order::{EncodedOrderAccount, OrderAccount};
    use cow_settlement_interface::fixtures::PROGRAM_ID;
    use cow_settlement_interface::instruction::cancel_order::fixtures::{
        default_cancel_data, valid_intent_bytes, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::{
        fake_account_owned_by, fake_sequential_accounts, fake_signer,
    };
    use cow_settlement_interface::pda::order::find_order_pda;

    use super::*;

    /// [`sample_account`] carrying its own canonical bump, plus the address of
    /// the PDA it belongs at.
    fn canonical_account(cancelled: bool) -> (OrderAccount, Address) {
        let mut account = sample_account(cancelled);
        let (pda_address, bump) = find_order_pda(&PROGRAM_ID, &account.intent.uid());
        account.bump = bump;
        (account, pda_address)
    }

    /// Accounts for cancelling the given existing order: a signing `owner`, an
    /// unused `created_by`, the program-owned order PDA carrying `account`, and a
    /// placeholder system program.
    fn accounts_for_existing(
        owner: Address,
        account: &OrderAccount,
        pda_address: Address,
    ) -> [AccountView; NUM_ACCOUNTS] {
        [
            fake_signer(owner),
            fake_signer(Address::new_from_array([0x24; 32])),
            fake_account_owned_by(
                pda_address,
                *PROGRAM_ID,
                &EncodedOrderAccount::from(account.clone())[..],
            ),
            fake_signer(Address::default()),
        ]
    }

    #[test]
    fn process_cancel_order_propagates_parse_error() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_cancel_data(&intent_bytes);
        data.pop(); // fewer bytes than necessary triggers a parse error
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn process_cancel_order_rejects_nonsigner_owner() {
        let intent_bytes = valid_intent_bytes();
        let data = default_cancel_data(&intent_bytes);
        // `fake_sequential_accounts` leaves the owner (index 0) unsigned.
        let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::MissingRequiredSignature),
        );
    }

    #[test]
    fn process_cancel_order_rejects_owner_mismatch_on_existing_order() {
        let (account, pda_address) = canonical_account(false);
        let wrong_owner = Address::new_from_array([0x67; 32]);
        assert_ne!(wrong_owner, account.intent.owner);

        let data = default_cancel_data(&valid_intent_bytes());
        let mut accounts = accounts_for_existing(wrong_owner, &account, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::OwnerMismatch.into()),
        );
    }

    #[test]
    fn process_cancel_order_marks_active_order_cancelled() {
        let (account, pda_address) = canonical_account(false);
        let owner = account.intent.owner;

        let data = default_cancel_data(&valid_intent_bytes());
        let mut accounts = accounts_for_existing(owner, &account, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Ok(()),
        );
        let data = accounts[2].try_borrow().expect("order data readable");
        assert_eq!(data[CANCELLED_OFFSET], 1, "order must be marked cancelled");
    }

    #[test]
    fn process_cancel_order_is_idempotent_on_cancelled_order() {
        let (account, pda_address) = canonical_account(true);
        let owner = account.intent.owner;
        let before: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(account.clone()).into();

        let data = default_cancel_data(&valid_intent_bytes());
        let mut accounts = accounts_for_existing(owner, &account, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Ok(()),
        );
        let after = accounts[2].try_borrow().expect("order data readable");
        assert_eq!(
            &after[..],
            &before[..],
            "cancelling again must not change the order"
        );
    }
}
