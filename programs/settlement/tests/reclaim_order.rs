use settlement_client::settlement_interface::{
    data::intent::{fixtures::sample_intent, EncodedOrderIntent, OrderIntent, OrderKind},
    instruction::{create_order::CreateOrder, reclaim_order::ReclaimOrder},
    pda::order::find_order_pda,
    SettlementError,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use crate::common::signed_tx;

mod common;

const VALID_TO: u32 = 1_000;

fn reclaim_sample_intent(owner: Pubkey) -> OrderIntent {
    OrderIntent {
        owner,
        valid_to: VALID_TO,
        ..sample_intent(OrderKind::Sell, true)
    }
}

fn encode_and_derive(
    intent: &OrderIntent,
    program_id: &Pubkey,
) -> ([u8; EncodedOrderIntent::SIZE], Pubkey) {
    let encoded = EncodedOrderIntent::from(intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, _) = find_order_pda(program_id, &encoded.hash());
    (bytes, pda)
}

/// Create an order PDA owned by `owner` (who also pays rent), return the PDA.
fn create_order(
    svm: &mut litesvm::LiteSVM,
    program_id: &Pubkey,
    owner: &Keypair,
    intent: &OrderIntent,
) -> Pubkey {
    let (encoded, pda) = encode_and_derive(intent, program_id);
    let ix = CreateOrder {
        program_id: *program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let tx = signed_tx(svm, owner, owner, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");
    pda
}

#[test]
fn happy_path_returns_lamports_and_closes_pda() {
    let (mut svm, program_id, fee_payer) = common::setup();

    // `reclaim_recipient` is the `created_by` funder; it's separate from the fee
    // payer so its balance change reflects only the returned rent, not tx fees.
    let reclaim_recipient = Keypair::new();
    svm.airdrop(&reclaim_recipient.pubkey(), 1_000_000_000)
        .expect("airdrop should succeed");

    let intent = OrderIntent {
        owner: fee_payer.pubkey(),
        ..reclaim_sample_intent(fee_payer.pubkey())
    };
    let encoded = EncodedOrderIntent::from(&intent);
    let encoded_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, bump) = find_order_pda(&program_id, &encoded.hash());

    let pda_rent = svm.minimum_balance_for_rent_exemption(
        settlement_client::settlement_interface::data::order::EncodedOrderAccount::SIZE,
    );

    // Create the order; `reclaim_recipient` funds the rent (`created_by`).
    let ix = CreateOrder {
        program_id,
        owner: fee_payer.pubkey(),
        created_by: reclaim_recipient.pubkey(),
        order_pda: pda,
        intent_bytes: encoded_bytes,
    };
    let tx = signed_tx(&svm, &fee_payer, &reclaim_recipient, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");

    // Since ReclaimOrder should return any funds in the order pda (even if beyond the rent limit), we airdrop some extra lamports
    let extra_lamports = 10;
    svm.airdrop(&pda, extra_lamports)
        .expect("airdrop should succeed");

    assert!(svm.get_account(&pda).is_some(), "order PDA must exist");

    let reclaim_recipient_before = common::lamports(&svm, &reclaim_recipient.pubkey());

    common::set_unix_timestamp(&mut svm, (VALID_TO + 1).into());

    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        bump,
        reclaim_recipient: reclaim_recipient.pubkey(),
    }
    .instruction();
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    svm.send_transaction(tx)
        .expect("reclaim_order should succeed after expiry");

    // PDA is gone.
    assert!(
        svm.get_account(&pda).is_none(),
        "order PDA must be closed after reclaim"
    );

    // Reclaim recipient account received all lamports that were in the order pda; it paid no tx fees.
    let reclaim_recipient_after = common::lamports(&svm, &reclaim_recipient.pubkey());
    assert_eq!(
        reclaim_recipient_after - reclaim_recipient_before,
        pda_rent + extra_lamports,
        "reclaim recipient account must receive exactly the order PDA's rent lamports"
    );
}

#[test]
fn rejects_when_order_not_yet_expired() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    let (_, bump) = find_order_pda(&program_id, &EncodedOrderIntent::from(&intent).hash());

    common::set_unix_timestamp(&mut svm, VALID_TO as i64); // technically this is the last valid timestamp

    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        bump,
        reclaim_recipient: owner.pubkey(),
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    common::assert_settlement_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::OrderNotExpired,
    );
}

#[test]
fn rejects_when_reclaim_recipient_mismatch() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    let (_, bump) = find_order_pda(&program_id, &EncodedOrderIntent::from(&intent).hash());

    common::set_unix_timestamp(&mut svm, (VALID_TO + 1).into());

    let wrong_recipient = Pubkey::new_unique();
    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        bump,
        reclaim_recipient: wrong_recipient,
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    common::assert_settlement_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::ReclaimRecipientMismatch,
    );
}
