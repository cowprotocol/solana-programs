//! `CancelOrder` instruction handler.

use cow_settlement_interface::{
    data::{order::OrderAccount, state::StateAccount},
    instruction::{cancel_order::CancelOrderInput, InstructionInputParsing},
    Role, SettlementError,
};
use pinocchio::{error::ProgramError, AccountView, Address, ProgramResult};

use crate::pda::state::{validate_is_state_pda, STATE_PDA};
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
        state_pda,
    } = CancelOrderInput::parse(instruction_data, accounts)?;

    if !owner.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if order_pda.owned_by(program_id) {
        // The order already exists: flip its cancelled flag in place. Its
        // stored body is authoritative, so any intent bytes on the wire are
        // ignored.
        let mut order_pda = *order_pda;
        let mut order = OrderAccount::load_from_pda_mut(&mut order_pda, program_id)?;
        let intent = OrderIntentAccessor::from_order(&order)?;

        // Is this a settlement owned order?
        if intent.owner() == STATE_PDA.as_array() {
            let state_pda = state_pda.ok_or(ProgramError::NotEnoughAccountKeys)?;
            validate_is_state_pda(state_pda.address().as_array())?;
            let authority = StateAccount::from_account(state_pda)?
                .authority(Role::SettlementOwnedOrderAuthority);
            if owner.address() != &authority {
                return Err(SettlementError::UnauthorizedSettlementOwnedOrder.into());
            }
        } else if owner.address().as_array() != intent.owner() {
            return Err(SettlementError::OwnerMismatch.into());
        }
        order.set_cancelled();
        Ok(())
    } else {
        // The order doesn't exist yet. Then we create the input intent in an
        // already cancelled state. A settlement-owned intent is rejected here,
        // since no signer can match the state PDA as owner.
        let Some(intent_bytes) = intent_bytes else {
            return Err(ProgramError::UninitializedAccount);
        };
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
    use crate::pda::ID as PROGRAM_ID;
    use cow_settlement_interface::data::intent::{EncodedOrderIntent, OrderIntent};
    use cow_settlement_interface::data::order::fixtures::{
        sample_order_fields, OrderFields, CANCELLED_OFFSET,
    };
    use cow_settlement_interface::data::order::SIZE;
    use cow_settlement_interface::data::state::fixtures::state_account_bytes;
    use cow_settlement_interface::data::state::StateInitArgs;
    use cow_settlement_interface::fixtures::pubkey_from_seed;
    use cow_settlement_interface::instruction::cancel_order::fixtures::{
        default_cancel_data, valid_intent_bytes, NUM_ACCOUNTS,
    };
    use cow_settlement_interface::instruction::fixtures::{
        fake_account, fake_account_owned_by, fake_account_with_data, fake_sequential_accounts,
        fake_signer,
    };
    use cow_settlement_interface::pda::order::find_order_pda;
    use std::sync::LazyLock;

    use super::*;

    static SETTLEMENT_OWNED_ORDER_AUTHORITY: LazyLock<Address> =
        LazyLock::new(|| pubkey_from_seed("settlement-owned-order authority"));

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
            fake_account_owned_by(pda_address, PROGRAM_ID, &fields.encode()[..]),
            fake_signer(pubkey_from_seed("system program")),
        ]
    }

    /// Accounts for cancelling an existing order owned by the state PDA, signed
    /// by `signer` and with `state_account` as the trailing state PDA.
    fn accounts_for_settlement_owned(
        signer: Address,
        state_account: AccountView,
    ) -> ([AccountView; NUM_ACCOUNTS + 1], OrderFields) {
        let fields = sample_order_fields(false);
        let intent = OrderIntent {
            owner: STATE_PDA,
            ..fields.intent
        };
        let (pda_address, bump) = find_order_pda(&PROGRAM_ID, &intent.uid());
        let fields = OrderFields {
            bump,
            intent,
            ..fields
        };
        let accounts = [
            fake_signer(signer),
            fake_account(pubkey_from_seed("created by")),
            fake_account_owned_by(pda_address, PROGRAM_ID, &fields.encode()[..]),
            fake_account(pubkey_from_seed("system program")),
            state_account,
        ];
        (accounts, fields)
    }

    /// The canonical state PDA, recording [`SETTLEMENT_OWNED_ORDER_AUTHORITY`].
    fn state_account() -> AccountView {
        let init_args = StateInitArgs {
            manager: pubkey_from_seed("unused manager"),
            solver_authority: pubkey_from_seed("unused solver authority"),
            reclaim_authority: pubkey_from_seed("unused reclaim authority"),
            settlement_owned_order_authority: *SETTLEMENT_OWNED_ORDER_AUTHORITY,
        };
        fake_account_with_data(STATE_PDA, &state_account_bytes(&init_args, &[]))
    }

    #[test]
    fn process_cancel_order_propagates_parse_error() {
        let intent_bytes = valid_intent_bytes();
        let mut data = default_cancel_data(Some(intent_bytes));
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
        let data = default_cancel_data(Some(intent_bytes));
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

        let data = default_cancel_data(Some(valid_intent_bytes()));
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

        let data = default_cancel_data(Some(valid_intent_bytes()));
        let mut accounts = accounts_for_existing(owner, &fields, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Ok(()),
        );
        let data = accounts[2].try_borrow().expect("order data readable");
        assert_eq!(data[CANCELLED_OFFSET], 1, "order must be marked cancelled");
    }

    #[test]
    fn process_cancel_order_recovers_intent_from_existing_pda() {
        let (fields, pda_address) = canonical_fields(false);
        let owner = fields.intent.owner;

        // No intent bytes on the wire: the order's data is recovered from the PDA.
        let data = default_cancel_data(None);
        let mut accounts = accounts_for_existing(owner, &fields, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Ok(()),
        );
        let data = accounts[2].try_borrow().expect("order data readable");
        assert_eq!(data[CANCELLED_OFFSET], 1, "order must be marked cancelled");
    }

    #[test]
    fn process_cancel_order_ignores_wire_intent_for_existing_order() {
        let (fields, pda_address) = canonical_fields(false);
        let owner = fields.intent.owner;

        let another_intent = [0xff; EncodedOrderIntent::SIZE];
        let data = default_cancel_data(Some(another_intent));
        let mut accounts = accounts_for_existing(owner, &fields, pda_address);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Ok(()),
        );

        // Only the cancelled flag flips; the stored intent (and the rest of the
        // body) is untouched.
        let (cancelled_fields, _) = canonical_fields(true);
        assert_eq!(
            &accounts[2].try_borrow().expect("order data readable")[..],
            &cancelled_fields.encode()[..],
            "the wire intent must not leak into the stored order"
        );
    }

    #[test]
    fn process_cancel_order_rejects_missing_intent_for_nonexistent_order() {
        // No intent bytes and no order PDA to recover from: there's nothing to
        // cancel, and no data to create the order from either.
        let data = default_cancel_data(None);
        let mut accounts = [
            fake_signer(pubkey_from_seed("owner")),
            fake_signer(pubkey_from_seed("created by")),
            fake_account(pubkey_from_seed("missing order pda")),
            fake_signer(pubkey_from_seed("system program")),
        ];

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
            Err(ProgramError::UninitializedAccount),
        );
    }

    #[test]
    fn process_cancel_order_is_idempotent_on_cancelled_order() {
        let (fields, pda_address) = canonical_fields(true);
        let owner = fields.intent.owner;
        let before: [u8; SIZE] = fields.encode();

        let data = default_cancel_data(Some(valid_intent_bytes()));
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

    #[test]
    fn process_cancel_order_lets_authority_cancel_settlement_owned_order() {
        let (mut accounts, fields) =
            accounts_for_settlement_owned(*SETTLEMENT_OWNED_ORDER_AUTHORITY, state_account());

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &default_cancel_data(None)),
            Ok(()),
        );
        let expected = OrderFields {
            cancelled: true,
            ..fields
        };
        assert_eq!(
            &accounts[2].try_borrow().expect("order data readable")[..],
            &expected.encode()[..],
            "only the cancelled flag may change"
        );
    }

    #[test]
    fn process_cancel_order_rejects_wrong_authority_for_settlement_owned_order() {
        let (mut accounts, _) =
            accounts_for_settlement_owned(pubkey_from_seed("unrelated"), state_account());

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &default_cancel_data(None)),
            Err(SettlementError::UnauthorizedSettlementOwnedOrder.into()),
        );
    }

    #[test]
    fn process_cancel_order_rejects_settlement_owned_order_without_state_pda() {
        let (mut accounts, _) =
            accounts_for_settlement_owned(*SETTLEMENT_OWNED_ORDER_AUTHORITY, state_account());

        assert_eq!(
            process_cancel_order(
                &PROGRAM_ID,
                &mut accounts[..NUM_ACCOUNTS],
                &default_cancel_data(None)
            ),
            Err(ProgramError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn process_cancel_order_rejects_non_canonical_state_pda_for_settlement_owned_order() {
        let impostor_state = fake_account(pubkey_from_seed("impostor state"));
        let (mut accounts, _) =
            accounts_for_settlement_owned(*SETTLEMENT_OWNED_ORDER_AUTHORITY, impostor_state);

        assert_eq!(
            process_cancel_order(&PROGRAM_ID, &mut accounts, &default_cancel_data(None)),
            Err(SettlementError::StateAccountMismatch.into()),
        );
    }
}
