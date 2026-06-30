use settlement_client::settlement_interface::{
    data::{
        intent::{fixtures, EncodedOrderIntent, OrderIntent, OrderKind},
        order::{EncodedOrderAccount, OrderAccount},
    },
    instruction::create_order::CreateOrder,
    pda::order::{find_order_pda, order_pda_seeds},
    SettlementError,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

use crate::common::{signed_tx, to_instruction_error};

mod common;

fn sample_intent(owner: Pubkey) -> OrderIntent {
    OrderIntent {
        owner,
        ..fixtures::sample_intent(OrderKind::Sell, true)
    }
}

fn encode_and_derive(
    intent: &OrderIntent,
    program_id: &Pubkey,
) -> ([u8; EncodedOrderIntent::SIZE], Pubkey) {
    let encoded = EncodedOrderIntent::from(intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, _bump) = find_order_pda(program_id, &encoded.hash());
    (bytes, pda)
}

#[test]
fn happy_path_creates_order_pda_with_expected_body() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey());
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    // `owner` doubles as `created_by` here: the same address may fill both
    // slots, which is the common case. It also pays the tx fee.
    let ix = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    }
    .into();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");

    let account = svm
        .get_account(&pda)
        .expect("order PDA should exist after create_order");
    assert_eq!(
        account.owner, program_id,
        "PDA must be owned by the program"
    );
    assert_eq!(
        account.data.len(),
        EncodedOrderAccount::SIZE,
        "PDA must be sized to the order body",
    );

    let expected_body: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: false,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: owner.pubkey(),
        intent: intent.clone(),
    })
    .into();
    assert_eq!(
        account.data, expected_body,
        "PDA body must match expected layout"
    );

    // Rent-exempt sanity: the PDA must hold exactly the rent minimum for
    // its size.
    let rent = svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE);
    assert!(
        account.lamports == rent,
        "PDA must be rent-exempt: {} < {}",
        account.lamports,
        rent,
    );
}

#[test]
fn creates_order_with_separate_fee_payers() {
    // Three distinct roles: `fee_payer` pays the tx fee, `owner` only
    // authenticates the order, and `created_by` funds the PDA's rent. This is
    // the rent-delegation case (e.g. when `owner` is a PDA).
    let (mut svm, program_id, fee_payer) = common::setup();
    let owner = Keypair::new_from_array([42; 32]);
    let created_by = Keypair::new_from_array([43; 32]);
    svm.airdrop(&created_by.pubkey(), 1_000_000_000)
        .expect("airdrop to created_by should succeed");

    let intent = sample_intent(owner.pubkey());
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    let fee_payer_before = common::lamports(&svm, &fee_payer.pubkey());
    let owner_before = common::lamports(&svm, &owner.pubkey());
    let created_by_before = common::lamports(&svm, &created_by.pubkey());

    let ix = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: created_by.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    }
    .into();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&fee_payer.pubkey()),
        &[&fee_payer, &owner, &created_by],
        svm.latest_blockhash(),
    );
    let receipt = svm
        .send_transaction(tx)
        .expect("create_order should succeed");

    let fee_payer_after = common::lamports(&svm, &fee_payer.pubkey());
    let owner_after = common::lamports(&svm, &owner.pubkey());
    let created_by_after = common::lamports(&svm, &created_by.pubkey());

    let rent = svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE);

    // `created_by` funded the new PDA's rent in full.
    assert_eq!(created_by_before - created_by_after, rent);
    // `owner` only authenticated the order; it paid nothing.
    assert_eq!(owner_before, owner_after);
    // `fee_payer` was charged only the transaction fee, not the rent.
    assert_eq!(fee_payer_before - fee_payer_after, receipt.fee);
    // Sanity check: fee doesn't include rent.
    assert!(rent > receipt.fee);

    // The body records `created_by`, not `owner`.
    let account = svm
        .get_account(&pda)
        .expect("order PDA should exist after create_order");
    let expected_body: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: false,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: created_by.pubkey(),
        intent,
    })
    .into();
    assert_eq!(
        account.data, expected_body,
        "PDA body must record created_by, not owner"
    );
}

#[test]
fn rejects_arbitrary_wrong_pda() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey());
    let (encoded, _canonical_pda) = encode_and_derive(&intent, &program_id);

    // Hand the client helper a deliberately wrong address; it forwards the
    // PDA we give it rather than deriving the canonical one.
    let wrong_pda = Pubkey::new_unique();
    let ix = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: wrong_pda,
        intent_bytes: encoded,
    }
    .into();
    let tx = signed_tx(&svm, &owner, &owner, ix);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_pda);
}

#[test]
fn rejects_non_canonical_bump_pda() {
    let (mut svm, program_id, fee_payer) = common::setup();

    let intent = sample_intent(fee_payer.pubkey());
    let encoded = EncodedOrderIntent::from(&intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let uid = encoded.hash();

    let (_bump, non_canonical_pda) =
        common::pda::find_noncanonical_pda(&program_id, order_pda_seeds(&uid));

    let ix = CreateOrder {
        program_id,
        owner: fee_payer.pubkey(),
        created_by: fee_payer.pubkey(),
        order_pda: non_canonical_pda,
        intent_bytes: bytes,
    }
    .into();
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &non_canonical_pda);
}

#[test]
fn rejects_creating_same_pda_twice() {
    let (mut svm, program_id, fee_payer) = common::setup();
    let another_fee_payer = Keypair::new_from_array([43; 32]);
    svm.airdrop(&another_fee_payer.pubkey(), 1_000_000_000)
        .expect("airdrop to another_fee_payer should succeed");

    let intent = sample_intent(fee_payer.pubkey());
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    // First creation populates the PDA.
    let ix = CreateOrder {
        program_id,
        owner: fee_payer.pubkey(),
        created_by: fee_payer.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    }
    .into();
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    svm.send_transaction(tx)
        .expect("first create_order should succeed");

    svm.expire_blockhash();

    // For good measure, we change `created_by` to stress that the input
    // account doesn't matter here.
    let ix = CreateOrder {
        program_id,
        owner: fee_payer.pubkey(),
        created_by: another_fee_payer.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    }
    .into();
    let tx = signed_tx(&svm, &another_fee_payer, &fee_payer, ix);
    common::pda::assert_rejected_as_existing(&mut svm, tx);
}

#[test]
fn rejects_when_intent_owner_differs_from_signer() {
    let (mut svm, program_id, fee_payer) = common::setup();

    // `intent.owner` is a fresh pubkey, distinct from `fee_payer.pubkey()`
    // who is the only signer for the `owner` slot.
    let intent_owner = Pubkey::new_unique();
    let intent = sample_intent(intent_owner);
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    let ix = CreateOrder {
        program_id,
        owner: fee_payer.pubkey(),
        created_by: fee_payer.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    }
    .into();
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    let err = svm
        .send_transaction(tx)
        .expect_err("create_order must reject when intent.owner differs from the signer");
    let expected_failing_instruction_index = 0;
    assert_eq!(
        err.err,
        TransactionError::InstructionError(
            expected_failing_instruction_index,
            to_instruction_error(SettlementError::OwnerMismatch),
        ),
        "expected MismatchingSettlePair at instruction {expected_failing_instruction_index}"
    );
}
