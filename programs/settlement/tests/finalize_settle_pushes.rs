//! Integration tests for the fund-push list carried by `FinalizeSettle`.

use crate::common::{order::OrderBuilder, setup, to_instruction_error, token};
use litesvm::LiteSVM;
use settlement_client::instructions::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent,
};
use settlement_client::settlement_interface::{Instruction, SettlementError};
use solana_sdk::{
    instruction::{AccountMeta, InstructionError},
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
fn finalizes_with_single_push() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &sell_mint).build();

    finalize(
        &mut svm,
        &program_id,
        &payer,
        &[FinalizedIntent {
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
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint)
        .salt(1)
        .build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint)
        .salt(2)
        .build();

    finalize(
        &mut svm,
        &program_id,
        &payer,
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
    )
    .expect("several pushes should parse and be accepted");
}

#[test]
fn finalizes_with_several_pushes_different_mint() {
    let (mut svm, program_id, payer) = setup();
    let mint0 = token::create_mint(&mut svm, &payer);
    let mint1 = token::create_mint(&mut svm, &payer);
    let intent0 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint0).build();
    let intent1 = OrderBuilder::new(&mut svm, &program_id, &payer, &mint1).build();

    finalize(
        &mut svm,
        &program_id,
        &payer,
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
    )
    .expect("several pushes should parse and be accepted");
}

#[test]
fn rejects_push_account_count_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &sell_mint).build();
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

    assert_eq!(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
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

    let result = send_settlement(&mut svm, &program_id, &payer, &[], finalize);
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

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer) = setup();
    let sell_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer, &sell_mint).build();
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

    assert_eq!(
        send_settlement(&mut svm, &program_id, &payer, &orders, finalize),
        Err(TransactionError::InstructionError(
            FINALIZE_INDEX,
            InstructionError::InvalidInstructionData,
        )),
    );
}
