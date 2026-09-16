//! Integration tests for placing a `CreateWithdrawalOrder`: an order owned by
//! the settlement state PDA, gated by the withdrawal authority, to sell the fees
//! that accumulate in the buffer accounts. Settling such an order is covered in
//! `settle_withdrawal_order.rs`.

use crate::common::{
    assert_instruction_error,
    order::{sample_intent, OrderBuilder},
    send_with_signers, setup_init, unique_keypair,
};
use cow_settlement_client::instruction::CreateWithdrawalOrder;
use cow_settlement_interface::{
    data::order::OrderAccount, pda::order::find_order_pda, SettlementError,
};
use solana_sdk::signer::Signer;

mod common;

#[test]
fn places_an_order_owned_by_the_state_pda() {
    let (mut svm, params) = setup_init();

    let intent = OrderBuilder::new(&mut svm, &params.program_id, &params.payer)
        .withdrawal(&params.withdrawal)
        .sell_amount(1_000_000)
        .buy_amount(500_000)
        .build();

    let (order_pda, bump) = find_order_pda(&params.program_id, &intent.uid());
    let account = svm
        .get_account(&order_pda)
        .expect("the order PDA should exist after placement");
    assert_eq!(
        account.owner, params.program_id,
        "the order PDA must be owned by the settlement program"
    );
    let OrderAccount {
        cancelled,
        amount_withdrawn,
        amount_received,
        created_by,
        intent: decoded_intent,
        bump: decoded_bump,
    } = OrderAccount::try_from(&account.data[..]).expect("the order PDA must decode");
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
fn rejects_a_caller_that_is_not_the_withdrawal_authority() {
    let (mut svm, params) = setup_init();
    let intent = sample_intent(params.state_pda, 0);

    // An unrelated signer isn't the withdrawal authority recorded in state.
    let impostor = unique_keypair();
    let ix = CreateWithdrawalOrder {
        program_id: params.program_id,
        authority: impostor.pubkey(),
        payer: params.payer.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(&mut svm, &params.payer, &[&impostor], &[ix.into()]);
    assert_instruction_error(result, SettlementError::UnauthorizedWithdrawalOrder);
}

#[test]
fn rejects_an_order_not_owned_by_the_state_pda() {
    let (mut svm, params) = setup_init();
    // Owned by an arbitrary account rather than the state PDA.
    let intent = sample_intent(unique_keypair().pubkey(), 0);

    let ix = CreateWithdrawalOrder {
        program_id: params.program_id,
        authority: params.withdrawal.pubkey(),
        payer: params.payer.pubkey(),
        intent: &intent,
    };
    let result = send_with_signers(&mut svm, &params.payer, &[&params.withdrawal], &[ix.into()]);
    assert_instruction_error(result, SettlementError::OwnerMismatch);
}
