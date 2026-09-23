//! `CancelOrder` instruction handler.

use cow_settlement_interface::{
    data::order::OrderAccount,
    instruction::{cancel_order::CancelOrderInput, InstructionInputParsing},
    SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::processor::create_order::process_new_onchain_order;
use crate::processor::utils::intent::OrderIntentAccessor;

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

    if !owner.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if order_pda.owned_by(program_id) {
        // The order already exists: flip its cancelled flag in place.
        let mut order_pda = *order_pda;
        let mut order = OrderAccount::load_from_pda_mut(&mut order_pda, program_id)?;
        let intent = OrderIntentAccessor::from_order(&order)?;
        if owner.address().as_array() != intent.owner() {
            return Err(SettlementError::OwnerMismatch.into());
        }
        order.set_cancelled();
        Ok(())
    } else {
        // The order doesn't exist yet: create it already cancelled rather than
        // leaving nothing behind. Intent validation is part of the processing.
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
    use cow_settlement_interface::data::order::fixtures::{
        sample_order_fields, OrderFields, CANCELLED_OFFSET,
    };
    use cow_settlement_interface::data::order::SIZE;
    use cow_settlement_interface::fixtures::{pubkey_from_seed, PROGRAM_ID};
    use cow_settlement_interface::instruction::cancel_order::fixtures::{
        default_cancel_data, valid_intent_bytes, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::{
        fake_account_owned_by, fake_sequential_accounts, fake_signer,
    };
    use cow_settlement_interface::pda::order::find_order_pda;

    use super::*;

    /// [`sample_order_fields`] carrying its own canonical bump, plus the address
    /// of the PDA it belongs at.
    fn canonical_fields(cancelled: bool) -> (OrderFields, Address) {
        let fields = sample_order_fields(cancelled);
        let (pda_address, bump) = find_order_pda(&PROGRAM_ID, &fields.intent.uid());
        (OrderFields { bump, ..fields }, pda_address)
    }

    /// Accounts for cancelling the given existing order: a signing `owner`, an
    /// unused `created_by`, the program-owned order PDA carrying `fields`, and a
    /// placeholder system program.
    fn accounts_for_existing(
        owner: Address,
        fields: &OrderFields,
        pda_address: Address,
    ) -> [AccountView; NUM_ACCOUNTS] {
        [
            fake_signer(owner),
            fake_signer(pubkey_from_seed("created by")),
            fake_account_owned_by(pda_address, *PROGRAM_ID, &fields.encode()[..]),
            fake_signer(pubkey_from_seed("system program")),
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
        let (fields, pda_address) = canonical_fields(false);
        let wrong_owner = pubkey_from_seed("wrong owner");
        assert_ne!(wrong_owner, fields.intent.owner);

        let data = default_cancel_data(&valid_intent_bytes());
        let mut accounts = accounts_for_existing(wrong_owner, &fields, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Err(SettlementError::OwnerMismatch.into()),
        );
    }

    #[test]
    fn process_cancel_order_marks_active_order_cancelled() {
        let (fields, pda_address) = canonical_fields(false);
        let owner = fields.intent.owner;

        let data = default_cancel_data(&valid_intent_bytes());
        let mut accounts = accounts_for_existing(owner, &fields, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Ok(()),
        );
        let data = accounts[2].try_borrow().expect("order data readable");
        assert_eq!(data[CANCELLED_OFFSET], 1, "order must be marked cancelled");
    }

    #[test]
    fn process_cancel_order_is_idempotent_on_cancelled_order() {
        let (fields, pda_address) = canonical_fields(true);
        let owner = fields.intent.owner;
        let before: [u8; SIZE] = fields.encode();

        let data = default_cancel_data(&valid_intent_bytes());
        let mut accounts = accounts_for_existing(owner, &fields, pda_address);

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
