//! Integration tests to verify the behavior of multiple token programs
//! within a single settlement.

use crate::common::{
    buffer,
    order::OrderBuilder,
    settlement::{BEGIN_INDEX, FINALIZE_INDEX},
    setup_settle_ready, token,
    token_2022::Extensions,
    unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{data::intent::OrderIntent, Instruction};
use cow_settlement_client::instruction::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull, TokenProgram,
};
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

/// What each order in a settlement sells and buys: `amount_in` of its sell
/// token pulled out, `amount_out` of its buy token pushed in.
struct Settled<'a> {
    intent: &'a OrderIntent,
    amount_in: u64,
    amount_out: u64,
}

/// Fund and settle `orders` in one `[BeginSettle, FinalizeSettle]` pair, with
/// each instruction narrowed to the token program it is given, or naming every
/// one of them when given `None`.
///
/// Every account involved is set up under its own mint's program, so the only
/// thing a test varies is which programs the settlement says it names.
fn settle_with(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    solver: &Keypair,
    orders: &[Settled],
    begin_program: Option<TokenProgram>,
    finalize_program: Option<TokenProgram>,
) -> Result<(), TransactionError> {
    let mut initialized: Vec<InitializedIntent> = vec![];
    let mut finalized: Vec<FinalizedIntent> = vec![];
    for order in orders {
        let intent = order.intent;
        // Sell side: fund the account and delegate the pull to the state PDA,
        // then pull into a throwaway account of the same mint.
        token::fund_and_delegate(
            svm,
            program_id,
            payer,
            &intent.sell.token_account,
            order.amount_in,
        );
        let sell_mint = token::mint_of(svm, &intent.sell.token_account);
        let destination = token::create_token_account(svm, payer, &sell_mint, &unique_pubkey());
        let pulls: &[Pull] = Box::leak(Box::new([Pull {
            destination,
            amount: order.amount_in,
        }]));
        initialized.push(InitializedIntent { intent, pulls });

        // Buy side: fund the buffer so the push has something to draw from.
        let buy_mint = token::mint_of(svm, &intent.buy.account());
        buffer::ensure_funded(svm, program_id, payer, &buy_mint, order.amount_out);
        finalized.push(FinalizedIntent {
            intent,
            amount: order.amount_out,
        });
    }

    let begin = BeginSettle {
        program_id: *program_id,
        solver: solver.pubkey(),
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        only_token_program: begin_program,
        orders: &initialized,
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: finalize_program,
        orders: &finalized,
    };
    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), finalize.into()],
        Some(&payer.pubkey()),
        &[payer, solver],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .map(|_| ())
        .map_err(|error| error.err)
}

/// An order selling a token under `sell_program` and buying one under
/// `buy_program`, priced 1:1 and partially fillable.
fn order_across(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    salt: u8,
    sell_program: &Pubkey,
    buy_program: &Pubkey,
) -> OrderIntent {
    // Bare mints: what these tests vary is which program a token lives under,
    // and a transfer-fee mint would refuse the unchecked `Transfer` the program
    // settles with before the crossing under test got a chance to matter.
    let sell_mint = token::create_mint_under(svm, payer, sell_program, Extensions::None);
    let buy_mint = token::create_mint_under(svm, payer, buy_program, Extensions::None);
    let intent = OrderBuilder::new(svm, program_id, payer)
        .salt(salt)
        .sell_mint(&sell_mint)
        .buy_mint(&buy_mint)
        .sell_amount(1_000)
        .buy_amount(1_000)
        .build();
    // The order's accounts have to have landed under the programs asked for, or
    // a test meant to settle Token-2022 would quietly be settling legacy tokens.
    assert_eq!(
        token::program_of(svm, &intent.sell.token_account),
        *sell_program,
    );
    assert_eq!(token::program_of(svm, &intent.buy.account()), *buy_program,);
    intent
}

#[test]
fn settles_orders_under_both_token_programs_simultaneously() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let legacy = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TokenProgram::SplToken.address(),
        &TokenProgram::SplToken.address(),
    );
    let token_2022 = order_across(
        &mut svm,
        &program_id,
        &payer,
        1,
        &TokenProgram::Token2022.address(),
        &TokenProgram::Token2022.address(),
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &solver,
        &[
            Settled {
                intent: &legacy,
                amount_in: 400,
                amount_out: 400,
            },
            Settled {
                intent: &token_2022,
                amount_in: 700,
                amount_out: 700,
            },
        ],
        None,
        None,
    )
    .expect("a settlement carrying both programs should settle orders under either");

    assert_eq!(token::balance(&svm, &legacy.buy.account()), 400);
    assert_eq!(token::balance(&svm, &token_2022.buy.account()), 700);
    // Both sell sides were drained by their own program's transfer.
    assert_eq!(token::balance(&svm, &legacy.sell.token_account), 0);
    assert_eq!(token::balance(&svm, &token_2022.sell.token_account), 0);
}

#[test]
fn settles_an_order_that_crosses_token_programs() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TokenProgram::SplToken.address(),
        &TokenProgram::Token2022.address(),
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &solver,
        &[Settled {
            intent: &intent,
            amount_in: 250,
            amount_out: 250,
        }],
        None,
        None,
    )
    .expect("an order selling under one program and buying under the other should settle");

    assert_eq!(token::balance(&svm, &intent.buy.account()), 250);
    assert_eq!(token::balance(&svm, &intent.sell.token_account), 0);
}

#[test]
fn settles_token_2022_orders_without_carrying_the_legacy_program() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TokenProgram::Token2022.address(),
        &TokenProgram::Token2022.address(),
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &solver,
        &[Settled {
            intent: &intent,
            amount_in: 300,
            amount_out: 300,
        }],
        Some(TokenProgram::Token2022),
        Some(TokenProgram::Token2022),
    )
    .expect("a Token-2022-only settlement should settle Token-2022 orders");

    assert_eq!(token::balance(&svm, &intent.buy.account()), 300);
}

#[test]
fn settles_legacy_orders_without_carrying_the_token_2022_program() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TokenProgram::SplToken.address(),
        &TokenProgram::SplToken.address(),
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &solver,
        &[Settled {
            intent: &intent,
            amount_in: 500,
            amount_out: 500,
        }],
        Some(TokenProgram::SplToken),
        Some(TokenProgram::SplToken),
    )
    .expect("a legacy-only settlement should not have to carry Token-2022");

    assert_eq!(token::balance(&svm, &intent.buy.account()), 500);
}

#[test]
fn settles_with_the_token_program_slots_swapped() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TokenProgram::SplToken.address(),
        &TokenProgram::SplToken.address(),
    );
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &intent.sell.token_account,
        100,
    );
    let sell_mint = token::mint_of(&svm, &intent.sell.token_account);
    let buy_mint = token::mint_of(&svm, &intent.buy.account());
    buffer::ensure_funded(&mut svm, &program_id, &payer, &buy_mint, 100);
    let destination = token::create_token_account(&mut svm, &payer, &sell_mint, &unique_pubkey());

    let pulls = [Pull {
        destination,
        amount: 100,
    }];
    let mut begin = Instruction::from(BeginSettle {
        program_id,
        solver: solver.pubkey(),
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        only_token_program: None,
        orders: &[InitializedIntent {
            intent: &intent,
            pulls: &pulls,
        }],
    });
    // `BeginSettle`'s accounts are `[solver, sysvar, state, spl_token,
    // token_2022, ...]`, so exchanging the two slots leaves both programs
    // present but each in the other's position.
    begin.accounts.swap(3, 4);
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders: &[FinalizedIntent {
            intent: &intent,
            amount: 100,
        }],
    };

    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize.into()],
        Some(&payer.pubkey()),
        &[&payer, &solver],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("the slots only name the programs, in either order");

    assert_eq!(token::balance(&svm, &intent.buy.account()), 100);
}

#[test]
fn narrowing_begin_settle_drops_one_account_from_the_transaction() {
    const AMOUNT: u64 = 100;

    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TokenProgram::Token2022.address(),
        &TokenProgram::Token2022.address(),
    );
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &intent.sell.token_account,
        AMOUNT,
    );
    buffer::ensure_funded(
        &mut svm,
        &program_id,
        &payer,
        &intent
            .buy
            .mint()
            .expect("intent must buy with token program"),
        AMOUNT,
    );
    let destination =
        token::create_token_account(&mut svm, &payer, &intent.sell.mint, &unique_pubkey());
    let pulls = [Pull {
        destination,
        amount: AMOUNT,
    }];
    let blockhash = svm.latest_blockhash();

    let settle_tx = |only_token_program| {
        let initialized = [InitializedIntent {
            intent: &intent,
            pulls: &pulls,
        }];
        let finalized = [FinalizedIntent {
            intent: &intent,
            amount: AMOUNT,
        }];
        let begin = BeginSettle {
            program_id,
            solver: solver.pubkey(),
            finalize_ix_index: FINALIZE_INDEX.into(),
            auction_id: 0,
            only_token_program,
            orders: &initialized,
        };
        let finalize = FinalizeSettle {
            program_id,
            begin_ix_index: BEGIN_INDEX.into(),
            only_token_program,
            orders: &finalized,
        };
        Transaction::new_signed_with_payer(
            &[begin.into(), finalize.into()],
            Some(&payer.pubkey()),
            &[&payer, &solver],
            blockhash,
        )
    };

    let both = settle_tx(None);
    let narrowed_legacy = settle_tx(Some(TokenProgram::SplToken));
    let narrowed_2022 = settle_tx(Some(TokenProgram::Token2022));

    assert_eq!(
        narrowed_legacy.message.account_keys.len() + 1,
        both.message.account_keys.len(),
        "narrowing `BeginSettle` to the SPL token program should cost the transaction \
         one account fewer",
    );
    assert_eq!(
        narrowed_2022.message.account_keys.len() + 1,
        both.message.account_keys.len(),
        "narrowing `BeginSettle` to the Token2022 token program should cost the transaction \
         one account fewer",
    );

    // And the shorter transaction is still one that settles.
    svm.send_transaction(narrowed_2022)
        .expect("a settlement narrowed to the program it uses should settle");
    assert_eq!(token::balance(&svm, &intent.buy.account()), AMOUNT);
}
