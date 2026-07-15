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
//! both count and destination. [`settle_and_pay`] builds exactly such a
//! fully-working settlement, so every test here builds one with it and either
//! sends it unmodified (when the rejection is already baked into the orders or
//! accounts passed in) or mutates its `BeginSettle` instruction in place
//! afterwards (a wrong account, a wrong token program, a wrong state PDA, an
//! extra account). [`rejects_wrong_bump`] is the exception: a wrong bump lives in
//! `BeginSettle`'s instruction data rather than its accounts, so it builds the
//! raw instruction directly instead of mutating a `settle_and_pay` result.

use crate::common::{
    assert_instruction_error, assert_settlement_error, buffer, create_account,
    order::{create_order_pda, sample_intent, OrderBuilder},
    send, set_unix_timestamp,
    settlement::{BEGIN_INDEX, FINALIZE_INDEX},
    setup, token,
};
use litesvm::LiteSVM;
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
    pda::{buffer::find_buffer_pda, order::find_order_pda, state::find_state_pda},
    Instruction, SettlementError, SettlementInstruction,
};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, InstructionError},
    message::Message,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

mod common;

/// A list of empty transfer lists, one per order. Used for settling `n` orders
/// without pulling any funds.
fn no_pulls(n: usize) -> Vec<&'static [Pull]> {
    vec![&[]; n]
}

/// Build the `[BeginSettle, FinalizeSettle]` instructions settling `orders` and
/// paying each one: the finalize pushes a zero amount from each order's canonical
/// buy-token buffer to its buy token account, lining up one-to-one with the
/// orders so `BeginSettle`'s push pass passes. The buffer for each order's buy
/// mint is created on demand (hence `&mut svm`, unlike the other builders). Use
/// it for settlements expected to succeed. (Real push amounts are exercised in
/// `finalize_settle_pushes.rs`.)
fn settle_and_pay(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[InitializedIntent],
) -> Vec<Instruction> {
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

    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders,
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &settled,
    };
    vec![begin.into(), finalize.into()]
}

#[test]
fn settles_a_single_order() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    send(&mut svm, &payer, instructions).expect("settlement should succeed");
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
    let instructions = settle_and_pay(&mut svm, &program_id, &payer, &orders);
    send(&mut svm, &payer, instructions).expect("multi-order settlement should succeed");
}

#[test]
fn rejects_wrong_bump() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let (order_pda, bump) = find_order_pda(&program_id, &intent.uid());

    // The wrong bump lives in `BeginSettle`'s instruction data, which the client
    // builder always derives correctly, so build the raw instruction directly to
    // inject a bad one. The finalize carries a placeholder push matching the
    // order in count and destination, clearing `BeginSettle`'s push checks
    // without ever executing (begin rejects the bump first).
    let begin = BeginSettleRaw {
        program_id,
        state_pda: find_state_pda(&program_id).0,
        finalize_ix_index: FINALIZE_INDEX.into(),
        order_pdas: &[order_pda],
        order_pda_bumps: &[bump ^ 0x01],
        sell_token_accounts: &[intent.sell_token_account],
        pulls: &no_pulls(1),
    };
    let finalize = FinalizeSettleRaw {
        program_id,
        state_pda: find_state_pda(&program_id).0,
        begin_ix_index: BEGIN_INDEX.into(),
        source_buffers: &[Pubkey::new_unique()],
        destinations: &[intent.buy_token_account],
        bumps: &[0],
        amounts: &[0],
    };
    let instructions = vec![begin.into(), finalize.into()];
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
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

    // Build a normal settlement for some real order, then swap its order_pda
    // and sell_token accounts for the fabricated ones: `BeginSettle` rejects the
    // fabricated order's address before the pushes ever run, so the finalize
    // just needs to look like a legitimate settlement.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let (real_order_pda, _bump) = find_order_pda(&program_id, &intent.uid());
    let mut instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    for meta in instructions[usize::from(BEGIN_INDEX)].accounts.iter_mut() {
        if meta.pubkey == real_order_pda {
            meta.pubkey = fake_order;
        } else if meta.pubkey == intent.sell_token_account {
            meta.pubkey = sell_token;
        }
    }

    assert_settlement_error(
        send(&mut svm, &payer, instructions),
        SettlementError::OrderNotCanonical,
    );
}

#[test]
fn rejects_non_order_account_in_order_slot() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // Put a token account in both the order and sell-token slots of a normal
    // settlement for some real order. Its 165-byte data can't decode as a
    // 199-byte order body, so it's rejected before the canonical-address check,
    // and the finalize just needs to look like a legitimate settlement.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let (real_order_pda, _bump) = find_order_pda(&program_id, &intent.uid());
    let mut instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    for meta in instructions[usize::from(BEGIN_INDEX)].accounts.iter_mut() {
        if meta.pubkey == real_order_pda || meta.pubkey == intent.sell_token_account {
            meta.pubkey = sell_token;
        }
    }

    assert_instruction_error(
        send(&mut svm, &payer, instructions),
        InstructionError::InvalidAccountData,
    );
}

#[test]
fn rejects_sell_token_account_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    // Supply a different token account than the one the order's intent names.
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let wrong_sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let mut instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    let meta = instructions[usize::from(BEGIN_INDEX)]
        .accounts
        .iter_mut()
        .find(|meta| meta.pubkey == intent.sell_token_account)
        .expect("BeginSettle should reference the order's sell token account");
    meta.pubkey = wrong_sell_token;

    assert_settlement_error(
        send(&mut svm, &payer, instructions),
        SettlementError::SellTokenAccountMismatch,
    );
}

#[test]
fn rejects_sell_token_owner_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);

    let other_owner = Pubkey::new_unique();
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &other_owner);
    let mut intent = sample_intent(payer.pubkey(), sell_token, 1);
    let buy_mint = token::create_mint(&mut svm, &payer);
    intent.buy_token_account =
        token::create_token_account(&mut svm, &payer, &buy_mint, &payer.pubkey());
    create_order_pda(&mut svm, &program_id, &payer, &intent);

    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
        SettlementError::SellTokenOwnerMismatch,
    );
}

#[test]
fn rejects_non_token_sell_account() {
    let (mut svm, program_id, payer) = setup();

    let non_token = Pubkey::new_unique();
    let mut intent = sample_intent(payer.pubkey(), non_token, 1);
    let buy_mint = token::create_mint(&mut svm, &payer);
    intent.buy_token_account =
        token::create_token_account(&mut svm, &payer, &buy_mint, &payer.pubkey());
    create_order_pda(&mut svm, &program_id, &payer, &intent);

    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
        SettlementError::SellTokenAccountInvalid,
    );
}

#[test]
fn rejects_duplicate_orders() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let instructions = settle_and_pay(
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
    );
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
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
    data.extend_from_slice(&u16::from(FINALIZE_INDEX).to_be_bytes());
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

    // One real zero-amount push per order, paying each order's buy token
    // account, aligned with begin's decreasing order. `BeginSettle` rejects the
    // ordering before the pushes ever run, so the finalize only needs to look
    // like a legitimate settlement of these two orders.
    let mut source_buffers = Vec::with_capacity(orders.len());
    let mut push_bumps = Vec::with_capacity(orders.len());
    let destinations: Vec<Pubkey> = orders.iter().map(|&(_, _, buy, _)| buy).collect();
    for &destination in &destinations {
        let mint = token::mint_of(&svm, &destination);
        buffer::ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);
        let (source_buffer, bump) = find_buffer_pda(&program_id, &mint);
        source_buffers.push(source_buffer);
        push_bumps.push(bump);
    }
    let amounts = vec![0u64; orders.len()];
    let finalize = Instruction::from(FinalizeSettleRaw {
        program_id,
        state_pda: find_state_pda(&program_id).0,
        begin_ix_index: BEGIN_INDEX.into(),
        source_buffers: &source_buffers,
        destinations: &destinations,
        bumps: &push_bumps,
        amounts: &amounts,
    });
    let instructions = vec![begin, finalize];
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
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
    let mut intent = sample_intent(payer.pubkey(), sell_token, 0);
    let buy_mint = token::create_mint(&mut svm, &payer);
    intent.buy_token_account =
        token::create_token_account(&mut svm, &payer, &buy_mint, &payer.pubkey());
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

    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
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

    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    assert_settlement_error(
        send(&mut svm, &payer, instructions),
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

    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    send(&mut svm, &payer, instructions).expect("an order is still settleable at exactly valid_to");
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
    let instructions = settle_and_pay(
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
    send(&mut svm, &payer, instructions)
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
    let instructions = settle_and_pay(
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
    );
    send(&mut svm, &payer, instructions).expect("multiple pulls from one order should succeed");

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
    let instructions = settle_and_pay(
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
    );
    send(&mut svm, &payer, instructions).expect("pulls from several orders should succeed");

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

    // `settle_and_pay` builds exactly the settlement this test needs: begin
    // settles the order with no pulls, and finalize pushes a zero amount from the
    // buy buffer (created on demand) to the buy token account. Compile the
    // account keys the same way `send` will, so we can inspect the issued token
    // instructions against them afterwards.
    let instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );
    let account_keys = Message::new(&instructions, Some(&payer.pubkey())).account_keys;
    let transaction =
        send(&mut svm, &payer, instructions).expect("settling without pulling should succeed");

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
    let mut instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );

    // Swap the state PDA account `BeginSettle` references for a bogus one.
    let (state_pda, _bump) = find_state_pda(&program_id);
    let meta = instructions[usize::from(BEGIN_INDEX)]
        .accounts
        .iter_mut()
        .find(|meta| meta.pubkey == state_pda)
        .expect("BeginSettle should reference the state PDA");
    meta.pubkey = Pubkey::new_unique();

    assert_settlement_error(
        send(&mut svm, &payer, instructions),
        SettlementError::StateAccountMismatch,
    );
}

#[test]
fn rejects_wrong_token_program() {
    let (mut svm, program_id, payer) = setup();

    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let mut instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );

    // Swap the SPL Token program account `BeginSettle` references for a bogus
    // one.
    let meta = instructions[usize::from(BEGIN_INDEX)]
        .accounts
        .iter_mut()
        .find(|meta| meta.pubkey == SPL_TOKEN_PROGRAM_ID)
        .expect("BeginSettle should reference the SPL Token program");
    meta.pubkey = Pubkey::new_unique();

    assert_instruction_error(
        send(&mut svm, &payer, instructions),
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

    let instructions = settle_and_pay(
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
        send(&mut svm, &payer, instructions),
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

    let instructions = settle_and_pay(
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
        send(&mut svm, &payer, instructions),
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
    let mut instructions = settle_and_pay(
        &mut svm,
        &program_id,
        &payer,
        &[InitializedIntent {
            intent: &intent,
            pulls: &[],
        }],
    );

    // Append one extra account to `BeginSettle`, so the account count no longer
    // matches the `2n + T` the instruction data implies.
    instructions[usize::from(BEGIN_INDEX)]
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));

    assert_settlement_error(
        send(&mut svm, &payer, instructions),
        SettlementError::AccountCountNotMatchingOrderCount,
    );
}
