//! Integration tests for the fund pushes carried by `FinalizeSettle` and
//! validated by `BeginSettle`.
//!
//! Each settlement transaction is a `[BeginSettle, FinalizeSettle]` pair (begin
//! at index 0 pointing to finalize at index 1, and vice versa). `BeginSettle`
//! settles the orders the finalize pays — created on-chain via `OrderBuilder`
//! with no pulls, so only the push side moves funds — and validates that each
//! order is paid by exactly one push to its buy token account from the canonical
//! buffer for that token's mint. `FinalizeSettle` then executes the transfers,
//! signed by the settlement state PDA that owns the buffers.

use crate::common::{
    buffer,
    order::{create_order_pda, sample_intent, OrderBuilder},
    setup, to_instruction_error, token,
};
use litesvm::LiteSVM;
use settlement_client::instructions::{BeginSettle, FinalizeSettle, SettleableOrder, SettledOrder};
use settlement_client::settlement_interface::{Instruction, SettlementError};
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

/// Assert the transaction failed in `BeginSettle` (index 0) with `expected`.
fn assert_begin_error(result: Result<(), TransactionError>, expected: SettlementError) {
    assert_eq!(
        result,
        Err(TransactionError::InstructionError(
            0,
            to_instruction_error(expected)
        )),
    );
}

/// Assert the transaction failed in `FinalizeSettle` (index 1) with `expected`.
fn assert_finalize_error(result: Result<(), TransactionError>, expected: InstructionError) {
    assert_eq!(result, Err(TransactionError::InstructionError(1, expected)));
}

/// Send `[begin, finalize]` signed by `payer`, where `finalize` is a pre-built
/// `FinalizeSettle` at index 1 and `begin` settles `orders` (with no pulls) at
/// index 0 — the same orders the finalize is expected to push to.
fn send_settlement(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[SettledOrder],
    finalize: Instruction,
) -> Result<(), TransactionError> {
    let begin_orders: Vec<SettleableOrder> = orders
        .iter()
        .map(|order| SettleableOrder {
            intent: order.intent,
            pulls: &[],
        })
        .collect();
    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: 1,
        orders: &begin_orders,
    }
    .instruction();
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    // Drop the success metadata, not needed in these tests.
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

/// Settle `orders` (begin) and push their proceeds (finalize) in a minimal
/// `[BeginSettle, FinalizeSettle]` transaction signed by `payer`.
fn finalize(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[SettledOrder],
) -> Result<(), TransactionError> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: 0,
        orders,
    }
    .instruction();
    send_settlement(svm, program_id, payer, orders, finalize)
}

#[test]
fn finalizes_with_no_pushes() {
    let (mut svm, program_id, payer) = setup();

    finalize(&mut svm, &program_id, &payer, &[]).expect("a finalize with no pushes should succeed");
}

#[test]
fn pushes_a_single_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let funding = 1_000;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, funding);

    let amount = 400;
    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            mint,
            amount,
        }],
    )
    .expect("a single push should be paid");

    assert_eq!(token::balance(&svm, &intent.buy_token_account), amount);
    assert_eq!(token::balance(&svm, &buffer_pda), funding - amount);
}

#[test]
fn pushes_several_orders_from_one_buffer() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    // Distinct orders (each `OrderBuilder` makes fresh sell and buy token
    // accounts) sharing one buy mint, so both pushes draw from one buffer.
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let funding = 10_000;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, funding);

    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[
            SettledOrder {
                intent: &intent0,
                mint,
                amount: 1_000,
            },
            SettledOrder {
                intent: &intent1,
                mint,
                amount: 2_000,
            },
        ],
    )
    .expect("several pushes from one buffer should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy_token_account), 1_000);
    assert_eq!(token::balance(&svm, &intent1.buy_token_account), 2_000);
    assert_eq!(token::balance(&svm, &buffer_pda), funding - 3_000);
}

#[test]
fn pushes_several_orders_from_different_buffers() {
    let (mut svm, program_id, payer) = setup();
    let mint0 = token::create_mint(&mut svm, &payer);
    let mint1 = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint0).build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint1).build();
    let buffer0 = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint0, 5_000);
    let buffer1 = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint1, 5_000);

    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[
            SettledOrder {
                intent: &intent0,
                mint: mint0,
                amount: 1_000,
            },
            SettledOrder {
                intent: &intent1,
                mint: mint1,
                amount: 2_000,
            },
        ],
    )
    .expect("pushes from different buffers should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy_token_account), 1_000);
    assert_eq!(token::balance(&svm, &intent1.buy_token_account), 2_000);
    assert_eq!(token::balance(&svm, &buffer0), 5_000 - 1_000);
    assert_eq!(token::balance(&svm, &buffer1), 5_000 - 2_000);
}

#[test]
fn rejects_push_to_wrong_destination() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let orders = [SettledOrder {
        intent: &intent,
        mint,
        amount: 100,
    }];

    let mut finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &orders,
    }
    .instruction();
    // Redirect the push to an account that isn't the order's buy token account.
    // Accounts: `[sysvar, state, token_program, source, destination]`.
    let destination_index = 4;
    finalize.accounts[destination_index].pubkey = Pubkey::new_unique();

    assert_begin_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        SettlementError::PushDestinationMismatch,
    );
}

#[test]
fn rejects_push_from_non_buffer_source() {
    let (mut svm, program_id, payer) = setup();
    let buy_mint = token::create_mint(&mut svm, &payer);
    let other_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &buy_mint).build();
    // The push draws from the buffer for `other_mint`, not the buy token's mint,
    // which `FinalizeSettle` rejects when it reads the destination's mint.
    let orders = [SettledOrder {
        intent: &intent,
        mint: other_mint,
        amount: 100,
    }];

    assert_finalize_error(
        finalize(&mut svm, &program_id, &payer, &orders),
        to_instruction_error(SettlementError::PushSourceNotBuffer),
    );
}

#[test]
fn rejects_fewer_pushes_than_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let orders = [SettledOrder {
        intent: &intent,
        mint,
        amount: 100,
    }];

    // A finalize carrying no pushes, paired with a begin settling one order.
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &[],
    }
    .instruction();

    assert_begin_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        SettlementError::SettledOrderPushCountMismatch,
    );
}

#[test]
fn rejects_more_pushes_than_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();

    // A finalize that pushes to one order, paired with a begin that settles none,
    // so the extra push has no order to account for it.
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &[SettledOrder {
            intent: &intent,
            mint,
            amount: 0,
        }],
    }
    .instruction();

    assert_begin_error(
        send_settlement(&mut svm, &program_id, &payer, &[], finalize),
        SettlementError::SettledOrderPushCountMismatch,
    );
}

#[test]
fn rejects_invalid_buy_token_account() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // The order's buy token account (the push destination) isn't a token account,
    // so `FinalizeSettle` can't read its mint to derive the buffer. `BeginSettle`
    // accepts it: the push destination still matches the intent's buy token.
    let not_a_token_account = Pubkey::new_unique();
    let mut intent = sample_intent(payer.pubkey(), sell_token, 0);
    intent.buy_token_account = not_a_token_account;
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    let orders = [SettledOrder {
        intent: &intent,
        mint,
        amount: 0,
    }];

    assert_finalize_error(
        finalize(&mut svm, &program_id, &payer, &orders),
        to_instruction_error(SettlementError::BuyTokenAccountInvalid),
    );
}

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let orders = [SettledOrder {
        intent: &intent,
        mint,
        amount: 100,
    }];

    let mut finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &orders,
    }
    .instruction();
    // Drop one byte so the trailing amount is no longer a whole `u64`. Begin
    // validates the push from the (unchanged) account metas and passes; finalize
    // then rejects the malformed data.
    finalize.data.pop();

    assert_finalize_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        InstructionError::InvalidInstructionData,
    );
}
