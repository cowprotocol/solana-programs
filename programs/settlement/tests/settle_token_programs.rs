//! Integration tests to verify the behavior of multiple token programs
//! within a single settlement.

use crate::common::{
    buffer,
    order::OrderBuilder,
    settlement::{build_staged_settlement, StagedOrder, BEGIN_INDEX, FINALIZE_INDEX},
    setup_settle_ready, token,
    token_2022::Extensions,
    unique_keypair, unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{
    data::intent::OrderIntent, pda::order::find_order_pda, Instruction,
};
use cow_settlement_client::instruction::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull, TokenProgram,
};
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::instruction::create_account;
use spl_token_2022_interface::instruction::{burn, close_account, initialize_mint2, mint_to};

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
        token::program_of(svm, &intent.sell_token_account),
        *sell_program,
    );
    assert_eq!(
        token::program_of(svm, &intent.buy_token_account),
        *buy_program,
    );
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

    assert_eq!(token::balance(&svm, &legacy.buy_token_account), 400);
    assert_eq!(token::balance(&svm, &token_2022.buy_token_account), 700);
    // Both sell sides were drained by their own program's transfer.
    assert_eq!(token::balance(&svm, &legacy.sell_token_account), 0);
    assert_eq!(token::balance(&svm, &token_2022.sell_token_account), 0);
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

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 250);
    assert_eq!(token::balance(&svm, &intent.sell_token_account), 0);
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

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 300);
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

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 500);
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

    assert_eq!(token::balance(&svm, &intent.buy_token_account), 100);
}

#[test]
fn narrowing_begin_settle_drops_one_account_from_the_transaction() {
    /// What the order settles for. Any amount does; it just has to be the same
    /// in both transactions, so the two differ only in what they name.
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
        &intent.sell_token_account,
        AMOUNT,
    );
    let sell_mint = token::mint_of(&svm, &intent.sell_token_account);
    let buy_mint = token::mint_of(&svm, &intent.buy_token_account);
    buffer::ensure_funded(&mut svm, &program_id, &payer, &buy_mint, AMOUNT);
    let destination = token::create_token_account(&mut svm, &payer, &sell_mint, &unique_pubkey());
    let pulls = [Pull {
        destination,
        amount: AMOUNT,
    }];
    let blockhash = svm.latest_blockhash();

    let verify_narrowed = |only_token_program| {
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

    let both = verify_narrowed(None);
    let narrowed_legacy = verify_narrowed(Some(TokenProgram::SplToken));
    let narrowed_2022 = verify_narrowed(Some(TokenProgram::Token2022));

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
    assert_eq!(token::balance(&svm, &intent.buy_token_account), AMOUNT);
}

/// Two orders netting one token against each other, as staged by
/// [`orders_netting_a_shared_token`].
struct SharedToken {
    /// Both orders, strictly increasing by order PDA, as `BeginSettle` takes them.
    orders: Vec<StagedOrder>,
    /// The token account the shared token is pushed into.
    buyer: Pubkey,
    /// The shared token's canonical buffer, which the push draws from. Created
    /// and empty: the settlement is what was to have filled it.
    buffer: Pubkey,
    /// The solver's own account, which the pull of the shared token lands in.
    pulled_into: Pubkey,
}

/// Stage two orders that net one token against each other: `shared_mint` is
/// what the first one sells and what the second one buys, `amount` of it each
/// way, with each order's other side a mint of its own. The shared token's only
/// liquidity is what the settlement pulls in, and it is pulled into an account
/// the solver owns — nothing pins a pull to the buffer that pays the other
/// order, only the push is tied to it.
fn orders_netting_a_shared_token(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    solver: &Pubkey,
    shared_mint: &Pubkey,
    amount: u64,
) -> SharedToken {
    let token_2022 = TokenProgram::Token2022.address();
    let bought_by_the_seller = token::create_mint_under(svm, payer, &token_2022, Extensions::None);
    let sold_by_the_buyer = token::create_mint_under(svm, payer, &token_2022, Extensions::None);

    let sells_shared = OrderBuilder::new(svm, program_id, payer)
        .salt(0)
        .sell_mint(shared_mint)
        .buy_mint(&bought_by_the_seller)
        .sell_amount(1_000)
        .buy_amount(1_000)
        .build();
    let buys_shared = OrderBuilder::new(svm, program_id, payer)
        .salt(1)
        .sell_mint(&sold_by_the_buyer)
        .buy_mint(shared_mint)
        .sell_amount(1_000)
        .buy_amount(1_000)
        .build();

    token::fund_and_delegate(
        svm,
        program_id,
        payer,
        &sells_shared.sell_token_account,
        amount,
    );
    token::fund_and_delegate(
        svm,
        program_id,
        payer,
        &buys_shared.sell_token_account,
        amount,
    );
    buffer::ensure_funded(svm, program_id, payer, &bought_by_the_seller, amount);

    let buyer = buys_shared.buy_token_account;
    let pulled_into = token::create_token_account(svm, payer, shared_mint, solver);
    let mut orders = vec![
        StagedOrder {
            intent: sells_shared,
            pulls: vec![Pull {
                destination: pulled_into,
                amount,
            }],
            amount_out: amount,
        },
        StagedOrder {
            intent: buys_shared,
            pulls: vec![Pull {
                destination: token::create_token_account(
                    svm,
                    payer,
                    &sold_by_the_buyer,
                    &unique_pubkey(),
                ),
                amount,
            }],
            amount_out: amount,
        },
    ];
    orders.sort_by_key(|order| find_order_pda(program_id, &order.intent.uid()).0);

    SharedToken {
        orders,
        buyer,
        buffer: buffer::ensure_buffer_exists(svm, program_id, payer, shared_mint),
        pulled_into,
    }
}

/// The most a recreated mint can be turned against a settlement: two orders net
/// one Token-2022 token against each other — one sells it, the other buys it —
/// and between the settlement's two halves the solver destroys that token and
/// puts a token of its own at the address, under the same program.
///
/// Nothing obliges it to pull the netted token into the buffer that pays the
/// other order: the pull's destination is its own to choose, so it takes
/// delivery itself and burns what it was paid, which is what puts the close in
/// reach (a mint only closes once its supply is gone). Then it reopens the
/// address as a Token-2022 mint it is the authority of, and prints itself
/// exactly what the buyer is owed — straight into the buffer the old token left
/// behind, because a token account outlives its mint and a Token-2022 mint is
/// what the buffer will still take.
///
/// And the push pays it out. Every part of the settlement identifies a token by
/// its address: the buffer PDA is derived from it, the intent names it, and the
/// token accounts record it — none of which the swap disturbs. Both sides are
/// under one program, so nothing is left to tell the two tokens apart, and the
/// buyer is paid in a token it never agreed to buy.
///
/// What this is *not* is a way past the program boundary: recreating the mint
/// under the legacy program instead leaves the buffer allocated to a program
/// that Token-2022 refuses to read, because `FinalizeSettle` takes the program
/// from the account being paid, which is still Token-2022's. The exposure is a
/// mint that carries a close authority the solver holds, over a token whose
/// entire supply it can burn.
#[test]
fn pays_a_buyer_in_a_token_recreated_at_the_same_address_mid_settlement() {
    /// What the two orders net against each other, and so what the substitute
    /// has to cover.
    const AMOUNT: u64 = 400;
    /// The replacement is a token of the solver's own that happens to inherit an
    /// address, so nothing about it is built to resemble what it replaces. Its
    /// decimals are what the assertions read the substitution off.
    const REOPENED_DECIMALS: u8 = 0;

    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let token_2022 = TokenProgram::Token2022.address();
    let mint_keypair = unique_keypair();
    let shared_mint = token::create_mint_at_under(
        &mut svm,
        &payer,
        &mint_keypair,
        &token_2022,
        Extensions::CloseAuthorityOnly,
    );
    let original_decimals = token::decimals_of(&svm, &shared_mint);
    let shared = orders_netting_a_shared_token(
        &mut svm,
        &program_id,
        &payer,
        &solver.pubkey(),
        &shared_mint,
        AMOUNT,
    );

    let substitution = vec![
        // Empty the solver's own holding, so the mint can be closed at all.
        burn(
            &token_2022,
            &shared.pulled_into,
            &shared_mint,
            &solver.pubkey(),
            &[],
            AMOUNT,
        )
        .expect("burn should build"),
        close_account(
            &token_2022,
            &shared_mint,
            &payer.pubkey(),
            &payer.pubkey(),
            &[],
        )
        .expect("close_account should build"),
        // Reopen the address as a mint the solver's side is the authority of,
        // under the program the old one lived under.
        create_account(
            &payer.pubkey(),
            &shared_mint,
            svm.minimum_balance_for_rent_exemption(Extensions::None.mint_len()),
            Extensions::None.mint_len() as u64,
            &token_2022,
        ),
        initialize_mint2(
            &token_2022,
            &shared_mint,
            &payer.pubkey(),
            None,
            REOPENED_DECIMALS,
        )
        .expect("initialize_mint2 should build"),
        // And print exactly what the buyer is owed, into the buffer the token it
        // ordered left behind.
        mint_to(
            &token_2022,
            &shared_mint,
            &shared.buffer,
            &payer.pubkey(),
            &[],
            AMOUNT,
        )
        .expect("mint_to should build"),
    ];

    let instructions =
        build_staged_settlement(&program_id, &solver.pubkey(), &shared.orders, substitution);
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer.pubkey()),
        &[&payer, &solver, &mint_keypair],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("a token swapped in under the same program settles like the one it replaced");

    // The buyer was paid in full, out of a buffer that was empty until the
    // substitute was printed into it.
    assert_eq!(token::balance(&svm, &shared.buyer), AMOUNT);
    assert_eq!(token::balance(&svm, &shared.buffer), 0);

    // And what it was paid in is not what it bought: same address, same program,
    // different token.
    assert_eq!(token::mint_of(&svm, &shared.buyer), shared_mint);
    assert_eq!(token::program_of(&svm, &shared_mint), token_2022);
    assert_ne!(original_decimals, REOPENED_DECIMALS);
    assert_eq!(token::decimals_of(&svm, &shared_mint), REOPENED_DECIMALS);
}
