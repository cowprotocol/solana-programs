//! Integration tests for placing a `CreateSelfOrder`: an order owned by
//! the settlement state PDA, gated by the self-order authority, to sell the fees
//! that accumulate in the buffer accounts. Settling such an order is covered in
//! `settle_self_order.rs`.

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    order::sample_intent,
    send_with_signers, setup_init, signed_tx, unique_keypair,
};
use cow_settlement_client::instruction::CreateSelfOrder;
use cow_settlement_client::pda::order::DecodedOrderAccount;
use cow_settlement_interface::{pda::order::find_order_pda, SettlementError};
use solana_sdk::signer::Signer;

mod common;

#[test]
fn places_an_order_owned_by_the_state_pda() {
    let (mut svm, params) = setup_init();

    let intent = sample_intent(params.state_pda, 0);
    let ix = CreateSelfOrder {
        program_id: params.program_id,
        authority: params.self_order.pubkey(),
        created_by: params.payer.pubkey(),
        intent: &intent,
    };
    let tx = signed_tx(&svm, &params.payer, &params.self_order, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::CreateSelfOrder)
        .expect("create_self_order should succeed");

    let (order_pda, bump) = find_order_pda(&params.program_id, &intent.uid());
    let account = svm
        .get_account(&order_pda)
        .expect("the order PDA should exist after placement");
    assert_eq!(
        account.owner, params.program_id,
        "the order PDA must be owned by the settlement program"
    );
    let DecodedOrderAccount {
        cancelled,
        amount_withdrawn,
        amount_received,
        created_by,
        intent: decoded_intent,
        bump: decoded_bump,
    } = DecodedOrderAccount::try_from(&account.data[..]).expect("the order PDA must decode");
    assert_eq!(decoded_intent, intent, "the stored intent must match");
    assert_eq!(
        decoded_intent.owner, params.state_pda,
        "the program must force the order's owner to the state PDA"
    );
    assert_eq!(
        created_by,
        params.payer.pubkey(),
        "the payer must be recorded as created_by, so the rent refunds to it"
    );
    assert!(!cancelled, "a fresh order isn't cancelled");
    assert_eq!(amount_withdrawn, 0);
    assert_eq!(amount_received, 0);
    assert_eq!(decoded_bump, bump);
}

#[test]
fn rejects_a_caller_that_is_not_the_self_order_authority() {
    let (mut svm, params) = setup_init();
    let intent = sample_intent(params.state_pda, 0);

    // An unrelated signer isn't the self-order authority recorded in state.
    let impostor = unique_keypair();
    let ix = CreateSelfOrder {
        program_id: params.program_id,
        authority: impostor.pubkey(),
        created_by: params.payer.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(&mut svm, &params.payer, &[&impostor], &[ix.into()]);
    assert_instruction_error(result, SettlementError::UnauthorizedSelfOrder);
}

#[test]
fn rejects_an_order_not_owned_by_the_state_pda() {
    let (mut svm, params) = setup_init();
    // Owned by an arbitrary account rather than the state PDA.
    let intent = sample_intent(unique_keypair().pubkey(), 0);

    let ix = CreateSelfOrder {
        program_id: params.program_id,
        authority: params.self_order.pubkey(),
        created_by: params.payer.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(&mut svm, &params.payer, &[&params.self_order], &[ix.into()]);
    assert_instruction_error(result, SettlementError::OwnerMismatch);
}

#[test]
fn rejects_an_order_not_flagged_as_created_on_chain() {
    let (mut svm, params) = setup_init();
    // Owned by the state PDA, but the intent claims an off-chain creation.
    let mut intent = sample_intent(params.state_pda, 0);
    intent.flags.created_on_chain = false;

    let ix = CreateSelfOrder {
        program_id: params.program_id,
        authority: params.self_order.pubkey(),
        created_by: params.payer.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(&mut svm, &params.payer, &[&params.self_order], &[ix.into()]);
    assert_instruction_error(result, SettlementError::OrderCreatedOnChainMismatch);
}
