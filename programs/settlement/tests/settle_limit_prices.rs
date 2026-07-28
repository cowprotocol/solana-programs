//! Integration tests for the limit-price check `BeginSettle` runs on a
//! settlement. Each test settles one order in a minimal `[BeginSettle,
//! FinalizeSettle]` pair, pulling `amount_in` of its sell token and pushing
//! `amount_out` of its buy token, and asserts the settlement either succeeds or
//! is rejected with the expected error.

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
    SettlementError,
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

    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders: &initialized,
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &finalized,
    };
    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), finalize.into()],
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

/// Settle a generous order (well above its limit) against a violating one (just
/// below its own), both of the given kinds and on a shared token pair, and
/// assert the whole settlement is rejected for the violation.
fn assert_locality_rejected(generous_kind: OrderKind, violating_kind: OrderKind) {
    // Order has a 2:1 limit.
    const SELL_AMOUNT: u64 = 900_000;
    const BUY_AMOUNT: u64 = 1_800_000;

    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let buy_mint = token::create_mint(&mut svm, &payer);

    // Both orders are partially fillable and trade the shared token pair at the
    // same limit; only their kind and fill (below) differ.
    let mut order = |kind| {
        OrderBuilder::new(&mut svm, &program_id, &payer)
            .kind(kind)
            .partially_fillable(true)
            .sell_mint(&sell_mint)
            .buy_mint(&buy_mint)
            .sell_amount(SELL_AMOUNT)
            .buy_amount(BUY_AMOUNT)
            .build()
    };
    let generous = order(generous_kind);
    let violating = order(violating_kind);

    let result = settle_all(
        &mut svm,
        &program_id,
        &payer,
        &[
            // Far above the 2:1 limit.
            (&generous, &[250_000], 1_000_000),
            // One token short of the 600_000 the 300_000 pull requires.
            (&violating, &[300_000], 599_999),
        ],
    );
    assert_eq!(
        result.err(),
        Some(TransactionError::InstructionError(
            BEGIN_INDEX,
            to_instruction_error(SettlementError::LimitPriceViolated),
        )),
        "generous={generous_kind:?} violating={violating_kind:?}",
    );
}

#[test]
fn settlement_rejected_when_one_order_below_limit() {
    // The check is per order: one order far above its limit can't excuse another
    // below its own. Sweep every buy/sell combination on a shared token pair.
    for generous_kind in [OrderKind::Sell, OrderKind::Buy] {
        for violating_kind in [OrderKind::Sell, OrderKind::Buy] {
            assert_locality_rejected(generous_kind, violating_kind);
        }
    }
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
