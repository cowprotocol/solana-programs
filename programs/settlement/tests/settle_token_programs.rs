//! Integration tests for the token-program slots a `BeginSettle` /
//! `FinalizeSettle` pair carries.
//!
//! Both instructions take one account per supported token program and issue
//! each transfer against the program that owns the account it moves, so a
//! single pair can settle legacy SPL Token and Token-2022 orders together. A
//! program the settlement doesn't need is left out by putting the system
//! program in its slot; a token account under a left-out program then has
//! nothing to be settled against.

use crate::common::{
    assert_settlement_error, buffer,
    order::OrderBuilder,
    settlement::{BEGIN_INDEX, FINALIZE_INDEX},
    setup, token, unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{
    data::intent::OrderIntent,
    token_program::{SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID},
    Instruction, SettlementError,
};
use cow_settlement_client::instructions::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull, TokenPrograms,
};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
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
/// each instruction carrying the token-program slots it is given.
///
/// Every account involved is set up under its own mint's program, so the only
/// thing a test varies is which programs the settlement says it carries.
fn settle_with(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[Settled],
    begin_programs: TokenPrograms,
    finalize_programs: TokenPrograms,
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
            &intent.sell_token_account,
            order.amount_in,
        );
        let sell_mint = token::mint_of(svm, &intent.sell_token_account);
        let destination = token::create_token_account(svm, payer, &sell_mint, &unique_pubkey());
        let pulls: &[Pull] = Box::leak(Box::new([Pull {
            destination,
            amount: order.amount_in,
        }]));
        initialized.push(InitializedIntent { intent, pulls });

        // Buy side: fund the buffer so the push has something to draw from.
        let buy_mint = token::mint_of(svm, &intent.buy_token_account);
        buffer::ensure_funded(svm, program_id, payer, &buy_mint, order.amount_out);
        finalized.push(FinalizedIntent {
            intent,
            mint: buy_mint,
            amount: order.amount_out,
        });
    }

    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        token_programs: begin_programs,
        orders: &initialized,
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        token_programs: finalize_programs,
        orders: &finalized,
    };
    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), finalize.into()],
        Some(&payer.pubkey()),
        &[payer],
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
    let sell_mint = token::create_mint_under(svm, payer, sell_program);
    let buy_mint = token::create_mint_under(svm, payer, buy_program);
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
        token::program_of(svm, &intent.sell_token_account),
        *sell_program,
    );
    assert_eq!(
        token::program_of(svm, &intent.buy_token_account),
        *buy_program,
    );
    intent
}

/// The headline capability: one settlement pair moving tokens under both
/// programs, each transfer issued against the program that owns the account.
#[test]
fn settles_orders_under_both_token_programs() {
    let (mut svm, program_id, payer) = setup();

    let legacy = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &SPL_TOKEN_PROGRAM_ID,
        &SPL_TOKEN_PROGRAM_ID,
    );
    let token_2022 = order_across(
        &mut svm,
        &program_id,
        &payer,
        1,
        &TOKEN_2022_PROGRAM_ID,
        &TOKEN_2022_PROGRAM_ID,
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
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
        TokenPrograms::BOTH,
        TokenPrograms::BOTH,
    )
    .expect("a settlement carrying both programs should settle orders under either");

    assert_eq!(token::balance(&svm, &legacy.buy_token_account), 400);
    assert_eq!(token::balance(&svm, &token_2022.buy_token_account), 700);
    // Both sell sides were drained by their own program's transfer.
    assert_eq!(token::balance(&svm, &legacy.sell_token_account), 0);
    assert_eq!(token::balance(&svm, &token_2022.sell_token_account), 0);
}

/// The two sides of one order need not share a program: the pull follows the
/// sell account's owner and the push the buy account's, independently.
#[test]
fn settles_an_order_that_crosses_token_programs() {
    let (mut svm, program_id, payer) = setup();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &SPL_TOKEN_PROGRAM_ID,
        &TOKEN_2022_PROGRAM_ID,
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &[Settled {
            intent: &intent,
            amount_in: 250,
            amount_out: 250,
        }],
        TokenPrograms::BOTH,
        TokenPrograms::BOTH,
    )
    .expect("an order selling under one program and buying under the other should settle");

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 250);
    assert_eq!(token::balance(&svm, &intent.sell_token_account), 0);
}

/// A settlement that carries only Token-2022 still settles Token-2022 orders:
/// the legacy slot holding the placeholder costs it nothing it needs.
#[test]
fn settles_token_2022_orders_without_carrying_the_legacy_program() {
    let (mut svm, program_id, payer) = setup();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TOKEN_2022_PROGRAM_ID,
        &TOKEN_2022_PROGRAM_ID,
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &[Settled {
            intent: &intent,
            amount_in: 300,
            amount_out: 300,
        }],
        TokenPrograms::TOKEN_2022,
        TokenPrograms::TOKEN_2022,
    )
    .expect("a Token-2022-only settlement should settle Token-2022 orders");

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 300);
}

/// `BeginSettle` pulls from the sell account, so leaving that account's program
/// out is what it refuses — by name, rather than as a malformed account.
#[test]
fn rejects_a_sell_account_under_a_left_out_program() {
    let (mut svm, program_id, payer) = setup();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &TOKEN_2022_PROGRAM_ID,
        &SPL_TOKEN_PROGRAM_ID,
    );

    assert_settlement_error(
        BEGIN_INDEX,
        settle_with(
            &mut svm,
            &program_id,
            &payer,
            &[Settled {
                intent: &intent,
                amount_in: 100,
                amount_out: 100,
            }],
            TokenPrograms::SPL_TOKEN,
            TokenPrograms::SPL_TOKEN,
        ),
        SettlementError::TokenProgramNotProvided,
    );
}

/// `FinalizeSettle` pushes into the buy account, so it is the one that refuses
/// a settlement whose slots leave that account's program out. `BeginSettle`
/// runs first and passes: it only pulls, and this order's sell side is legacy.
#[test]
fn rejects_a_buy_account_under_a_left_out_program() {
    let (mut svm, program_id, payer) = setup();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &SPL_TOKEN_PROGRAM_ID,
        &TOKEN_2022_PROGRAM_ID,
    );

    assert_settlement_error(
        FINALIZE_INDEX,
        settle_with(
            &mut svm,
            &program_id,
            &payer,
            &[Settled {
                intent: &intent,
                amount_in: 100,
                amount_out: 100,
            }],
            TokenPrograms::BOTH,
            TokenPrograms::SPL_TOKEN,
        ),
        SettlementError::TokenProgramNotProvided,
    );
}

/// The slots are positional. Handing each one the other's program isn't a way
/// to carry both: each slot takes its own program or the placeholder, nothing
/// else.
#[test]
fn rejects_swapped_token_program_slots() {
    let (mut svm, program_id, payer) = setup();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &SPL_TOKEN_PROGRAM_ID,
        &SPL_TOKEN_PROGRAM_ID,
    );
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &intent.sell_token_account,
        100,
    );
    let sell_mint = token::mint_of(&svm, &intent.sell_token_account);
    let buy_mint = token::mint_of(&svm, &intent.buy_token_account);
    buffer::ensure_funded(&mut svm, &program_id, &payer, &buy_mint, 100);
    let destination = token::create_token_account(&mut svm, &payer, &sell_mint, &unique_pubkey());

    let pulls = [Pull {
        destination,
        amount: 100,
    }];
    let mut begin = Instruction::from(BeginSettle {
        program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        token_programs: TokenPrograms::BOTH,
        orders: &[InitializedIntent {
            intent: &intent,
            pulls: &pulls,
        }],
    });
    // `BeginSettle`'s accounts are `[sysvar, state, spl_token, token_2022, ...]`,
    // so exchanging the two slots leaves both programs present but each in the
    // other's position.
    begin.accounts.swap(2, 3);
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        token_programs: TokenPrograms::BOTH,
        orders: &[FinalizedIntent {
            intent: &intent,
            mint: buy_mint,
            amount: 100,
        }],
    };

    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize.into()],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let error = svm
        .send_transaction(tx)
        .expect_err("swapped slots should be rejected")
        .err;
    assert_eq!(
        error,
        TransactionError::InstructionError(BEGIN_INDEX, InstructionError::IncorrectProgramId),
    );
}

/// Every settlement in the rest of the suite leaves Token-2022's slot empty, so
/// the placeholder has to be accepted for a legacy-only settlement — and it is
/// only the accounts under the left-out program that become unsettleable.
#[test]
fn accepts_the_placeholder_for_a_legacy_only_settlement() {
    let (mut svm, program_id, payer) = setup();

    let intent = order_across(
        &mut svm,
        &program_id,
        &payer,
        0,
        &SPL_TOKEN_PROGRAM_ID,
        &SPL_TOKEN_PROGRAM_ID,
    );

    settle_with(
        &mut svm,
        &program_id,
        &payer,
        &[Settled {
            intent: &intent,
            amount_in: 500,
            amount_out: 500,
        }],
        TokenPrograms::SPL_TOKEN,
        TokenPrograms::SPL_TOKEN,
    )
    .expect("a legacy-only settlement should not have to carry Token-2022");

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 500);
}
