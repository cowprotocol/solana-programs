//! Integration tests for the settled-orders list carried by `BeginSettle`.
//!
//! Each settlement transaction here is the minimal `[BeginSettle, FinalizeSettle]`
//! pair (begin at index 0 pointing to finalize at index 1, and vice versa) so
//! that the begin/finalize pairing always validates and execution reaches the
//! order-list checks, which is what these tests exercise.

use crate::common::{
    assert_instruction_error, assert_settlement_error, create_account, set_unix_timestamp, setup,
    signed_tx, token,
};
use litesvm::LiteSVM;
use settlement_client::instructions::{begin_settle, create_order, finalize_settle};
use settlement_client::settlement_interface::{
    data::{
        intent::{OrderIntent, OrderKind},
        order::{EncodedOrderAccount, OrderAccount},
    },
    pda::order::find_order_pda,
    settle::begin_settle as raw_begin_settle,
    Instruction, SettlementError,
};
use solana_sdk::{
    account::Account,
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

fn sample_intent(owner: Pubkey, sell_token_account: Pubkey, salt: u8) -> OrderIntent {
    OrderIntent {
        owner,
        buy_token_account: Pubkey::new_from_array([0x22; 32]),
        sell_token_account,
        sell_amount: 1_000_000,
        buy_amount: 2_000_000,
        valid_to: 0xdead_beef,
        kind: OrderKind::Sell,
        partially_fillable: true,
        // `salt` is folded into `app_data` so callers can mint several orders that
        // hash to different UIDs (and therefore different order PDAs).
        app_data: [salt; 32],
    }
}

/// Create `intent`'s order PDA on-chain, signed and paid for by `owner`.
fn create_order_pda(svm: &mut LiteSVM, program_id: &Pubkey, owner: &Keypair, intent: &OrderIntent) {
    let ix = create_order(program_id, &owner.pubkey(), &owner.pubkey(), intent);
    let tx = signed_tx(svm, owner, owner, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");
}

/// Mint a valid order on-chain, distinct per `salt`, selling from a fresh token
/// account owned by `payer`, and return its intent.
fn settleable_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    mint: &Pubkey,
    salt: u8,
) -> OrderIntent {
    settleable_order_valid_to(svm, program_id, payer, mint, 0xdead_beef, salt)
}

fn settleable_order_valid_to(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    mint: &Pubkey,
    valid_to: u32,
    salt: u8,
) -> OrderIntent {
    let sell_token = token::create_token_account(svm, payer, mint, &payer.pubkey());
    let mut intent = sample_intent(payer.pubkey(), sell_token, salt);
    intent.valid_to = valid_to;
    create_order_pda(svm, program_id, payer, &intent);
    intent
}

/// Send `[begin, finalize_settle(..)]` signed by `payer`, where `begin` is a
/// pre-built `BeginSettle` instruction.
fn send_settlement(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    begin: Instruction,
) -> Result<(), TransactionError> {
    let finalize = finalize_settle(program_id, 0);
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

#[test]
fn settles_a_single_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = settleable_order(&mut svm, &program_id, &payer, &mint, 0);
    send_settlement(
        &mut svm,
        &program_id,
        &payer,
        begin_settle(&program_id, 1, &[intent]),
    )
    .expect("settlement should succeed");
}

#[test]
fn settles_multiple_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let mut intents = Vec::new();
    for salt in 0..3u8 {
        intents.push(settleable_order(&mut svm, &program_id, &payer, &mint, salt));
    }

    send_settlement(
        &mut svm,
        &program_id,
        &payer,
        begin_settle(&program_id, 1, &intents),
    )
    .expect("multi-order settlement should succeed");
}

#[test]
fn rejects_wrong_bump() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = settleable_order(&mut svm, &program_id, &payer, &mint, 0);
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            raw_begin_settle(
                &program_id,
                1,
                &[order_pda],
                &[intent.sell_token_account],
                &[bump ^ 0x01],
            ),
        ),
        SettlementError::OrderNotCanonical,
    );
}

#[test]
fn rejects_fabricated_program_owned_account() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let body: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: false,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: payer.pubkey(),
        intent: sample_intent(payer.pubkey(), sell_token, 0),
    })
    .into();
    // A program-owned account holding a valid order body, but sitting at an
    // address that isn't the canonical order PDA.
    let fake_order = create_account(&mut svm, &program_id, &body);

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            raw_begin_settle(&program_id, 1, &[fake_order], &[sell_token], &[255]),
        ),
        SettlementError::OrderNotCanonical,
    );
}

#[test]
fn rejects_non_order_account_in_order_slot() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // Put a token account in the order slot. Its 165-byte data can't decode as a
    // 199-byte order body, so it's rejected before the canonical-address check.
    assert_instruction_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            raw_begin_settle(&program_id, 1, &[sell_token], &[sell_token], &[255]),
        ),
        InstructionError::InvalidAccountData,
    );
}

#[test]
fn rejects_sell_token_account_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // Supply a different token account than the one the order's intent names.
    let intent = settleable_order(&mut svm, &program_id, &payer, &mint, 0);
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    let wrong_sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            raw_begin_settle(&program_id, 1, &[order_pda], &[wrong_sell_token], &[bump]),
        ),
        SettlementError::SellTokenAccountMismatch,
    );
}

#[test]
fn rejects_sell_token_owner_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let other_owner = Pubkey::new_unique();
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &other_owner);
    let intent = sample_intent(payer.pubkey(), sell_token, 1);
    create_order_pda(&mut svm, &program_id, &payer, &intent);

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            begin_settle(&program_id, 1, &[intent]),
        ),
        SettlementError::SellTokenOwnerMismatch,
    );
}

#[test]
fn rejects_non_token_sell_account() {
    let (mut svm, program_id, payer) = setup();

    let non_token = Pubkey::new_unique();
    let intent = sample_intent(payer.pubkey(), non_token, 1);
    create_order_pda(&mut svm, &program_id, &payer, &intent);

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            begin_settle(&program_id, 1, &[intent]),
        ),
        SettlementError::SellTokenAccountInvalid,
    );
}

#[test]
fn rejects_duplicate_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let intent = settleable_order(&mut svm, &program_id, &payer, &mint, 0);
    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            begin_settle(&program_id, 1, &[intent.clone(), intent]),
        ),
        SettlementError::OrdersNotStrictlyIncreasing,
    );
}

#[test]
fn rejects_orders_in_wrong_address_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let first = settleable_order(&mut svm, &program_id, &payer, &mint, 0);
    let second = settleable_order(&mut svm, &program_id, &payer, &mint, 1);

    let (first_pda, first_bump) = find_order_pda(&program_id, &first.uid());
    let (second_pda, second_bump) = find_order_pda(&program_id, &second.uid());
    let mut orders = [
        (first_pda, first_bump, first.sell_token_account),
        (second_pda, second_bump, second.sell_token_account),
    ];
    orders.sort_by_key(|(pda, _, _)| *pda);
    let [(lo_pda, lo_bump, lo_token), (hi_pda, hi_bump, hi_token)] = orders;

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            raw_begin_settle(
                &program_id,
                1,
                &[hi_pda, lo_pda],
                &[hi_token, lo_token],
                &[hi_bump, lo_bump],
            ),
        ),
        SettlementError::OrdersNotStrictlyIncreasing,
    );
}

#[test]
fn rejects_cancelled_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // There's no cancel instruction yet, and `CreateOrder` always writes an
    // active order, so write the PDA directly with the `cancelled` flag set. The
    // account still sits at the canonical PDA holding a matching intent, so it
    // clears the provenance check and the cancelled flag is what trips the
    // rejection.
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let intent = sample_intent(payer.pubkey(), sell_token, 0);
    let (order_pda, _bump) = find_order_pda(&program_id, &intent.uid());

    let data: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: true,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: payer.pubkey(),
        intent: intent.clone(),
    })
    .into();
    svm.set_account(
        order_pda,
        Account {
            lamports: svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE),
            data: data.to_vec(),
            owner: program_id,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("placing a cancelled order at its canonical PDA should succeed");

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            begin_settle(&program_id, 1, &[intent]),
        ),
        SettlementError::OrderCancelled,
    );
}

#[test]
fn rejects_expired_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let valid_to = 1_000_000;
    let intent = settleable_order_valid_to(&mut svm, &program_id, &payer, &mint, valid_to, 0);
    let after_expiration = i64::from(valid_to) + 1;
    set_unix_timestamp(&mut svm, after_expiration);

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            begin_settle(&program_id, 1, &[intent]),
        ),
        SettlementError::OrderExpired,
    );
}

#[test]
fn settles_order_at_exact_valid_to() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let valid_to = 1_000_000;
    let intent = settleable_order_valid_to(&mut svm, &program_id, &payer, &mint, valid_to, 0);
    set_unix_timestamp(&mut svm, i64::from(valid_to));

    send_settlement(
        &mut svm,
        &program_id,
        &payer,
        begin_settle(&program_id, 1, &[intent]),
    )
    .expect("an order is still settleable at exactly valid_to");
}
