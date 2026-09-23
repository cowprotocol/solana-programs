//! Integration tests for settling orders whose proceeds are native SOL.

use crate::common::{
    assert_instruction_error_at, buffer, lamports,
    order::{create_order_pda, settlable_intent, OrderBuilder},
    send, send_metered,
    settlement::{
        build_settlement, build_staged_settlement, stage_order, BEGIN_INDEX, FINALIZE_INDEX,
    },
    setup_settle_ready, state, token, unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{
    data::intent::{Asset, OrderIntent, OrderKind},
    instruction::settle::FinalizeSettle as FinalizeSettleRaw,
    pda::{buffer::find_buffer_pda, state::find_state_pda},
    token_program::NATIVE_SOL_MINT,
    Instruction, SettlementError,
};
use cow_settlement_client::instruction::{FinalizeSettle, FinalizedIntent};
use solana_sdk::{pubkey::Pubkey, signer::Signer, transaction::TransactionError};

mod common;

/// Assert the transaction failed in `BeginSettle` (at [`BEGIN_INDEX`]) with
/// `expected`. Every push-source check lives there, so that is where a
/// malformed native push is caught.
#[track_caller]
fn assert_begin_error<T>(result: Result<T, TransactionError>, expected: SettlementError) {
    assert_instruction_error_at(BEGIN_INDEX, result, expected);
}

/// The `[BeginSettle, FinalizeSettle]` pair settling `orders` with no pulls,
/// the finalize pushing each order's amount.
fn native_sol_settlement(
    program_id: &Pubkey,
    solver: &Pubkey,
    orders: &[FinalizedIntent],
) -> Vec<Instruction> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders,
        only_token_program: None,
    };
    build_settlement(program_id, solver, orders, finalize)
}

#[test]
fn happy_path_sell_tokens_for_native_sol() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .sell_amount(1_000)
        .buy_amount(2_000_000)
        .partially_fillable(false)
        .kind(OrderKind::Sell)
        .build();
    let staged = stage_order(&mut svm, &program_id, &payer, &intent, &[1_000], 2_000_000);
    let (state_pda, _bump) = find_state_pda(&program_id);
    let before = lamports(&svm, &state_pda);

    let instructions =
        build_staged_settlement(&program_id, &solver.pubkey(), &[staged], Vec::new());
    send_metered(
        &mut svm,
        &solver,
        &instructions,
        common::benchmark::BenchLabel::Settle,
    )
    .expect("a fully filled SOL buy should settle");

    assert_eq!(token::balance(&svm, &intent.sell.token_account), 0);
    assert_eq!(lamports(&svm, &intent.buy.account()), 2_000_000);
    assert_eq!(lamports(&svm, &state_pda), before - 2_000_000);
}

/// A settlement mixing both kinds of push: several orders paid out of a buffer,
/// interleaved with as many paid out of the state PDA's lamports.
#[test]
fn happy_path_with_many_payouts() {
    /// Orders of each payout kind the mixed settlement carries.
    const MIXED_ORDER_COUNT: u8 = 5;

    // The amount pushed to the `i`th order of each kind. Distinct per order, so
    // no assertion below passes on a payout that landed in the wrong account.
    let spl_amount = |i: u8| 3_000 + u64::from(i) * 100;
    let sol_amount = |i: u8| 1_000_000 + u64::from(i) * 10_000;

    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let mint = token::create_mint(&mut svm, &payer);
    // Salts have to be unique across both sets, since they are what give the
    // orders distinct UIDs, and so distinct order PDAs.
    let spl_intents: Vec<_> = (0..MIXED_ORDER_COUNT)
        .map(|i| {
            OrderBuilder::new(&mut svm, &program_id, &payer)
                .buy_mint(&mint)
                .salt(i)
                .build()
        })
        .collect();
    let sol_intents: Vec<_> = (0..MIXED_ORDER_COUNT)
        .map(|i| {
            OrderBuilder::new(&mut svm, &program_id, &payer)
                .buy_mint(&NATIVE_SOL_MINT)
                .salt(MIXED_ORDER_COUNT + i)
                .build()
        })
        .collect();

    let spl_total: u64 = (0..MIXED_ORDER_COUNT).map(spl_amount).sum();
    let sol_total: u64 = (0..MIXED_ORDER_COUNT).map(sol_amount).sum();
    let buffer_funding = spl_total * 2;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, buffer_funding);
    let sol_funding = sol_total * 2;
    let funded = state::fund_with_lamports(&mut svm, &program_id, sol_funding);

    // Interleaved, so the settlement alternates between the two push sources
    // rather than draining one and then the other.
    let orders: Vec<FinalizedIntent> = (0..MIXED_ORDER_COUNT)
        .flat_map(|i| {
            let index = usize::from(i);
            [
                FinalizedIntent {
                    intent: &spl_intents[index],
                    amount: spl_amount(i),
                },
                FinalizedIntent {
                    intent: &sol_intents[index],
                    amount: sol_amount(i),
                },
            ]
        })
        .collect();
    let instructions = native_sol_settlement(&program_id, &solver.pubkey(), &orders);
    send(&mut svm, &solver, &instructions).expect("a mixed settlement should be paid");

    let (state_pda, _bump) = find_state_pda(&program_id);
    for (i, intent) in spl_intents.iter().enumerate() {
        assert_eq!(
            token::balance(&svm, &intent.buy.account()),
            spl_amount(i as u8),
            "SPL order {i} should be paid out of the buffer",
        );
    }
    for (i, intent) in sol_intents.iter().enumerate() {
        assert_eq!(
            lamports(&svm, &intent.buy.account()),
            sol_amount(i as u8),
            "native order {i} should be paid out of the state PDA",
        );
    }
    assert_eq!(
        token::balance(&svm, &buffer_pda),
        buffer_funding - spl_total
    );
    assert_eq!(lamports(&svm, &state_pda), funded - sol_total);
}

/// Two orders buying SOL both draw on the one balance, the way two orders
/// buying the same token both draw on its one buffer.
#[test]
fn happy_path_multiple_native_orders_can_settle() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .salt(0)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .salt(1)
        .build();
    let funded = state::fund_with_lamports(&mut svm, &program_id, 9_000_000);

    let amount0 = 1_000_000;
    let amount1 = 2_000_000;
    let instructions = native_sol_settlement(
        &program_id,
        &solver.pubkey(),
        &[
            FinalizedIntent {
                intent: &intent0,
                amount: amount0,
            },
            FinalizedIntent {
                intent: &intent1,
                amount: amount1,
            },
        ],
    );
    send(&mut svm, &solver, &instructions).expect("both native pushes should be paid");

    let (state_pda, _bump) = find_state_pda(&program_id);
    assert_eq!(lamports(&svm, &intent0.buy.account()), amount0);
    assert_eq!(lamports(&svm, &intent1.buy.account()), amount1);
    assert_eq!(lamports(&svm, &state_pda), funded - amount0 - amount1);
}

#[test]
fn happy_path_zero_amount() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .build();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let before = lamports(&svm, &state_pda);

    let instructions = native_sol_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: 0,
        }],
    );
    send(&mut svm, &solver, &instructions).expect("a zero-amount native push should succeed");

    assert_eq!(lamports(&svm, &intent.buy.account()), 0);
    assert_eq!(lamports(&svm, &state_pda), before);
}

#[test]
fn happy_path_state_pda_receiver_still_works() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let intent = OrderIntent {
        buy: Asset::Native(state_pda),
        ..settlable_intent(&mut svm, &payer, payer.pubkey(), 0)
    };
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    let funded = state::fund_with_lamports(&mut svm, &program_id, 1_000_000);

    let instructions = native_sol_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: 100,
        }],
    );
    send(&mut svm, &solver, &instructions)
        .expect("a push that credits its own source should settle as a no-op");

    assert_eq!(lamports(&svm, &state_pda), funded);
}

#[test]
fn rejects_a_push_spending_the_state_pdas_rent() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .build();
    let funding = 1_000_000;
    let funded = state::fund_with_lamports(&mut svm, &program_id, funding);

    // One lamport past the balance that isn't rent, so the push is affordable
    // but leaves the account under-funded for its own data.
    let instructions = native_sol_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: funding + 1,
        }],
    );
    let err = send(&mut svm, &solver, &instructions)
        .expect_err("a push into the state PDA's rent must be rejected");
    assert!(
        matches!(err, TransactionError::InsufficientFundsForRent { .. }),
        "expected a rent failure, got {err:?}",
    );

    let (state_pda, _bump) = find_state_pda(&program_id);
    assert_eq!(lamports(&svm, &state_pda), funded);
    assert_eq!(lamports(&svm, &intent.buy.account()), 0);
}

#[test]
fn rejects_a_push_larger_than_the_whole_balance() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .build();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let balance = lamports(&svm, &state_pda);

    let instructions = native_sol_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: balance + 1,
        }],
    );
    assert_instruction_error_at(
        FINALIZE_INDEX,
        send(&mut svm, &solver, &instructions),
        solana_sdk::instruction::InstructionError::ArithmeticOverflow,
    );
}

#[test]
fn rejects_a_native_push_from_a_buffer() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .build();
    state::fund_with_lamports(&mut svm, &program_id, 1_000_000);

    let mint = token::create_mint(&mut svm, &payer);
    buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, 1_000);
    let (buffer_pda, buffer_bump) = find_buffer_pda(&program_id, &mint);

    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 100,
    }];
    let finalize = FinalizeSettleRaw {
        program_id,
        state_pda: find_state_pda(&program_id).0,
        begin_ix_index: BEGIN_INDEX.into(),
        source_buffers: &[buffer_pda],
        destinations: &[intent.buy.account()],
        bumps: &[buffer_bump],
        amounts: &[100],
        only_token_program: None,
    };

    let instructions = build_settlement(&program_id, &solver.pubkey(), &orders, finalize);
    assert_begin_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::PushSourceNotStatePda,
    );
}

#[test]
fn rejects_a_native_push_to_wrong_destination() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&NATIVE_SOL_MINT)
        .build();
    state::fund_with_lamports(&mut svm, &program_id, 1_000_000);

    let (state_pda, state_bump) = find_state_pda(&program_id);
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 100,
    }];
    let finalize = FinalizeSettleRaw {
        program_id,
        state_pda,
        begin_ix_index: BEGIN_INDEX.into(),
        source_buffers: &[state_pda],
        destinations: &[unique_pubkey()],
        bumps: &[state_bump],
        amounts: &[100],
        only_token_program: None,
    };

    let instructions = build_settlement(&program_id, &solver.pubkey(), &orders, finalize);
    assert_begin_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::PushDestinationMismatch,
    );
}
