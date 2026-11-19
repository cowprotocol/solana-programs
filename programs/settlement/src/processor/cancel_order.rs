//! `CancelOrder` instruction handler.

use cow_settlement_interface::{
    data::{intent::extract_owner, order::OrderAccount, state::StateAccount},
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
            check_settlement_owned_order_authority(owner, state_pda)?;
        } else if owner.address().as_array() != intent.owner() {
            return Err(SettlementError::OwnerMismatch.into());
        }
        order.set_cancelled();
        Ok(())
    } else {
        // The order doesn't exist yet. Then we create the input intent in an
        // already cancelled state.
        let Some(intent_bytes) = intent_bytes else {
            return Err(ProgramError::UninitializedAccount);
        };
        // `process_new_onchain_order` validates the intent; only its owner is
        // needed beforehand, to pick who authorizes the creation.
        let expected_owner = if extract_owner(&intent_bytes) == STATE_PDA.as_array() {
            check_settlement_owned_order_authority(owner, state_pda)?;
            &STATE_PDA
        } else {
            owner.address()
        };
        process_new_onchain_order(
            program_id,
            (order_pda, &intent_bytes),
            expected_owner,
            created_by,
            true,
        )
    }
}

/// Checks that `signer` is the settlement-owned-order authority recorded in
/// `state_pda`, which must be the canonical state PDA.
#[inline(always)]
fn check_settlement_owned_order_authority(
    signer: &AccountView,
    state_pda: Option<&AccountView>,
) -> ProgramResult {
    let state_pda = state_pda.ok_or(ProgramError::NotEnoughAccountKeys)?;
    validate_is_state_pda(state_pda.address().as_array())?;
    let authority =
        StateAccount::from_account(state_pda)?.authority(Role::SettlementOwnedOrderAuthority);
    if signer.address() != &authority {
        return Err(SettlementError::UnauthorizedSettlementOwnedOrder.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::pda::ID as PROGRAM_ID;
    use cow_settlement_interface::data::intent::{EncodedOrderIntent, OrderIntent};
    use cow_settlement_interface::data::order::fixtures::{sample_order_fields, OrderFields};
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

    /// The account in `accounts` at the canonical PDA of the order `fields`.
    fn order_account<'a>(accounts: &'a [AccountView], fields: &OrderFields) -> &'a AccountView {
        let (address, _) = find_order_pda(&PROGRAM_ID, &fields.intent.uid());
        accounts
            .iter()
            .find(|account| account.address() == &address)
            .expect("order PDA among the accounts")
    }

    mod input {
        use super::*;

        #[test]
        fn propagates_parse_error() {
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
        fn rejects_nonsigner_owner() {
            let intent_bytes = valid_intent_bytes();
            let data = default_cancel_data(Some(intent_bytes));
            // `fake_sequential_accounts` leaves the owner (index 0) unsigned.
            let mut accounts = fake_sequential_accounts::<NUM_ACCOUNTS>();

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
                Err(ProgramError::MissingRequiredSignature),
            );
        }
    }

    /// Generate arbitrary order data and the corresponding PDA.
    /// The intent owner and cancelled state are determined by the input.
    fn order_at_pda_owned_by(owner: Address, cancelled: bool) -> (OrderFields, Address) {
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

    mod existing_order {
        use super::*;

        fn user_order_at_pda(cancelled: bool) -> (OrderFields, Address) {
            order_at_pda_owned_by(pubkey_from_seed("user_order_at_pda's owner"), cancelled)
        }

        /// Whether the order stored at the canonical PDA of `fields` is cancelled.
        fn is_cancelled(accounts: &[AccountView], fields: &OrderFields) -> bool {
            OrderAccount::load_from_pda(order_account(accounts, fields), &PROGRAM_ID)
                .and_then(|order| order.cancelled())
                .expect("order readable")
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

        #[test]
        fn rejects_owner_mismatch() {
            let (fields, pda_address) = user_order_at_pda(false);
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
        fn marks_active_order_cancelled() {
            let (fields, pda_address) = user_order_at_pda(false);
            let owner = fields.intent.owner;

            let data = default_cancel_data(Some(valid_intent_bytes()));
            let mut accounts = accounts_for_existing(owner, &fields, pda_address);

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
                Ok(()),
            );
            assert!(
                is_cancelled(&accounts, &fields),
                "order must be marked cancelled"
            );
        }

        #[test]
        fn recovers_intent_from_pda() {
            let (fields, pda_address) = user_order_at_pda(false);
            let owner = fields.intent.owner;

            // No intent bytes on the wire: the order's data is recovered from the PDA.
            let data = default_cancel_data(None);
            let mut accounts = accounts_for_existing(owner, &fields, pda_address);

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
                Ok(()),
            );
            assert!(
                is_cancelled(&accounts, &fields),
                "order must be marked cancelled"
            );
        }

        #[test]
        fn ignores_wire_intent() {
            let (fields, pda_address) = user_order_at_pda(false);
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
            let (cancelled_fields, _) = user_order_at_pda(true);
            assert_eq!(
                &order_account(&accounts, &fields)
                    .try_borrow()
                    .expect("order data readable")[..],
                &cancelled_fields.encode()[..],
                "the wire intent must not leak into the stored order"
            );
        }

        #[test]
        fn is_idempotent_on_cancelled_order() {
            let (fields, pda_address) = user_order_at_pda(true);
            let owner = fields.intent.owner;
            let before: [u8; SIZE] = fields.encode();

            let data = default_cancel_data(Some(valid_intent_bytes()));
            let mut accounts = accounts_for_existing(owner, &fields, pda_address);

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
                Ok(()),
            );
            let after = order_account(&accounts, &fields)
                .try_borrow()
                .expect("order data readable");
            assert_eq!(
                &after[..],
                &before[..],
                "cancelling again must not change the order"
            );
        }
    }

    mod new_order {
        use super::*;

        #[test]
        fn rejects_missing_intent() {
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
    }

    mod settlement_owned {
        use super::*;

        static SETTLEMENT_OWNED_ORDER_AUTHORITY: LazyLock<Address> =
            LazyLock::new(|| pubkey_from_seed("settlement-owned-order authority"));

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

        /// Builds instruction data and accounts for cancelling a settlement-owned
        /// order, signed by the given signer and with the given account as the
        /// trailing state PDA.
        type SetupFn = fn(Address, AccountView) -> (Vec<u8>, [AccountView; NUM_ACCOUNTS + 1]);

        fn assert_rejects_wrong_authority(setup: SetupFn) {
            let (data, mut accounts) = setup(pubkey_from_seed("unrelated"), state_account());

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
                Err(SettlementError::UnauthorizedSettlementOwnedOrder.into()),
            );
        }

        fn assert_rejects_missing_state_pda(setup: SetupFn) {
            let (data, mut accounts) = setup(*SETTLEMENT_OWNED_ORDER_AUTHORITY, state_account());

            let (state_pda, accounts) = accounts
                .split_last_mut()
                .expect("should have a last element");

            assert_eq!(state_pda.address(), &STATE_PDA);

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, accounts, &data),
                Err(ProgramError::NotEnoughAccountKeys),
            );
        }

        fn assert_rejects_non_canonical_state_pda(setup: SetupFn) {
            let impostor_state = fake_account(pubkey_from_seed("impostor state"));
            let (data, mut accounts) = setup(*SETTLEMENT_OWNED_ORDER_AUTHORITY, impostor_state);

            assert_eq!(
                process_cancel_order(&PROGRAM_ID, &mut accounts, &data),
                Err(SettlementError::StateAccountMismatch.into()),
            );
        }

        mod existing_order {
            use super::*;

            /// Accounts for cancelling an existing order owned by the state PDA, signed
            /// by `signer` and with `state_account` as the trailing state PDA.
            fn accounts_for_settlement_owned(
                signer: Address,
                state_account: AccountView,
            ) -> ([AccountView; NUM_ACCOUNTS + 1], OrderFields) {
                let (fields, pda_address) = order_at_pda_owned_by(STATE_PDA, false);
                let accounts = [
                    fake_signer(signer),
                    fake_account(pubkey_from_seed("created by")),
                    fake_account_owned_by(pda_address, PROGRAM_ID, &fields.encode()[..]),
                    fake_account(pubkey_from_seed("system program")),
                    state_account,
                ];
                (accounts, fields)
            }

            #[test]
            fn lets_authority_cancel() {
                let (mut accounts, fields) = accounts_for_settlement_owned(
                    *SETTLEMENT_OWNED_ORDER_AUTHORITY,
                    state_account(),
                );

                assert_eq!(
                    process_cancel_order(&PROGRAM_ID, &mut accounts, &default_cancel_data(None)),
                    Ok(()),
                );
                let expected = OrderFields {
                    cancelled: true,
                    ..fields
                };
                assert_eq!(
                    &order_account(&accounts, &expected)
                        .try_borrow()
                        .expect("order data readable")[..],
                    &expected.encode()[..],
                    "only the cancelled flag may change"
                );
            }

            /// [`accounts_for_settlement_owned`] paired with its instruction data.
            fn existing_settlement_owned(
                signer: Address,
                state_account: AccountView,
            ) -> (Vec<u8>, [AccountView; NUM_ACCOUNTS + 1]) {
                let (accounts, _) = accounts_for_settlement_owned(signer, state_account);
                (default_cancel_data(None), accounts)
            }

            #[test]
            fn rejects_wrong_authority() {
                assert_rejects_wrong_authority(existing_settlement_owned);
            }

            #[test]
            fn rejects_missing_state_pda() {
                assert_rejects_missing_state_pda(existing_settlement_owned);
            }

            #[test]
            fn rejects_non_canonical_state_pda() {
                assert_rejects_non_canonical_state_pda(existing_settlement_owned);
            }
        }

        mod new_order {
            use super::*;

            /// Instruction data and accounts for cancelling a not-yet-created order
            /// owned by the state PDA, signed by `signer` and with `state_account` as
            /// the trailing state PDA. The order PDA is a placeholder: every test using
            /// this stops before the (CPI-based) allocation.
            fn new_settlement_owned(
                signer: Address,
                state_account: AccountView,
            ) -> (Vec<u8>, [AccountView; NUM_ACCOUNTS + 1]) {
                let intent = OrderIntent {
                    owner: STATE_PDA,
                    ..sample_order_fields(false).intent
                };
                let data = default_cancel_data(Some((&EncodedOrderIntent::from(&intent)).into()));
                let accounts = [
                    fake_signer(signer),
                    fake_signer(pubkey_from_seed("created by")),
                    fake_account(pubkey_from_seed("missing order pda")),
                    fake_account(pubkey_from_seed("system program")),
                    state_account,
                ];
                (data, accounts)
            }

            #[test]
            fn rejects_wrong_authority() {
                assert_rejects_wrong_authority(new_settlement_owned);
            }

            #[test]
            fn rejects_missing_state_pda() {
                assert_rejects_missing_state_pda(new_settlement_owned);
            }

            #[test]
            fn rejects_non_canonical_state_pda() {
                assert_rejects_non_canonical_state_pda(new_settlement_owned);
            }
        }
    }
}
