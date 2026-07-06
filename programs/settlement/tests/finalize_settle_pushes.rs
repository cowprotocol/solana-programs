//! Integration tests for the fund pushes carried by `FinalizeSettle` and
//! validated by `BeginSettle`.
//!
//! Each settlement transaction is a `[BeginSettle, FinalizeSettle]` pair (begin
//! at [`BEGIN_INDEX`] pointing to finalize at [`FINALIZE_INDEX`], and vice
//! versa). `BeginSettle` settles the orders the finalize pays (created on-chain
//! via `OrderBuilder` with no pulls, so only the push side moves funds) and
//! validates that each order is paid by exactly one push to its buy token
//! account. `FinalizeSettle` then executes the transfers out of the buffers,
//! signed by the settlement state PDA that owns them.

use crate::common::{
    buffer, create_account,
    order::{create_order_pda, sample_intent, OrderBuilder},
    setup, to_instruction_error, token,
};
use litesvm::LiteSVM;
use settlement_client::instructions::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent,
};
use settlement_client::settlement_interface::{Instruction, SettlementError};
use solana_sdk::{
    instruction::InstructionError,
    program_error::ProgramError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

/// The following [`send_settlement`] function simulates a settlement and for
/// that hardcodes some instruction indices that will be referenced in the
/// tests. We make those indices more explicit with a constant.
const BEGIN_INDEX: u8 = 0;
const FINALIZE_INDEX: u8 = 1;

/// Assert the transaction failed in `BeginSettle` (at [`BEGIN_INDEX`]) with
/// `expected`.
fn assert_begin_error(result: Result<(), TransactionError>, expected: SettlementError) {
    assert_eq!(
        result,
        Err(TransactionError::InstructionError(
            BEGIN_INDEX,
            to_instruction_error(expected),
        )),
    );
}

/// Assert the transaction failed in `FinalizeSettle` (at [`FINALIZE_INDEX`])
/// with `expected`.
fn assert_finalize_error(result: Result<(), TransactionError>, expected: InstructionError) {
    assert_eq!(
        result,
        Err(TransactionError::InstructionError(FINALIZE_INDEX, expected)),
    );
}

/// Send `[begin, finalize]` signed by `payer`, where `finalize` is a pre-built
/// `FinalizeSettle` at [`FINALIZE_INDEX`] and `begin` settles `orders` (with no
/// pulls) at [`BEGIN_INDEX`], the same orders the finalize is expected to push
/// to.
fn send_settlement(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[FinalizedIntent],
    finalize: impl Into<Instruction>,
) -> Result<(), TransactionError> {
    let begin_orders: Vec<InitializedIntent> = orders
        .iter()
        .map(|order| InitializedIntent {
            intent: order.intent,
            pulls: &[],
        })
        .collect();
    let begin = Instruction::from(BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders: &begin_orders,
    });
    // Assemble the transaction, confirming each instruction lands at its named
    // index to make sure the constants are meaningfully defined.
    let mut instructions = Vec::new();
    assert_eq!(instructions.len(), usize::from(BEGIN_INDEX));
    instructions.push(begin);
    assert_eq!(instructions.len(), usize::from(FINALIZE_INDEX));
    instructions.push(finalize.into());
    let tx = Transaction::new_signed_with_payer(
        &instructions,
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
    orders: &[FinalizedIntent],
) -> Result<(), TransactionError> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders,
    };
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
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint)
        .build();
    let funding = 1_000;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, funding);

    let amount = 400;
    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[FinalizedIntent {
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
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint)
        .salt(0)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint)
        .salt(1)
        .build();
    let funding = 10_000;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, funding);

    let amount0 = 1_000;
    let amount1 = 2_000;
    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[
            FinalizedIntent {
                intent: &intent0,
                mint,
                amount: amount0,
            },
            FinalizedIntent {
                intent: &intent1,
                mint,
                amount: amount1,
            },
        ],
    )
    .expect("several pushes from one buffer should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy_token_account), amount0);
    assert_eq!(token::balance(&svm, &intent1.buy_token_account), amount1);
    assert_eq!(
        token::balance(&svm, &buffer_pda),
        funding - amount0 - amount1,
    );
}

#[test]
fn pushes_several_orders_from_different_buffers() {
    let (mut svm, program_id, payer) = setup();
    let mint0 = token::create_mint(&mut svm, &payer);
    let mint1 = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint0)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint1)
        .build();
    let funding = 5_000;
    let buffer0 = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint0, funding);
    let buffer1 = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint1, funding);

    let amount0 = 1_000;
    let amount1 = 2_000;
    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[
            FinalizedIntent {
                intent: &intent0,
                mint: mint0,
                amount: amount0,
            },
            FinalizedIntent {
                intent: &intent1,
                mint: mint1,
                amount: amount1,
            },
        ],
    )
    .expect("pushes from different buffers should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy_token_account), amount0);
    assert_eq!(token::balance(&svm, &intent1.buy_token_account), amount1);
    assert_eq!(token::balance(&svm, &buffer0), funding - amount0);
    assert_eq!(token::balance(&svm, &buffer1), funding - amount1);
}

#[test]
fn rejects_push_to_wrong_destination() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 100,
    }];

    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
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
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&buy_mint)
        .build();
    // The push draws from the buffer for `other_mint`, not the buy token's mint,
    // which `FinalizeSettle` rejects when it reads the destination's mint.
    let orders = [FinalizedIntent {
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
fn rejects_push_from_substituted_source() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint)
        .build();
    buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, 1_000);
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 100,
    }];

    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
    // Point the push at an account that isn't the canonical buffer, leaving the
    // rest well-formed. Accounts: `[sysvar, state, token_program, source,
    // destination]`. `BeginSettle` doesn't validate the source, so it passes;
    // `FinalizeSettle` re-derives the buffer from the destination's mint and
    // rejects the mismatch before touching the substituted account.
    let source_index = 3;
    finalize.accounts[source_index].pubkey = Pubkey::new_unique();

    assert_finalize_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        to_instruction_error(SettlementError::PushSourceNotBuffer),
    );
}

#[test]
fn rejects_fewer_pushes_than_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 100,
    }];

    // A finalize carrying no pushes, paired with a begin settling one order.
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &[],
    };

    assert_begin_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        SettlementError::SettledOrderPushCountMismatch,
    );
}

#[test]
fn rejects_more_pushes_than_orders() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();

    // A finalize that pushes to one order, paired with a begin that settles none,
    // so the extra push has no order to account for it.
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &[FinalizedIntent {
            intent: &intent,
            mint,
            amount: 0,
        }],
    };

    assert_begin_error(
        send_settlement(&mut svm, &program_id, &payer, &[], finalize),
        SettlementError::SettledOrderPushCountMismatch,
    );
}

#[test]
fn rejects_push_account_count_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 100,
    }];

    // A well-formed single-push finalize (five accounts, a nine-byte push body)...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
    // ...with another push's worth of data bytes appended but no matching
    // accounts. `BeginSettle` derives the push count from the (unchanged) account
    // metas (one push, matching its one order and paying the right destination)
    // so it passes. Only the finalize reads the data, where it now parses two
    // pushes against two push accounts and rejects the mismatch. This is the
    // account/data disagreement `BeginSettle` structurally can't see.
    finalize.data.extend_from_slice(&[0u8; 9]);

    assert_finalize_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        to_instruction_error(SettlementError::AccountCountNotMatchingPushCount),
    );
}

#[test]
fn rejects_too_few_accounts() {
    let (mut svm, program_id, payer) = setup();

    // A well-formed no-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &[],
    });
    // ...with one of its three fixed accounts popped. `BeginSettle` runs first
    // but only reads push destinations off the accounts (finding none, matching
    // its zero orders) so it passes. The finalize then can't even destructure
    // its fixed accounts and raises `NotEnoughAccountKeys`.
    finalize.accounts.pop();

    let err = send_settlement(&mut svm, &program_id, &payer, &[], finalize)
        .expect_err("a finalize missing a fixed account must be rejected");
    let TransactionError::InstructionError(FINALIZE_INDEX, ix_err) = err else {
        panic!("expected the finalize (index {FINALIZE_INDEX}) to fail, got {err:?}");
    };
    // Compare against the non-deprecated `ProgramError` variant the program
    // returns; naming the `InstructionError` variant directly would touch a
    // deprecated alias.
    assert_eq!(
        ProgramError::try_from(ix_err),
        Ok(ProgramError::NotEnoughAccountKeys),
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
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 0,
    }];

    assert_finalize_error(
        finalize(&mut svm, &program_id, &payer, &orders),
        to_instruction_error(SettlementError::InvalidBuyTokenAccount),
    );
}

#[test]
fn rejects_buy_token_account_owned_by_wrong_program() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // Genuine token-account bytes: right length, a real mint at offset 0, so
    // `from_account_view` would gladly read the mint. Assign those exact bytes
    // to an account not owned by the token program, so it's not a valid
    // token account.
    let genuine = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let token_shaped = svm
        .get_account(&genuine)
        .expect("the genuine token account exists")
        .data;
    let impostor = create_account(&mut svm, &Pubkey::new_unique(), &token_shaped);

    // `BeginSettle` only checks the push destination matches the intent's buy
    // token, so it accepts the impostor; `FinalizeSettle` rejects it when the
    // owner check in `from_account_view` fails, before the mint is ever read.
    let mut intent = sample_intent(payer.pubkey(), sell_token, 0);
    intent.buy_token_account = impostor;
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 0,
    }];

    assert_finalize_error(
        finalize(&mut svm, &program_id, &payer, &orders),
        to_instruction_error(SettlementError::InvalidBuyTokenAccount),
    );
}

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 100,
    }];

    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
    // Drop one byte so the trailing amount is no longer a whole `u64`. Begin
    // validates the push from the (unchanged) account metas and passes; finalize
    // then rejects the malformed data.
    finalize.data.pop();

    assert_finalize_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        InstructionError::InvalidInstructionData,
    );
}
