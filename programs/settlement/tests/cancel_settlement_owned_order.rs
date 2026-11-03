//! Integration tests for `CancelSettlementOwnedOrder`: cancelling an order
//! owned by the settlement state PDA, gated by the settlement-owned-order
//! authority. Placing such an order is covered in
//! `create_settlement_owned_order.rs`.

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    create_account_at,
    order::{read_order, sample_intent},
    send_with_signers, setup_init, signed_tx, unique_keypair, unique_pubkey, InitializedParams,
};
use cow_settlement_client::instruction::CancelSettlementOwnedOrder;
use cow_settlement_client::pda::order::DecodedOrderAccount;
use cow_settlement_interface::SettlementError;
use cow_settlement_interface::{data::intent::OrderIntent, pda::order::find_order_pda};
use litesvm::LiteSVM;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey, signer::Signer};

mod common;

/// Plant an active order for `intent` at its canonical PDA, funded by
/// `created_by`, and return the PDA.
fn plant_order(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    intent: &OrderIntent,
    created_by: Pubkey,
) -> Pubkey {
    let (pda, bump) = find_order_pda(&params.program_id, &intent.uid());
    let body = DecodedOrderAccount {
        bump,
        cancelled: false,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by,
        intent: intent.clone(),
    }
    .encode();
    create_account_at(svm, pda, &params.program_id, &body);
    pda
}

#[test]
fn cancels_an_order_owned_by_the_state_pda() {
    let (mut svm, params) = setup_init();
    let intent = sample_intent(params.state_pda, 0);
    let created_by = unique_pubkey();
    let pda = plant_order(&mut svm, &params, &intent, created_by);

    let ix = CancelSettlementOwnedOrder {
        program_id: params.program_id,
        authority: params.settlement_owned_order.pubkey(),
        intent: &intent,
    };
    let tx = signed_tx(&svm, &params.payer, &params.settlement_owned_order, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::CancelSettlementOwnedOrder)
        .expect("cancel_settlement_owned_order should succeed");

    let DecodedOrderAccount {
        cancelled,
        created_by: stored_created_by,
        intent: stored_intent,
        ..
    } = read_order(&svm, &pda);
    assert!(cancelled, "the order must be marked cancelled");
    assert_eq!(stored_intent, intent, "the stored intent must be untouched");
    assert_eq!(
        stored_created_by, created_by,
        "the rent refund target must be untouched"
    );
}

#[test]
fn rejects_a_caller_that_is_not_the_settlement_owned_order_authority() {
    let (mut svm, params) = setup_init();
    let intent = sample_intent(params.state_pda, 0);
    let pda = plant_order(&mut svm, &params, &intent, unique_pubkey());

    // An unrelated signer isn't the settlement-owned-order authority recorded in state.
    let impostor = unique_keypair();
    let ix = CancelSettlementOwnedOrder {
        program_id: params.program_id,
        authority: impostor.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(&mut svm, &params.payer, &[&impostor], &[ix.into()]);
    assert_instruction_error(result, SettlementError::UnauthorizedSettlementOwnedOrder);
    assert!(!read_order(&svm, &pda).cancelled);
}

#[test]
fn rejects_an_order_not_owned_by_the_state_pda() {
    let (mut svm, params) = setup_init();
    // A user's order: the settlement-owned-order authority has no say over it.
    let intent = sample_intent(unique_pubkey(), 0);
    let pda = plant_order(&mut svm, &params, &intent, unique_pubkey());

    let ix = CancelSettlementOwnedOrder {
        program_id: params.program_id,
        authority: params.settlement_owned_order.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(
        &mut svm,
        &params.payer,
        &[&params.settlement_owned_order],
        &[ix.into()],
    );
    assert_instruction_error(result, SettlementError::OwnerMismatch);
    assert!(!read_order(&svm, &pda).cancelled);
}

#[test]
fn rejects_an_order_that_does_not_exist() {
    let (mut svm, params) = setup_init();
    let intent = sample_intent(params.state_pda, 0);

    let ix = CancelSettlementOwnedOrder {
        program_id: params.program_id,
        authority: params.settlement_owned_order.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(
        &mut svm,
        &params.payer,
        &[&params.settlement_owned_order],
        &[ix.into()],
    );
    assert_instruction_error(result, InstructionError::UninitializedAccount);
}
