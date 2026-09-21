use cow_settlement_client::cow_settlement_interface::{
    data::{
        intent::{fixtures, EncodedOrderIntent, OrderIntent},
        order::{EncodedOrderAccount, OrderAccount},
    },
    instruction::{cancel_order::CancelOrder, create_order::CreateOrder},
    pda::order::find_order_pda,
    SettlementError,
};
use cow_settlement_interface::data::intent::{Flags, OrderKind};
use solana_sdk::{pubkey::Pubkey, signature::Signer, transaction::TransactionError};

use crate::common::{
    benchmark::{send_transaction_metered, BenchLabel},
    signed_tx, unique_pubkey,
};

mod common;

fn sample_intent(owner: Pubkey) -> OrderIntent {
    OrderIntent {
        owner,
        ..fixtures::sample_intent(Flags {
            created_on_chain: true,
            kind: OrderKind::Sell,
            partially_fillable: false,
        })
    }
}

fn encode_and_derive(
    intent: &OrderIntent,
    program_id: &Pubkey,
) -> ([u8; EncodedOrderIntent::SIZE], Pubkey, u8) {
    let encoded = EncodedOrderIntent::from(intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, bump) = find_order_pda(program_id, &encoded.hash());
    (bytes, pda, bump)
}

/// The canonical encoded order body for `intent` at its PDA, with the given
/// `cancelled` state and `created_by`, matching a freshly created (unfilled)
/// order.
fn expected_body(
    intent: &OrderIntent,
    bump: u8,
    cancelled: bool,
    created_by: Pubkey,
) -> [u8; EncodedOrderAccount::SIZE] {
    EncodedOrderAccount::from(OrderAccount {
        bump,
        cancelled,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by,
        intent: intent.clone(),
    })
    .into()
}

#[test]
fn cancels_existing_active_order() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey());
    let (encoded, pda, bump) = encode_and_derive(&intent, &program_id);

    // Create the order first, then cancel it.
    let create = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    svm.send_transaction(signed_tx(&svm, &owner, &owner, create))
        .expect("create_order should succeed");
    assert!(
        !common::order::read_order(&svm, &pda).cancelled,
        "order must start active"
    );

    svm.expire_blockhash();

    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let tx = signed_tx(&svm, &owner, &owner, cancel);
    send_transaction_metered(&mut svm, tx, BenchLabel::CancelOrder)
        .expect("cancel_order should succeed");

    let account = svm
        .get_account(&pda)
        .expect("order PDA must remain after cancellation");
    assert_eq!(account.owner, program_id, "PDA must stay program-owned");
    assert_eq!(
        account.data,
        expected_body(&intent, bump, true, owner.pubkey()),
        "only the cancelled flag must change"
    );
    common::assert_rent_exempt(&svm, &account);
}

#[test]
fn creates_cancelled_pda_for_nonexistent_order() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey());
    let (encoded, pda, bump) = encode_and_derive(&intent, &program_id);
    assert!(
        svm.get_account(&pda).is_none(),
        "the order must not exist yet"
    );

    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let tx = signed_tx(&svm, &owner, &owner, cancel);
    send_transaction_metered(&mut svm, tx, BenchLabel::CancelOrder)
        .expect("cancel_order should create the order already cancelled");

    let account = svm
        .get_account(&pda)
        .expect("cancelling a missing order must create its PDA, not delete it");
    assert_eq!(account.owner, program_id, "PDA must be program-owned");
    assert_eq!(
        account.data,
        expected_body(&intent, bump, true, owner.pubkey()),
        "the created order must be cancelled and record created_by"
    );
    common::assert_rent_exempt(&svm, &account);
}

#[test]
fn cancelling_is_idempotent() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey());
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    let cancel = || CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };

    // First cancellation creates the order cancelled.
    svm.send_transaction(signed_tx(&svm, &owner, &owner, cancel()))
        .expect("first cancel_order should succeed");
    let after_first = svm.get_account(&pda).expect("order PDA must exist");

    svm.expire_blockhash();

    // Second cancellation is a no-op and leaves the order byte-for-byte the same.
    svm.send_transaction(signed_tx(&svm, &owner, &owner, cancel()))
        .expect("second cancel_order should succeed");
    let after_second = svm.get_account(&pda).expect("order PDA must still exist");

    assert_eq!(
        after_first, after_second,
        "cancelling an already-cancelled order must not change it"
    );
}

#[test]
fn rejects_cancellation_by_non_owner() {
    let (mut svm, program_id, attacker) = common::setup();
    let owner = common::unique_keypair();
    svm.airdrop(&owner.pubkey(), 1_000_000_000)
        .expect("airdrop to owner should succeed");

    let intent = sample_intent(owner.pubkey());
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    // The real owner creates the order.
    let create = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    svm.send_transaction(signed_tx(&svm, &owner, &owner, create))
        .expect("create_order should succeed");
    let before = svm.get_account(&pda).expect("order PDA must exist");

    svm.expire_blockhash();

    // A different signer, presenting the same order, must not be able to cancel it.
    let cancel = CancelOrder {
        program_id,
        owner: attacker.pubkey(),
        created_by: attacker.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let err = svm
        .send_transaction(signed_tx(&svm, &attacker, &attacker, cancel))
        .expect_err("cancellation by a non-owner must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, SettlementError::OwnerMismatch.into()),
    );

    let after = svm.get_account(&pda).expect("order PDA must still exist");
    assert_eq!(
        before, after,
        "a rejected cancellation must not change the order"
    );
}

#[test]
fn rejects_nonexistent_off_chain_intent() {
    let (mut svm, program_id, owner) = common::setup();

    // An off-chain-authenticated intent has no on-chain owner-direct cancel path;
    // the create branch rejects it just as CreateOrder would.
    let intent = OrderIntent {
        flags: Flags {
            created_on_chain: false,
            ..Default::default()
        },
        ..sample_intent(owner.pubkey())
    };
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let err = svm
        .send_transaction(signed_tx(&svm, &owner, &owner, cancel))
        .expect_err("cancelling a missing off-chain order must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, SettlementError::OrderCreatedOnChainMismatch.into()),
    );
    assert!(
        svm.get_account(&pda).is_none(),
        "a rejected cancellation must not leave a PDA behind"
    );
}

#[test]
fn rejects_arbitrary_wrong_pda() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey());
    let (encoded, _canonical_pda, _bump) = encode_and_derive(&intent, &program_id);

    // A wrong, non-canonical order PDA for a missing order: the create branch
    // rejects it as CreateOrder does.
    let wrong_pda = unique_pubkey();
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: wrong_pda,
        intent_bytes: encoded,
    };
    let tx = signed_tx(&svm, &owner, &owner, cancel);
    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_pda);
}
