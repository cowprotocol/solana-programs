//! Integration tests for the fund-push list carried by `FinalizeSettle`.
//!
//! Each settlement transaction here is a `[BeginSettle, FinalizeSettle]` pair
//! (begin at index 0 pointing to finalize at index 1, and vice versa). Begin
//! settles the same orders the finalize pushes to — created on-chain via
//! `OrderBuilder` but with no pulls, so no funds move — and the tests assert
//! that the finalize push layout parses (or is rejected). The buffers and
//! destinations need not exist: this step's finalize handler neither reads nor
//! writes them.

use crate::common::{order::OrderBuilder, setup, to_instruction_error, token};
use litesvm::LiteSVM;
use settlement_client::instructions::{BeginSettle, FinalizeSettle, SettleableOrder, SettledOrder};
use settlement_client::settlement_interface::{Instruction, SettlementError};
use solana_sdk::{
    instruction::{AccountMeta, InstructionError},
    program_error::ProgramError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

mod common;

/// Assert that the transaction failed in the `FinalizeSettle` instruction, which
/// always sits at index 1 (begin must precede finalize).
fn assert_finalize_error(result: Result<(), TransactionError>, expected: InstructionError) {
    assert_eq!(result, Err(TransactionError::InstructionError(1, expected)));
}

/// Like [`assert_finalize_error`], but compares the failure on the program-error
/// side. Converting the recorded `InstructionError` to a `ProgramError` lets us
/// name `ProgramError::NotEnoughAccountKeys` rather than its deprecated
/// `InstructionError` mirror.
fn assert_finalize_program_error(result: Result<(), TransactionError>, expected: ProgramError) {
    let Err(TransactionError::InstructionError(index, ix_error)) = result else {
        panic!("expected a finalize instruction error, got {result:?}");
    };
    assert_eq!(index, 1);
    assert_eq!(ProgramError::try_from(ix_error), Ok(expected));
}

/// Send `[begin, finalize]` signed by `payer`, where `finalize` is a pre-built
/// `FinalizeSettle` at index 1 and `begin` settles `orders` (with no pulls) at
/// index 0 — the same orders the finalize is expected to push to.
fn send_settlement(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    orders: &[SettledOrder],
    finalize: Instruction,
) -> Result<(), TransactionError> {
    let begin_orders: Vec<SettleableOrder> = orders
        .iter()
        .map(|order| SettleableOrder {
            intent: order.intent,
            pulls: &[],
        })
        .collect();
    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: 1,
        orders: &begin_orders,
    }
    .instruction();
    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
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
    orders: &[SettledOrder],
) -> Result<(), TransactionError> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: 0,
        orders,
    }
    .instruction();
    send_settlement(svm, program_id, payer, orders, finalize)
}

#[test]
fn finalizes_with_no_pushes() {
    let (mut svm, program_id, payer) = setup();

    finalize(&mut svm, &program_id, &payer, &[]).expect("a finalize with no pushes should succeed");
}

#[test]
fn finalizes_with_single_push() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &sell_mint).build();

    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[SettledOrder {
            intent: &intent,
            mint: Pubkey::new_unique(),
            amount: 1_000,
        }],
    )
    .expect("a single push should parse and be accepted");
}

#[test]
fn finalizes_with_several_pushes_same_mint() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint).build();

    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[
            SettledOrder {
                intent: &intent0,
                mint,
                amount: 1_000,
            },
            SettledOrder {
                intent: &intent1,
                mint,
                amount: 2_000,
            },
        ],
    )
    .expect("several pushes should parse and be accepted");
}

#[test]
fn finalizes_with_several_pushes_different_mint() {
    let (mut svm, program_id, payer) = setup();
    let mint_1 = token::create_mint(&mut svm, &payer);
    let mint_2 = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint_1).build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint_2).build();

    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[
            SettledOrder {
                intent: &intent0,
                mint: mint_1,
                amount: 1_000,
            },
            SettledOrder {
                intent: &intent1,
                mint: mint_2,
                amount: 2_000,
            },
        ],
    )
    .expect("several pushes should parse and be accepted");
}

#[test]
fn rejects_push_account_count_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &sell_mint).build();
    let orders = [SettledOrder {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 1_000,
    }];

    // A well-formed single-push finalize...
    let mut finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &orders,
    }
    .instruction();
    // ...with one extra account appended, so the account count no longer matches
    // the `2T` the instruction data implies.
    finalize
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));

    assert_finalize_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        to_instruction_error(SettlementError::AccountCountNotMatchingPushCount),
    );
}

#[test]
fn rejects_too_few_accounts() {
    let (mut svm, program_id, payer) = setup();

    // A well-formed single-push finalize...
    let mut finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &[],
    }
    .instruction();
    // ...with one account popped, so the accounts don't match with the amounts anymore.
    finalize.accounts.pop();

    assert_finalize_program_error(
        send_settlement(&mut svm, &program_id, &payer, &[], finalize),
        ProgramError::NotEnoughAccountKeys,
    );
}

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &sell_mint).build();
    let orders = [SettledOrder {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 1_000,
    }];

    // A well-formed single-push finalize...
    let mut finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &orders,
    }
    .instruction();
    // ...with one byte popped, so the trailing amount is no longer a whole `u64`.
    finalize.data.pop();

    assert_finalize_error(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        InstructionError::InvalidInstructionData,
    );
}
