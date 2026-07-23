//! Integration tests for the per-order amount checks `BeginSettle` runs on a
//! settlement: the limit price, the sell/buy caps, and the fill-or-kill
//! requirement, plus the cumulative fill tracking that enforces the caps across
//! several settlements. Each test settles one or more orders in a minimal
//! `[BeginSettle, FinalizeSettle]` pair, pulling `amount_in` of the sell token
//! and pushing `amount_out` of the buy token, and asserts the settlement either
//! succeeds or is rejected with the expected error.

use crate::common::{
    buffer,
    order::OrderBuilder,
    settlement::{BEGIN_INDEX, FINALIZE_INDEX},
    setup, to_instruction_error, token,
};
use litesvm::LiteSVM;
use settlement_client::instructions::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull,
};
use settlement_client::settlement_interface::{
    data::intent::{OrderIntent, OrderKind},
    data::order::{EncodedOrderAccount, OrderAccount},
    pda::order::find_order_pda,
    Instruction, SettlementError,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

fn assert_settlement_error(result: Result<(), TransactionError>, expected: SettlementError) {
    assert_eq!(
        result.err(),
        Some(TransactionError::InstructionError(
            BEGIN_INDEX,
            to_instruction_error(expected),
        )),
    );
}

/// Read `intent`'s order PDA and return its persisted `(amount_withdrawn,
/// amount_received)` cumulative fill totals.
fn order_fill(svm: &LiteSVM, program_id: &Pubkey, intent: &OrderIntent) -> (u64, u64) {
    let (order_pda, _bump) = find_order_pda(program_id, &intent.uid());
    let data = svm
        .get_account(&order_pda)
        .expect("the order account exists")
        .data;
    let bytes: [u8; EncodedOrderAccount::SIZE] = data
        .try_into()
        .expect("order account has the expected size");
    let order = OrderAccount::try_from(bytes).expect("valid order account");
    (order.amount_withdrawn, order.amount_received)
}

/// Do everything required to settle the input intent with the specified in/out
/// amounts as long as the limit price is respected.
fn settle(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    intent: &OrderIntent,
    amount_in: u64,
    amount_out: u64,
) -> Result<(), TransactionError> {
    settle_all(
        svm,
        program_id,
        payer,
        &[(intent, &[amount_in], amount_out)],
    )
}

/// Like [`settle`] but settles several orders and pulls in a single settlement.
fn settle_all(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[(&OrderIntent, &[u64], u64)],
) -> Result<(), TransactionError> {
    let mut initialized: Vec<InitializedIntent> = vec![];
    let mut finalized: Vec<FinalizedIntent> = vec![];
    for &(intent, pulls, amount_out) in orders {
        // Sell side: fund and delegate the total pulled, and give each pull its
        // own throwaway destination of the sell mint to pull into.
        let amount_in: u64 = pulls.iter().sum();
        token::fund_and_delegate(
            svm,
            program_id,
            payer,
            &intent.sell_token_account,
            amount_in,
        );
        let sell_mint = token::mint_of(svm, &intent.sell_token_account);
        let mut pull_list: Vec<Pull> = vec![];
        for &amount in pulls {
            let destination =
                token::create_token_account(svm, payer, &sell_mint, &Pubkey::new_unique());
            pull_list.push(Pull {
                destination,
                amount,
            });
        }
        // Leak the pulls so the `InitializedIntent` can borrow them until the
        // builder consumes every order's pulls at once, below.
        let pulls: &[Pull] = Box::leak(pull_list.into_boxed_slice());
        initialized.push(InitializedIntent { intent, pulls });

        // Buy side: fund the buffer so the push can draw `amount_out`.
        let buy_mint = token::mint_of(svm, &intent.buy_token_account);
        buffer::ensure_funded(svm, program_id, payer, &buy_mint, amount_out);

        finalized.push(FinalizedIntent {
            intent,
            mint: buy_mint,
            amount: amount_out,
        });
    }

    let begin = Instruction::from(BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders: &initialized,
    });
    let finalize = Instruction::from(FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &finalized,
    });
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

// --- Limit price ---------------------------------------------------------

#[test]
fn sell_order_succeeds_at_limit_price() {
    let (mut svm, program_id, payer) = setup();

    // Sell up to 1_200_000 for at least 3_600_000: a 3:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(1_200_000)
        .buy_amount(3_600_000)
        .build();

    // Sell 400_000, receive exactly the proportional minimum 1_200_000.
    settle(&mut svm, &program_id, &payer, &intent, 400_000, 1_200_000)
        .expect("a price exactly at the limit should be accepted");
    assert_eq!(token::balance(&svm, &intent.buy_token_account), 1_200_000);
}

#[test]
fn sell_order_below_limit_price_is_rejected() {
    let (mut svm, program_id, payer) = setup();

    // Sell up to 900_000 for at least 1_800_000: a 2:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(900_000)
        .buy_amount(1_800_000)
        .build();

    // One token short of the proportional minimum (600_000) for the pull.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 300_000, 599_999),
        SettlementError::LimitPriceViolated,
    );
}

#[test]
fn sell_order_above_limit_price_succeeds() {
    let (mut svm, program_id, payer) = setup();

    // Sell up to 500_000 for at least 2_500_000: a 5:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(500_000)
        .buy_amount(2_500_000)
        .build();

    // A better-than-limit price (more buy than the 500_000 minimum) is fine.
    settle(&mut svm, &program_id, &payer, &intent, 100_000, 750_000)
        .expect("a better-than-limit price should be accepted");
    assert_eq!(token::balance(&svm, &intent.buy_token_account), 750_000);
}

#[test]
fn buy_order_succeeds_at_limit_price() {
    let (mut svm, program_id, payer) = setup();

    // Buy up to 800_000, spending at most 2_400_000: a 3:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .sell_amount(2_400_000)
        .buy_amount(800_000)
        .build();

    // Buy 400_000 for exactly the proportional maximum spend of 1_200_000.
    settle(&mut svm, &program_id, &payer, &intent, 1_200_000, 400_000)
        .expect("a price exactly at the limit should be accepted");
    assert_eq!(token::balance(&svm, &intent.buy_token_account), 400_000);
}

#[test]
fn buy_order_above_limit_price_succeeds() {
    let (mut svm, program_id, payer) = setup();

    // Buy up to 800_000, spending at most 2_400_000: a 3:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .sell_amount(2_400_000)
        .buy_amount(800_000)
        .build();

    // A better-than-limit price (less spent than the 1_200_000 maximum) is fine.
    settle(&mut svm, &program_id, &payer, &intent, 900_000, 400_000)
        .expect("a better-than-limit price should be accepted");
    assert_eq!(token::balance(&svm, &intent.buy_token_account), 400_000);
}

#[test]
fn buy_order_below_limit_price_is_rejected() {
    let (mut svm, program_id, payer) = setup();

    // Buy up to 500_000, spending at most 2_000_000: a 4:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .sell_amount(2_000_000)
        .buy_amount(500_000)
        .build();

    // One token more than the proportional maximum spend (800_000) for the buy.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 800_001, 200_000),
        SettlementError::LimitPriceViolated,
    );
}

// --- Per-order locality --------------------------------------------------

#[test]
fn sell_settlement_rejected_when_one_order_below_limit() {
    let (mut svm, program_id, payer) = setup();

    // The check is per order: a fill well above one order's limit can't excuse
    // another order settled below its own.
    let generous = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();
    let violating = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(900_000)
        .buy_amount(1_800_000)
        .build();

    assert_settlement_error(
        settle_all(
            &mut svm,
            &program_id,
            &payer,
            &[
                // Far above its 2:1 limit: 1_000_000 out for 250_000 in.
                (&generous, &[250_000], 1_000_000),
                // One token short of its 2:1 limit: 599_999 out for 300_000 in.
                (&violating, &[300_000], 599_999),
            ],
        ),
        SettlementError::LimitPriceViolated,
    );
}

#[test]
fn buy_settlement_rejected_when_one_order_below_limit() {
    let (mut svm, program_id, payer) = setup();

    let generous = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .sell_amount(2_400_000)
        .buy_amount(800_000)
        .build();
    let violating = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .sell_amount(2_000_000)
        .buy_amount(500_000)
        .build();

    assert_settlement_error(
        settle_all(
            &mut svm,
            &program_id,
            &payer,
            &[
                // Far above its 3:1 limit: buy 400_000 spending only 600_000.
                (&generous, &[600_000], 400_000),
                // One token over its 4:1 limit: buy 200_000 spending 800_001.
                (&violating, &[400_001], 100_000),
            ],
        ),
        SettlementError::LimitPriceViolated,
    );
}

// --- Multiple pulls ------------------------------------------------------

#[test]
fn multiple_pulls_clearing_the_limit_are_accepted() {
    let (mut svm, program_id, payer) = setup();

    // Sell up to 1_000_000 for at least 2_000_000: a 2:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // Three pulls totalling 600_000 need at least 1_200_000 back; paying exactly
    // that clears the limit. The check sums the pulls, so the split is irrelevant.
    settle_all(
        &mut svm,
        &program_id,
        &payer,
        &[(&intent, &[300_000, 200_000, 100_000], 1_200_000)],
    )
    .expect("a payment clearing the limit for the summed pull should be accepted");
    assert_eq!(token::balance(&svm, &intent.buy_token_account), 1_200_000);
}

#[test]
fn multiple_pulls_below_the_limit_are_rejected() {
    let (mut svm, program_id, payer) = setup();

    // Sell up to 1_000_000 for at least 2_000_000: a 2:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // One token short of the 1_200_000 the summed 600_000 pull requires; the
    // shortfall can't be hidden by splitting the pull across parts.
    assert_settlement_error(
        settle_all(
            &mut svm,
            &program_id,
            &payer,
            &[(&intent, &[300_000, 200_000, 100_000], 1_199_999)],
        ),
        SettlementError::LimitPriceViolated,
    );
}

// --- Fill amounts (caps, fill-or-kill, cumulative) -----------------------

#[test]
fn sell_order_cannot_exceed_its_sell_amount() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // Pull 1_500_000 > the 1_000_000 sell amount, paid to match the limit price
    // so only the sell cap can reject it.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 1_500_000, 3_000_000),
        SettlementError::FillExceedsOrderAmount,
    );
}

#[test]
fn buy_order_cannot_exceed_its_buy_amount() {
    let (mut svm, program_id, payer) = setup();

    // Buy up to 1_000_000, spending at most 2_000_000.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .sell_amount(2_000_000)
        .buy_amount(1_000_000)
        .build();

    // Receive 1_500_000 > the 1_000_000 buy amount, spending within the limit.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 1_000_000, 1_500_000),
        SettlementError::FillExceedsOrderAmount,
    );
}

#[test]
fn fill_or_kill_order_must_be_filled_completely() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .partially_fillable(false)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // Selling only half a fill-or-kill order isn't allowed, even at the limit.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 500_000, 1_000_000),
        SettlementError::OrderNotFullyFilled,
    );
}

#[test]
fn partially_fillable_order_fills_across_settlements() {
    let (mut svm, program_id, payer) = setup();

    // Sell up to 1_000_000 for at least 2_000_000: a 2:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .partially_fillable(true)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // Two settlements that together fill the order exactly, each at the limit.
    settle(&mut svm, &program_id, &payer, &intent, 400_000, 800_000)
        .expect("the first partial fill should be accepted");
    // The first fill's totals are persisted to the order PDA.
    assert_eq!(order_fill(&svm, &program_id, &intent), (400_000, 800_000));

    settle(&mut svm, &program_id, &payer, &intent, 600_000, 1_200_000)
        .expect("a second fill completing the order should be accepted");
    // The second fill accumulates onto the first.
    assert_eq!(
        order_fill(&svm, &program_id, &intent),
        (1_000_000, 2_000_000)
    );

    // The buy token account accumulates both settlements' proceeds.
    assert_eq!(token::balance(&svm, &intent.buy_token_account), 2_000_000);
}

#[test]
fn order_cannot_be_overfilled_across_settlements() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .partially_fillable(true)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // Fill 600_000 first; the order PDA now records 600_000 withdrawn.
    settle(&mut svm, &program_id, &payer, &intent, 600_000, 1_200_000)
        .expect("the first partial fill should be accepted");

    // A second 500_000 pull would take 1_100_000 in total, past the sell amount.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 500_000, 1_000_000),
        SettlementError::FillExceedsOrderAmount,
    );
}

#[test]
fn buy_order_cannot_be_overfilled_across_settlements() {
    let (mut svm, program_id, payer) = setup();

    // Buy up to 1_000_000, spending at most 2_000_000: a 2:1 limit.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Buy)
        .partially_fillable(true)
        .sell_amount(2_000_000)
        .buy_amount(1_000_000)
        .build();

    // Buy 600_000 first (spending 1_200_000 at the limit); the order PDA now
    // records 600_000 received.
    settle(&mut svm, &program_id, &payer, &intent, 1_200_000, 600_000)
        .expect("the first partial buy should be accepted");

    // Buying another 500_000 would total 1_100_000, past the buy amount, even
    // though this settlement's spend stays within the limit price.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 1_000_000, 500_000),
        SettlementError::FillExceedsOrderAmount,
    );
}

#[test]
fn fill_or_kill_order_cannot_be_settled_twice() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .partially_fillable(false)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    // A full fill-or-kill fill is accepted once.
    settle(&mut svm, &program_id, &payer, &intent, 1_000_000, 2_000_000)
        .expect("a full fill-or-kill fill should be accepted");

    // Advance the blockhash so the identical second attempt (same amounts to the
    // same accounts) isn't rejected as a duplicate transaction before it runs.
    svm.expire_blockhash();

    // Settling it again would take more than the sell amount in total.
    assert_settlement_error(
        settle(&mut svm, &program_id, &payer, &intent, 1_000_000, 2_000_000),
        SettlementError::FillExceedsOrderAmount,
    );
}

#[test]
fn settlement_rejected_when_one_order_exceeds_its_amount() {
    let (mut svm, program_id, payer) = setup();

    // The cap is per order: one order settled within its amount can't excuse
    // another pulled past its sell amount in the same settlement.
    let ok = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .partially_fillable(true)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();
    let overfilled = OrderBuilder::new(&mut svm, &program_id, &payer)
        .kind(OrderKind::Sell)
        .partially_fillable(false)
        .sell_amount(1_000_000)
        .buy_amount(2_000_000)
        .build();

    assert_settlement_error(
        settle_all(
            &mut svm,
            &program_id,
            &payer,
            &[
                // Within its amount and at the limit.
                (&ok, &[400_000], 800_000),
                // Pulls 1_500_000 > its 1_000_000 sell amount.
                (&overfilled, &[1_500_000], 3_000_000),
            ],
        ),
        SettlementError::FillExceedsOrderAmount,
    );
}
