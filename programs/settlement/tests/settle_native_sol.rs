//! Integration tests for settling orders whose proceeds are native SOL.

use crate::common::{
    assert_instruction_error_at, buffer, lamports,
    order::{buy_account, buy_sol_account, create_order_pda, settlable_intent, OrderBuilder},
    send, send_metered,
    settlement::{
        build_matching_settlement, build_settlement, build_staged_settlement, stage_order,
        BEGIN_INDEX, FINALIZE_INDEX,
    },
    setup_settle_ready, token, unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{
    data::intent::{Asset, OrderIntent, OrderKind, ENCODED_NATIVE_SOL_TRANSFER},
    instruction::settle::FinalizeSettle as FinalizeSettleRaw,
    pda::{
        buffer::find_buffer_pda,
        state::{
            NATIVE_SOL_BUFFER_PDA, NATIVE_SOL_BUFFER_PDA_AND_BUMP, STATE_PDA, STATE_PDA_AND_BUMP,
        },
    },
    SettlementError,
};
use cow_settlement_client::instruction::{CreateBuffers, FinalizedIntent};
use cow_settlement_interface::token_program::TokenProgram;
use solana_sdk::{signer::Signer, transaction::TransactionError};

mod common;

/// Assert the transaction failed in `BeginSettle` (at [`BEGIN_INDEX`]) with
/// `expected`. Every push-source check lives there, so that is where a
/// malformed native push is caught.
#[track_caller]
fn assert_begin_error<T>(result: Result<T, TransactionError>, expected: SettlementError) {
    assert_instruction_error_at(BEGIN_INDEX, result, expected);
}

#[test]
fn happy_path_sell_tokens_for_native_sol() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .sell_amount(1_000)
        .buy_amount(2_000_000)
        .partially_fillable(false)
        .kind(OrderKind::Sell)
        .build();
    let staged = stage_order(&mut svm, &program_id, &payer, &intent, &[1_000], 2_000_000);
    let before = lamports(&svm, &NATIVE_SOL_BUFFER_PDA);

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
    assert_eq!(lamports(&svm, &buy_sol_account(&intent)), 2_000_000);
    assert_eq!(lamports(&svm, &NATIVE_SOL_BUFFER_PDA), before - 2_000_000);
}

/// The native SOL buffer `CreateBuffer` creates is one `FinalizeSettle` can pay
/// out of, not just the account the other tests put in place.
#[test]
fn happy_path_pays_out_of_a_created_native_sol_buffer() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .build();
    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[ENCODED_NATIVE_SOL_TRANSFER],
    };
    send(&mut svm, &payer, &[ix.into()]).expect("create_buffer should succeed");
    svm.airdrop(&NATIVE_SOL_BUFFER_PDA, 1_000_000)
        .expect("airdrop to the native SOL buffer should succeed");
    let funded = lamports(&svm, &NATIVE_SOL_BUFFER_PDA);

    let instructions = build_matching_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: 1_000_000,
        }],
    );
    send(&mut svm, &solver, &instructions).expect("the native push should be paid");

    assert_eq!(lamports(&svm, &buy_sol_account(&intent)), 1_000_000);
    assert_eq!(lamports(&svm, &NATIVE_SOL_BUFFER_PDA), funded - 1_000_000);
}

/// A settlement mixing both kinds of push: several orders paid out of a buffer,
/// alongside as many paid out of the native SOL buffer's lamports.
///
/// Settling breaks with `UnbalancedInstruction` when a native order sorts
/// before a token order, and order PDAs sort effectively at random, so a single
/// pair of orders would miss that half the time. With many orders it is all but
/// certain to happen, and the test asserts that it did.
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
                .buy_sol()
                .salt(MIXED_ORDER_COUNT + i)
                .build()
        })
        .collect();

    let spl_total: u64 = (0..MIXED_ORDER_COUNT).map(spl_amount).sum();
    let sol_total: u64 = (0..MIXED_ORDER_COUNT).map(sol_amount).sum();
    let buffer_funding = spl_total * 2;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, buffer_funding);
    let sol_funding = sol_total * 2;
    let funded = buffer::add_native_lamports(&mut svm, sol_funding);

    // The builder sorts the orders by PDA, so the order they are listed in here
    // doesn't matter.
    let orders: Vec<_> = (0..MIXED_ORDER_COUNT)
        .map(|i| FinalizedIntent {
            intent: &spl_intents[usize::from(i)],
            amount: spl_amount(i),
        })
        .chain((0..MIXED_ORDER_COUNT).map(|i| FinalizedIntent {
            intent: &sol_intents[usize::from(i)],
            amount: sol_amount(i),
        }))
        .collect();

    let instructions = build_matching_settlement(&program_id, &solver.pubkey(), &orders);

    // Sanity: do we have a native push in the first half of the batch?
    let finalize_accounts = &instructions[usize::from(FINALIZE_INDEX)].accounts;
    // each order is two accounts, source and destination. We only want to confirm the address.
    let push_sources: Vec<_> = finalize_accounts[finalize_accounts.len() - 2 * orders.len()..]
        .iter()
        .map(|push| push.pubkey)
        .collect();

    // If we see the native SOL buffer in the first half then we are good.
    assert!(
        push_sources[..orders.len() / 2].contains(&NATIVE_SOL_BUFFER_PDA),
        "no native order sorted into the first half; pick different salts",
    );

    send(&mut svm, &solver, &instructions).expect("a mixed settlement should be paid");

    for (i, intent) in spl_intents.iter().enumerate() {
        assert_eq!(
            token::balance(&svm, &buy_account(intent)),
            spl_amount(i as u8),
            "SPL order {i} should be paid out of the buffer",
        );
    }
    for (i, intent) in sol_intents.iter().enumerate() {
        assert_eq!(
            lamports(&svm, &buy_sol_account(intent)),
            sol_amount(i as u8),
            "native order {i} should be paid out of the native SOL buffer",
        );
    }
    assert_eq!(
        token::balance(&svm, &buffer_pda),
        buffer_funding - spl_total
    );
    assert_eq!(lamports(&svm, &NATIVE_SOL_BUFFER_PDA), funded - sol_total);
}

/// Two orders buying SOL both draw on the one balance, the way two orders
/// buying the same token both draw on its one buffer.
#[test]
fn happy_path_multiple_native_orders_can_settle() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .salt(0)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .salt(1)
        .build();
    let funded = buffer::add_native_lamports(&mut svm, 9_000_000);

    let amount0 = 1_000_000;
    let amount1 = 2_000_000;
    let instructions = build_matching_settlement(
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

    assert_eq!(lamports(&svm, &buy_sol_account(&intent0)), amount0);
    assert_eq!(lamports(&svm, &buy_sol_account(&intent1)), amount1);
    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        funded - amount0 - amount1
    );
}

/// Two orders paying out to the same address both land there: the second
/// credit adds to the first rather than overwriting it.
#[test]
fn happy_path_native_orders_sharing_a_destination() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let destination = unique_pubkey();
    let intent0 = OrderIntent {
        buy: Asset::Native(destination),
        ..settlable_intent(&mut svm, &payer, payer.pubkey(), 0)
    };
    let intent1 = OrderIntent {
        buy: Asset::Native(destination),
        ..settlable_intent(&mut svm, &payer, payer.pubkey(), 1)
    };
    create_order_pda(&mut svm, &program_id, &payer, &intent0);
    create_order_pda(&mut svm, &program_id, &payer, &intent1);
    let funded = buffer::add_native_lamports(&mut svm, 9_000_000);

    let amount0 = 1_000_000;
    let amount1 = 2_000_000;
    let instructions = build_matching_settlement(
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
    send(&mut svm, &solver, &instructions).expect("both pushes to one destination should be paid");

    assert_eq!(lamports(&svm, &destination), amount0 + amount1);
    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        funded - amount0 - amount1
    );
}

#[test]
fn happy_path_zero_amount() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .build();
    let before = lamports(&svm, &NATIVE_SOL_BUFFER_PDA);

    let instructions = build_matching_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: 0,
        }],
    );
    send(&mut svm, &solver, &instructions).expect("a zero-amount native push should succeed");

    assert_eq!(lamports(&svm, &buy_sol_account(&intent)), 0);
    assert_eq!(lamports(&svm, &NATIVE_SOL_BUFFER_PDA), before);
}

#[test]
fn happy_path_native_sol_buffer_receiver_still_works() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderIntent {
        buy: Asset::Native(NATIVE_SOL_BUFFER_PDA),
        ..settlable_intent(&mut svm, &payer, payer.pubkey(), 0)
    };
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    let funded = buffer::add_native_lamports(&mut svm, 1_000_000);

    let instructions = build_matching_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: 100,
        }],
    );
    send(&mut svm, &solver, &instructions)
        .expect("a push that credits its own source should settle as a no-op");

    assert_eq!(lamports(&svm, &NATIVE_SOL_BUFFER_PDA), funded);
}

#[test]
fn rejects_a_push_spending_the_native_sol_buffers_rent() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .build();
    let funding = 1_000_000;
    let funded = buffer::add_native_lamports(&mut svm, funding);

    // sanity: Ensure that the buffer exists and holds its rent on top of the funding
    let native_buffer = svm
        .get_account(&NATIVE_SOL_BUFFER_PDA)
        .expect("native SOL buffer must exist");
    assert!(native_buffer.lamports > funding);

    // One lamport past the balance that isn't rent, so the push is affordable
    // but leaves the account under-funded.
    let instructions = build_matching_settlement(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: funding + 1,
        }],
    );
    let err = send(&mut svm, &solver, &instructions)
        .expect_err("a push into the native SOL buffer's rent must be rejected");
    assert!(
        matches!(err, TransactionError::InsufficientFundsForRent { .. }),
        "expected a rent failure, got {err:?}",
    );

    assert_eq!(lamports(&svm, &NATIVE_SOL_BUFFER_PDA), funded);
    assert_eq!(lamports(&svm, &buy_sol_account(&intent)), 0);
}

#[test]
fn rejects_a_push_larger_than_the_whole_balance() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .build();
    let balance = lamports(&svm, &NATIVE_SOL_BUFFER_PDA);

    let instructions = build_matching_settlement(
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
        .buy_sol()
        .build();
    buffer::add_native_lamports(&mut svm, 1_000_000);

    let mint = token::create_mint(&mut svm, &payer);
    buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, 1_000);
    let (buffer_pda, buffer_bump) = find_buffer_pda(&program_id, &mint);

    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 100,
    }];
    let finalize = FinalizeSettleRaw {
        program_id,
        state_pda: STATE_PDA,
        begin_ix_index: BEGIN_INDEX.into(),
        source_buffers: &[buffer_pda],
        destinations: &[buy_sol_account(&intent)],
        bumps: &[buffer_bump],
        amounts: &[100],
        only_token_program: None,
    };

    let instructions = build_settlement(&program_id, &solver.pubkey(), &orders, finalize);
    assert_begin_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::PushSourceNotBuffer,
    );
}

#[test]
fn rejects_a_native_push_to_wrong_destination() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_sol()
        .build();
    buffer::add_native_lamports(&mut svm, 1_000_000);
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 100,
    }];
    let finalize = FinalizeSettleRaw {
        program_id,
        state_pda: STATE_PDA,
        begin_ix_index: BEGIN_INDEX.into(),
        source_buffers: &[NATIVE_SOL_BUFFER_PDA],
        destinations: &[unique_pubkey()],
        bumps: &[NATIVE_SOL_BUFFER_PDA_AND_BUMP.1],
        amounts: &[100],
        only_token_program: None,
    };

    let instructions = build_settlement(&program_id, &solver.pubkey(), &orders, finalize);
    assert_begin_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::PushDestinationMismatch,
    );
}
