//! Integration tests for the settled-orders list carried by `BeginSettle`.
//!
//! Each settlement transaction here is the minimal `[BeginSettle, FinalizeSettle]`
//! pair (begin at [`BEGIN_INDEX`] pointing to finalize at [`FINALIZE_INDEX`], and
//! vice versa) so that the begin/finalize pairing always validates and execution
//! reaches the order-list checks, which is what these tests exercise.
//!
//! `BeginSettle` pairs one push with each order and checks that push pays the
//! order's buy token account, so even a settlement expected to be rejected during
//! order validation must pair with a finalize whose pushes match the orders in
//! both count and destination. [`settle`] and [`settle_raw`] attach such pushes
//! (with placeholder source, bump, and amount, so they never execute), while
//! [`settle_and_pay`] attaches fully real ones for settlements expected to
//! succeed. Tests rejected before the push checks (wrong token program or state
//! PDA) pair with an empty finalize ([`send_settlement`]).

use crate::common::{
    assert_instruction_error, assert_settlement_error, buffer, create_account,
    order::{create_order_pda, sample_intent, OrderBuilder},
    set_unix_timestamp, setup, token,
};
use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::spl_token::error::TokenError;
use settlement_client::instructions::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull,
};
use settlement_client::settlement_interface::{
    data::order::{EncodedOrderAccount, OrderAccount},
    instruction::settle::{
        BeginSettle as BeginSettleRaw, FinalizeSettle as FinalizeSettleRaw, INSTRUCTIONS_SYSVAR_ID,
        SPL_TOKEN_PROGRAM_ID,
    },
    pda::{order::find_order_pda, state::find_state_pda},
    Instruction, SettlementError, SettlementInstruction,
};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

/// The positions of the two instructions in every settlement transaction below,
/// named so each instruction's reference to its counterpart reads clearly.
const BEGIN_INDEX: u16 = 0;
const FINALIZE_INDEX: u16 = 1;

/// A list of empty transfer lists, one per order. Used for settling `n` orders
/// without pulling any funds.
fn no_pulls(n: usize) -> Vec<&'static [Pull]> {
    vec![&[]; n]
}

/// Send `[begin, finalize]` signed by `payer`, where `begin` is a pre-built
/// `BeginSettle` instruction and `finalize` settles no pushes. Use it only for
/// cases rejected before `BeginSettle`'s one-push-per-order count check (wrong
/// token program or state PDA); otherwise the empty finalize trips that check.
fn send_settlement(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    begin: impl Into<Instruction>,
) -> Result<TransactionMetadata, TransactionError> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX,
        orders: &[],
    };
    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), finalize.into()],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map_err(|e| e.err)
}

/// Send `[begin, finalize]` where `finalize` carries one push per `destination`
/// (enough to satisfy `BeginSettle`'s one-push-per-order pairing) and, with
/// each push targeting its order's buy token account, its push-destination
/// check too. The pushes' source, bump, and amount are placeholders: these
/// settlements are expected to be rejected during order validation, so the
/// finalize never runs.
fn send_settlement_with_placeholder_pushes(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    begin: impl Into<Instruction>,
    destinations: &[Pubkey],
) -> Result<TransactionMetadata, TransactionError> {
    let push_count = destinations.len();
    let placeholder_sources: Vec<Pubkey> = (0..push_count).map(|_| Pubkey::new_unique()).collect();
    let bumps = vec![0u8; push_count];
    let amounts = vec![0u64; push_count];
    let finalize = Instruction::from(FinalizeSettleRaw {
        program_id: *program_id,
        state_pda: find_state_pda(program_id).0,
        begin_ix_index: BEGIN_INDEX,
        source_buffers: &placeholder_sources,
        destinations,
        bumps: &bumps,
        amounts: &amounts,
    });
    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), finalize],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map_err(|e| e.err)
}

/// Settle `orders` in a minimal `[BeginSettle, FinalizeSettle]` transaction
/// (begin at [`BEGIN_INDEX`], finalize at [`FINALIZE_INDEX`]) signed by `payer`.
/// The finalize carries placeholder pushes matching the orders in count and
/// destination, so this clears the push checks and reaches `BeginSettle`'s order
/// validation: use it for cases expected to be rejected there.
fn settle(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[InitializedIntent],
) -> Result<TransactionMetadata, TransactionError> {
    let destinations: Vec<Pubkey> = orders
        .iter()
        .map(|order| order.intent.buy_token_account)
        .collect();
    send_settlement_with_placeholder_pushes(
        svm,
        program_id,
        payer,
        BeginSettle {
            program_id: *program_id,
            finalize_ix_index: FINALIZE_INDEX,
            orders,
        },
        &destinations,
    )
}

/// Settle `orders` and pay each one: the finalize pushes a zero amount from each
/// order's canonical buy-token buffer to its buy token account, lining up
/// one-to-one with the orders so `BeginSettle`'s push pass passes. The buffer for
/// each order's buy mint is created on demand. Use it for settlements expected to
/// succeed. (Real push amounts are exercised in `finalize_settle_pushes.rs`.)
fn settle_and_pay(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[InitializedIntent],
) -> Result<TransactionMetadata, TransactionError> {
    let settled: Vec<FinalizedIntent> = orders
        .iter()
        .map(|order| {
            let buy_mint = token::mint_of(svm, &order.intent.buy_token_account);
            buffer::ensure_buffer_exists(svm, program_id, payer, &buy_mint);
            FinalizedIntent {
                intent: order.intent,
                mint: buy_mint,
                amount: 0,
            }
        })
        .collect();

    let begin = Instruction::from(BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX,
        orders,
    });
    let finalize = Instruction::from(FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX,
        orders: &settled,
    });
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map_err(|e| e.err)
}

/// Settle orders described by raw, parallel `(order_pda, sell_token, buy_token,
/// bump)` lists, pulling nothing. Uses the canonical state PDA and SPL Token
/// program so execution reaches the order-validation checks; tests that need a
/// non-canonical state PDA or token program build the instruction directly. The
/// finalize carries placeholder pushes, one per order and aimed at that order's
/// `buy_token`, to clear the push count and destination checks; every caller
/// expects rejection during order validation. Callers rejected before the push
/// destination check (a non-canonical or undecodable order) may pass any
/// `buy_token`.
fn settle_raw(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    order_pdas: &[Pubkey],
    sell_token_accounts: &[Pubkey],
    buy_token_accounts: &[Pubkey],
    bumps: &[u8],
) -> Result<TransactionMetadata, TransactionError> {
    let begin = BeginSettleRaw {
        program_id: *program_id,
        state_pda: find_state_pda(program_id).0,
        finalize_ix_index: FINALIZE_INDEX,
        order_pdas,
        order_pda_bumps: bumps,
        sell_token_accounts,
        pulls: &no_pulls(bumps.len()),
    };
    send_settlement_with_placeholder_pushes(svm, program_id, payer, begin, buy_token_accounts)
}

#[test]
fn settles_a_single_order() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    )
    .expect("settlement should succeed");
}

#[test]
fn settles_multiple_orders() {
    let (mut svm, program_id, payer) = setup();

    let mut intents = Vec::new();
    for salt in 0..3u8 {
        intents.push(
            OrderBuilder::new(&mut svm, &program_id, &payer)
                .salt(salt)
                .build(),
        );
    }

    let orders: Vec<InitializedIntent> = intents
        .iter()
        .map(|intent| InitializedIntent { intent, pulls: &[] })
        .collect();
    settle_and_pay(&mut svm, &program_id, &payer, &orders)
        .expect("multi-order settlement should succeed");
}

#[test]
fn rejects_wrong_bump() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    assert_settlement_error(
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[order_pda],
            &[intent.sell_token_account],
            &[intent.buy_token_account],
            &[bump ^ 0x01],
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
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[fake_order],
            &[sell_token],
            &[Pubkey::new_unique()],
            &[255],
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
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[sell_token],
            &[sell_token],
            &[Pubkey::new_unique()],
            &[255],
        ),
        InstructionError::InvalidAccountData,
    );
}

#[test]
fn rejects_sell_token_account_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // Supply a different token account than the one the order's intent names.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    let wrong_sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    assert_settlement_error(
        settle_raw(
            &mut svm,
            &program_id,
            &payer,
            &[order_pda],
            &[wrong_sell_token],
            &[intent.buy_token_account],
            &[bump],
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
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[InitializedIntent {
                intent: &intent,
                pulls: &[],
            }],
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
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[InitializedIntent {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::SellTokenAccountInvalid,
    );
}

#[test]
fn rejects_duplicate_orders() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    assert_settlement_error(
        settle_and_pay(
            &mut svm,
            &program_id,
            &payer,
            &[
                InitializedIntent {
                    intent: &intent,
                    pulls: &[],
                },
                InitializedIntent {
                    intent: &intent,
                    pulls: &[],
                },
            ],
        ),
        SettlementError::OrdersNotStrictlyIncreasing,
    );
}

#[test]
fn rejects_orders_in_wrong_address_order() {
    let (mut svm, program_id, payer) = setup();

    let first = OrderBuilder::new(&mut svm, &program_id, &payer)
        .salt(0)
        .build();
    let second = OrderBuilder::new(&mut svm, &program_id, &payer)
        .salt(1)
        .build();

    let (first_pda, first_bump) = find_order_pda(&program_id, &first.uid());
    let (second_pda, second_bump) = find_order_pda(&program_id, &second.uid());

    // Lay out the two distinct orders strictly decreasing by PDA address, which
    // the program rejects. The interface builders would sort them, so build both
    // instructions by hand in the current wire format. Begin data is
    // `[discriminator, finalize_ix_index (BE), order_count, bump×n, transfer_count×n]`
    // (no transfers here) and begin accounts are `[instructions_sysvar, state_pda,
    // token_program, (order_pda, sell_token_account)...]`. The finalize's push
    // destinations are laid out in the same decreasing order, so the first order's
    // destination check passes and the second order trips the ordering check.
    let mut orders = [
        (
            first_pda,
            first.sell_token_account,
            first.buy_token_account,
            first_bump,
        ),
        (
            second_pda,
            second.sell_token_account,
            second.buy_token_account,
            second_bump,
        ),
    ];
    orders.sort_by_key(|&(pda, ..)| std::cmp::Reverse(pda));

    let mut data = vec![SettlementInstruction::BeginSettle.discriminator()];
    data.extend_from_slice(&FINALIZE_INDEX.to_be_bytes());
    data.push(orders.len() as u8);
    data.extend(orders.iter().map(|&(_, _, _, bump)| bump));
    // No transfers: one zero transfer-count byte per order.
    data.extend(orders.iter().map(|_| 0u8));

    let mut accounts = vec![
        AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false),
        AccountMeta::new_readonly(find_state_pda(&program_id).0, false),
        AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
    ];
    for (order_pda, sell_token_account, _, _) in orders {
        accounts.push(AccountMeta::new_readonly(order_pda, false));
        accounts.push(AccountMeta::new(sell_token_account, false));
    }
    let begin = Instruction {
        program_id,
        accounts,
        data,
    };

    // One zero-amount push per order, paying each order's buy token account,
    // aligned with begin's decreasing order. `BeginSettle` checks only the
    // destinations, so the sources are placeholders (and the finalize never runs,
    // as begin rejects the ordering first).
    let placeholder_source = Pubkey::new_unique();
    let finalize = Instruction::from(FinalizeSettleRaw {
        program_id,
        state_pda: find_state_pda(&program_id).0,
        begin_ix_index: BEGIN_INDEX,
        source_buffers: &[placeholder_source, placeholder_source],
        destinations: &[orders[0].2, orders[1].2],
        bumps: &[0, 0],
        amounts: &[0, 0],
    });

    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let result = svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err);
    assert_settlement_error(result, SettlementError::OrdersNotStrictlyIncreasing);
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
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[InitializedIntent {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::OrderCancelled,
    );
}

#[test]
fn rejects_expired_order() {
    let (mut svm, program_id, payer) = setup();

    let valid_to = 1_000_000;
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .valid_to(valid_to)
        .build();
    let after_expiration = i64::from(valid_to) + 1;
    set_unix_timestamp(&mut svm, after_expiration);

    assert_settlement_error(
        settle(
            &mut svm,
            &program_id,
            &payer,
            &[InitializedIntent {
                intent: &intent,
                pulls: &[],
            }],
        ),
        SettlementError::OrderExpired,
    );
}

#[test]
fn settles_order_at_exact_valid_to() {
    let (mut svm, program_id, payer) = setup();

    let valid_to = 1_000_000;
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .valid_to(valid_to)
        .build();
    set_unix_timestamp(&mut svm, i64::from(valid_to));

    settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    )
    .expect("an order is still settleable at exactly valid_to");
}

#[test]
fn pulls_funds_to_destination() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .build();
    let sell_token = intent.sell_token_account;
    let initial_amount = 42_000_000;
    token::fund_and_delegate(&mut svm, &program_id, &payer, &sell_token, initial_amount);
    let destination =
        token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());

    let amount = 2_000_000;
    settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[Pull {
                destination,
                amount,
            }],
        }],
    )
    .expect("a pull within the approved delegation should succeed");

    assert_eq!(token::balance(&svm, &destination), amount);
    assert_eq!(token::balance(&svm, &sell_token), initial_amount - amount);
    assert_eq!(
        token::delegated_amount(&svm, &sell_token),
        initial_amount - amount
    );
}

#[test]
fn pulls_to_multiple_destinations() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .build();
    let sell_token = intent.sell_token_account;
    let initial_amount: u64 = 1_000_000;
    token::fund_and_delegate(&mut svm, &program_id, &payer, &sell_token, initial_amount);
    let dest0 = token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());
    let dest1 = token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());

    let pulled0 = 300_000;
    let pulled1 = 100_000;
    settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[
                Pull {
                    destination: dest0,
                    amount: pulled0,
                },
                Pull {
                    destination: dest1,
                    amount: pulled1,
                },
            ],
        }],
    )
    .expect("multiple pulls from one order should succeed");

    assert_eq!(token::balance(&svm, &dest0), pulled0);
    assert_eq!(token::balance(&svm, &dest1), pulled1);
    assert_eq!(
        token::balance(&svm, &sell_token),
        initial_amount - pulled0 - pulled1
    );
    assert_eq!(
        token::delegated_amount(&svm, &sell_token),
        initial_amount - pulled0 - pulled1
    );
}

#[test]
fn pulls_from_multiple_orders() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);

    // Two distinct orders, each selling from its own token account.
    let first = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .salt(0)
        .build();
    let second = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .salt(1)
        .build();
    let initial_amount_first = 1_337_000;
    let initial_amount_second = 31_337_000;
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &first.sell_token_account,
        initial_amount_first,
    );
    token::fund_and_delegate(
        &mut svm,
        &program_id,
        &payer,
        &second.sell_token_account,
        initial_amount_second,
    );
    let dest_first =
        token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());
    let dest_second =
        token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());

    let pulled_first = 42_000;
    let pulled_second = 67_000;
    settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[
            InitializedIntent {
                intent: &first,
                pulls: &[Pull {
                    destination: dest_first,
                    amount: pulled_first,
                }],
            },
            InitializedIntent {
                intent: &second,
                pulls: &[Pull {
                    destination: dest_second,
                    amount: pulled_second,
                }],
            },
        ],
    )
    .expect("pulls from several orders should succeed");

    assert_eq!(token::balance(&svm, &dest_first), pulled_first);
    assert_eq!(token::balance(&svm, &dest_second), pulled_second);
    assert_eq!(
        token::balance(&svm, &first.sell_token_account),
        initial_amount_first - pulled_first
    );
    assert_eq!(
        token::balance(&svm, &second.sell_token_account),
        initial_amount_second - pulled_second
    );
}

#[test]
fn zero_pulls_moves_nothing() {
    let (mut svm, program_id, payer) = setup();
    // The intent specifies a sell mint. We want to see that, when no pull is
    // specified, this account isn't touched in the transaction.
    let sell_mint = token::create_mint(&mut svm, &payer);
    let buy_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .buy_mint(&buy_mint)
        .build();
    let sell_token = intent.sell_token_account;

    let initial_amount = 42_000_000;
    token::mint_to(&mut svm, &payer, &sell_mint, &sell_token, initial_amount);
    // The buy-side buffer must exist for the (zero-amount) push to draw from.
    buffer::ensure_buffer_exists(&mut svm, &program_id, &payer, &buy_mint);

    // Build the `[begin, finalize]` settlement by hand so the issued token
    // instructions can be inspected. Begin settles the order with no pulls;
    // finalize pushes a zero amount from the buy buffer to the buy token account.
    let begin = Instruction::from(BeginSettle {
        program_id,
        finalize_ix_index: FINALIZE_INDEX,
        orders: &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    });
    let finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX,
        orders: &[FinalizedIntent {
            intent: &intent,
            mint: buy_mint,
            amount: 0,
        }],
    });
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let account_keys = tx.message.account_keys.clone();
    let transaction = svm
        .send_transaction(tx)
        .expect("settling without pulling should succeed");

    // No token instruction references the sell token account (the sell mint's
    // only account here): the lone token transfer is the buy-side push, which
    // draws from `buy_mint`'s buffer. Its balance is also left untouched.
    token::assert_no_token_instruction_touching(&transaction, &account_keys, &sell_token);
    assert_eq!(token::balance(&svm, &sell_token), initial_amount);
}

#[test]
fn rejects_wrong_state_pda() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());
    let not_the_state_pda = Pubkey::new_unique();

    assert_settlement_error(
        send_settlement(
            &mut svm,
            &program_id,
            &payer,
            BeginSettleRaw {
                program_id,
                state_pda: not_the_state_pda,
                finalize_ix_index: FINALIZE_INDEX,
                order_pdas: &[order_pda],
                order_pda_bumps: &[bump],
                sell_token_accounts: &[intent.sell_token_account],
                pulls: &no_pulls(1),
            },
        ),
        SettlementError::StateAccountMismatch,
    );
}

#[test]
fn rejects_wrong_token_program() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();

    // The builder always fills in the SPL Token program, so we swap the
    // token-program account out afterwards.
    let mut begin: Instruction = BeginSettle {
        program_id,
        finalize_ix_index: FINALIZE_INDEX,
        orders: &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    }
    .into();
    let token_account_index = 2;
    begin.accounts[token_account_index] = AccountMeta::new_readonly(Pubkey::new_unique(), false);

    assert_instruction_error(
        send_settlement(&mut svm, &program_id, &payer, begin),
        InstructionError::IncorrectProgramId,
    );
}

#[test]
fn rejects_pull_delegated_to_incorrect_address() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .build();
    let amount = 100_000;
    let sell_token = intent.sell_token_account;
    // Funds are present but some account other than the state PDA was
    // approved as a delegate.
    token::mint_to(&mut svm, &payer, &sell_mint, &sell_token, 1_000_000);
    token::delegate(&mut svm, &payer, &sell_token, &Pubkey::new_unique(), amount);
    let destination =
        token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());

    let result = settle(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[Pull {
                destination,
                amount,
            }],
        }],
    );
    assert_instruction_error(
        result,
        InstructionError::Custom(TokenError::OwnerMismatch as u32),
    );
}

#[test]
fn rejects_pull_exceeding_delegation() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .sell_mint(&sell_mint)
        .build();
    let sell_token = intent.sell_token_account;
    // Funded generously, but the state PDA is delegated only 100_000.
    let initial_amount = 42_000_000;
    let delegated = 100_000;
    token::mint_to(&mut svm, &payer, &sell_mint, &sell_token, initial_amount);
    token::delegate(
        &mut svm,
        &payer,
        &sell_token,
        &find_state_pda(&program_id).0,
        delegated,
    );
    let destination =
        token::create_token_account(&mut svm, &payer, &sell_mint, &Pubkey::new_unique());

    let result = settle(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[Pull {
                destination,
                amount: 200_000,
            }],
        }],
    );
    assert_instruction_error(
        result,
        InstructionError::Custom(TokenError::InsufficientFunds as u32),
    );
    assert_eq!(token::balance(&svm, &sell_token), initial_amount);
    assert_eq!(token::balance(&svm, &destination), 0);
    // The rejected pull must not have consumed any of the delegation.
    assert_eq!(token::delegated_amount(&svm, &sell_token), delegated);
}

#[test]
fn rejects_extra_account() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    // A well-formed single-order, no-transfer settlement...
    let mut begin: Instruction = BeginSettle {
        program_id,
        finalize_ix_index: FINALIZE_INDEX,
        orders: &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    }
    .into();
    // ...with one extra account appended, so the account count no longer matches
    // the `2n + T` the instruction data implies.
    begin
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));

    assert_settlement_error(
        send_settlement(&mut svm, &program_id, &payer, begin),
        SettlementError::AccountCountNotMatchingOrderCount,
    );
}
