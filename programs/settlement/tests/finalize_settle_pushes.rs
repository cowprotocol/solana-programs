//! Integration tests for the fund-push list carried by `FinalizeSettle`.

use crate::common::{order::OrderBuilder, send, setup, to_instruction_error, token};
use settlement_client::instructions::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent,
};
use settlement_client::settlement_interface::{Instruction, SettlementError};
use solana_sdk::{
    instruction::{AccountMeta, InstructionError},
    program_error::ProgramError,
    pubkey::Pubkey,
    transaction::TransactionError,
};

mod common;

/// Position of `BeginSettle` in the `[BeginSettle, FinalizeSettle]` pair the
/// tests in this file build; the finalize sits right after it. Kept in sync with
/// the `assert_eq!(index, FINALIZE_INDEX)` checks that a rejection came from the
/// finalize.
const BEGIN_INDEX: u8 = 0;
const FINALIZE_INDEX: u8 = 1;

/// Build the `[begin, finalize]` instructions where `finalize` is a pre-built
/// `FinalizeSettle` at [`FINALIZE_INDEX`] and `begin` settles `orders` (with no
/// pulls) at [`BEGIN_INDEX`], the same orders the finalize is expected to push
/// to. Submit the result with [`send`].
fn build_settlement(
    program_id: &Pubkey,
    orders: &[FinalizedIntent],
    finalize: impl Into<Instruction>,
) -> Vec<Instruction> {
    let begin_orders: Vec<InitializedIntent> = orders
        .iter()
        .map(|order| InitializedIntent {
            intent: order.intent,
            pulls: &[],
        })
        .collect();
    let begin = BeginSettle {
        program_id: *program_id,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders: &begin_orders,
    };
    vec![begin.into(), finalize.into()]
}

/// Build the minimal `[BeginSettle, FinalizeSettle]` instructions that settle
/// `orders` (begin) and push their proceeds (finalize).
fn finalize(program_id: &Pubkey, orders: &[FinalizedIntent]) -> Vec<Instruction> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders,
    };
    build_settlement(program_id, orders, finalize)
}

#[test]
fn finalizes_with_no_pushes() {
    let (mut svm, program_id, payer) = setup();

    let instructions = finalize(&program_id, &[]);
    send(&mut svm, &payer, instructions).expect("a finalize with no pushes should succeed");
}

#[test]
fn finalizes_with_single_push() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();

    let instructions = finalize(
        &program_id,
        &[FinalizedIntent {
            intent: &intent,
            mint: Pubkey::new_unique(),
            amount: 1_000,
        }],
    );
    send(&mut svm, &payer, instructions).expect("a single push should parse and be accepted");
}

#[test]
fn finalizes_with_several_pushes_same_mint() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .salt(1)
        .buy_mint(&mint)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .salt(2)
        .buy_mint(&mint)
        .build();

    let instructions = finalize(
        &program_id,
        &[
            FinalizedIntent {
                intent: &intent0,
                mint,
                amount: 1_000,
            },
            FinalizedIntent {
                intent: &intent1,
                mint,
                amount: 2_000,
            },
        ],
    );
    send(&mut svm, &payer, instructions).expect("several pushes should parse and be accepted");
}

#[test]
fn finalizes_with_several_pushes_different_mint() {
    let (mut svm, program_id, payer) = setup();
    let mint0 = token::create_mint(&mut svm, &payer);
    let mint1 = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint0)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint1)
        .build();

    let instructions = finalize(
        &program_id,
        &[
            FinalizedIntent {
                intent: &intent0,
                mint: mint0,
                amount: 1_000,
            },
            FinalizedIntent {
                intent: &intent1,
                mint: mint1,
                amount: 2_000,
            },
        ],
    );
    send(&mut svm, &payer, instructions).expect("several pushes should parse and be accepted");
}

#[test]
fn rejects_push_account_count_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 1_000,
    }];

    // A well-formed single-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
    // ...with one extra account appended.
    finalize
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));

    let instructions = build_settlement(&program_id, &orders, finalize);
    assert_eq!(
        send(&mut svm, &payer, instructions),
        Err(TransactionError::InstructionError(
            FINALIZE_INDEX,
            to_instruction_error(SettlementError::AccountCountNotMatchingPushCount),
        )),
    );
}

#[test]
fn rejects_too_few_accounts() {
    let (mut svm, program_id, payer) = setup();

    // A well-formed single-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &[],
    });
    // ...with one account popped.
    finalize.accounts.pop();

    let instructions = build_settlement(&program_id, &[], finalize);
    let result = send(&mut svm, &payer, instructions);
    let Err(TransactionError::InstructionError(index, ix_error)) = result else {
        panic!("expected an instruction error, got {result:?}");
    };
    assert_eq!(index, FINALIZE_INDEX);
    assert_eq!(
        // This unusual way to test is because `InstructionError::NotEnoughAccountKeys`
        // is deprecated, while `ProgramError::NotEnoughAccountKeys` is not.
        // Rather than silencing a linting, let's use the program error.
        ProgramError::try_from(ix_error),
        Ok(ProgramError::NotEnoughAccountKeys),
    );
}

// Similar to `rejects_too_few_accounts`, but pops two accounts instead of one.
// This is because variable-length accounts in the instruction are naturally
// grouped in pairs, so a single missing account could just be an unsuccessful
// pairing rather than accounting for missing accounts.
#[test]
fn rejects_two_too_few_accounts() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 1_000,
    }];

    // A well-formed single-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
    // ...with that push's whole (source, destination) pair popped, so the data
    // still declares one push while no push accounts remain.
    finalize.accounts.pop();
    finalize.accounts.pop();

    // The paired `Begin` settles no orders, so it never checks the push
    // destinations: the inconsistency is left for the finalize's own
    // account-count check to reject.
    let instructions = build_settlement(&program_id, &[], finalize);
    assert_eq!(
        send(&mut svm, &payer, instructions),
        Err(TransactionError::InstructionError(
            FINALIZE_INDEX,
            to_instruction_error(SettlementError::AccountCountNotMatchingPushCount),
        )),
    );
}

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 1_000,
    }];

    // A well-formed single-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &orders,
    });
    // ...with one byte popped, so the trailing amount is no longer a whole `u64`.
    finalize.data.pop();

    let instructions = build_settlement(&program_id, &orders, finalize);
    assert_eq!(
        send(&mut svm, &payer, instructions),
        Err(TransactionError::InstructionError(
            FINALIZE_INDEX,
            InstructionError::InvalidInstructionData,
        )),
    );
}
