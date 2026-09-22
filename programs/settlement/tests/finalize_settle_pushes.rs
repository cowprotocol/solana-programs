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
    assert_instruction_error_at,
    benchmark::BenchLabel,
    buffer, create_account,
    order::{create_order_pda, settlable_intent, OrderBuilder},
    replace_first_matching_account, send, send_metered,
    settlement::{build_settlement, BEGIN_INDEX, FINALIZE_INDEX},
    setup_settle_ready, token, unique_pubkey,
};
use cow_settlement_client::instruction::{FinalizeSettle, FinalizedIntent};
use cow_settlement_client::{
    cow_settlement_interface::{
        data::intent::{Asset, OrderIntent, TokenAsset},
        pda::state::find_state_pda,
        Instruction, SettlementError,
    },
    instruction::TokenProgram,
};
use litesvm_token::spl_token::error::TokenError;
use solana_sdk::{
    instruction::InstructionError, program_error::ProgramError, pubkey::Pubkey, signer::Signer,
    transaction::TransactionError,
};

mod common;

/// Assert the transaction failed in `FinalizeSettle` (at [`FINALIZE_INDEX`])
/// with `expected`.
#[track_caller]
fn assert_finalize_error<T>(
    result: Result<T, TransactionError>,
    expected: impl Into<InstructionError>,
) {
    assert_instruction_error_at(FINALIZE_INDEX, result, expected);
}

/// Build the minimal `[BeginSettle, FinalizeSettle]` instructions that settle
/// `orders` (begin) and push their proceeds (finalize).
fn finalize(program_id: &Pubkey, solver: &Pubkey, orders: &[FinalizedIntent]) -> Vec<Instruction> {
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders,
    };
    build_settlement(program_id, solver, orders, finalize)
}

#[test]
fn finalizes_with_no_pushes() {
    let (mut svm, program_id, _payer, solver) = setup_settle_ready();

    let instructions = finalize(&program_id, &solver.pubkey(), &[]);
    send_metered(&mut svm, &solver, &instructions, BenchLabel::Settle)
        .expect("a finalize with no pushes should succeed");
}

#[test]
fn pushes_a_single_order() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint)
        .build();
    let funding = 1_000;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, funding);

    let amount = 400;
    let instructions = finalize(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount,
        }],
    );
    send_metered(&mut svm, &solver, &instructions, BenchLabel::Settle)
        .expect("a single push should be paid");

    assert_eq!(token::balance(&svm, &intent.buy.account()), amount);
    assert_eq!(token::balance(&svm, &buffer_pda), funding - amount);
}

#[test]
fn pushes_several_orders_from_one_buffer() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
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
    let instructions = finalize(
        &program_id,
        &solver.pubkey(),
        &[
            FinalizedIntent {
                intent: &intent0,
                amount: amount0,
            },
            FinalizedIntent {
                intent: &intent1,
                amount: amount1,
            },
        ],
    );
    send_metered(&mut svm, &solver, &instructions, BenchLabel::Settle)
        .expect("several pushes from one buffer should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy.account()), amount0);
    assert_eq!(token::balance(&svm, &intent1.buy.account()), amount1);
    assert_eq!(
        token::balance(&svm, &buffer_pda),
        funding - amount0 - amount1,
    );
}

#[test]
fn pushes_several_orders_from_different_buffers() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
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
    let instructions = finalize(
        &program_id,
        &solver.pubkey(),
        &[
            FinalizedIntent {
                intent: &intent0,
                amount: amount0,
            },
            FinalizedIntent {
                intent: &intent1,
                amount: amount1,
            },
        ],
    );
    send_metered(&mut svm, &solver, &instructions, BenchLabel::Settle)
        .expect("pushes from different buffers should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy.account()), amount0);
    assert_eq!(token::balance(&svm, &intent1.buy.account()), amount1);
    assert_eq!(token::balance(&svm, &buffer0), funding - amount0);
    assert_eq!(token::balance(&svm, &buffer1), funding - amount1);
}

#[test]
fn rejects_buy_token_account_recreated_for_another_mint() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let buy_mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&buy_mint)
        .build();
    buffer::ensure_funded(&mut svm, &program_id, &payer, &buy_mint, 1_000);

    let another_mint = token::create_mint(&mut svm, &payer);
    token::overwrite_token_account(&mut svm, &payer, &intent.buy.account(), &another_mint);

    let instructions = finalize(
        &program_id,
        &solver.pubkey(),
        &[FinalizedIntent {
            intent: &intent,
            amount: 100,
        }],
    );
    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        InstructionError::Custom(TokenError::MintMismatch as u32),
    );
}

#[test]
fn rejects_a_token_program_the_instruction_doesnt_name() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 0,
    }];

    let mut instructions = finalize(&program_id, &solver.pubkey(), &orders);
    replace_first_matching_account(
        &mut instructions[usize::from(FINALIZE_INDEX)],
        &TokenProgram::SplToken.address(),
        unique_pubkey(),
    );

    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        InstructionError::MissingAccount,
    );
}

#[test]
fn rejects_wrong_state_pda() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 0,
    }];

    let mut instructions = finalize(&program_id, &solver.pubkey(), &orders);
    let (state_pda, _bump) = find_state_pda(&program_id);
    replace_first_matching_account(
        &mut instructions[usize::from(FINALIZE_INDEX)],
        &state_pda,
        unique_pubkey(),
    );

    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::StateAccountMismatch,
    );
}

#[test]
fn rejects_push_account_count_mismatch() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 100,
    }];

    // A well-formed single-push finalize (five accounts, a nine-byte push body)...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders: &orders,
    });
    // ...with another push's worth of data bytes appended but no matching
    // accounts. `BeginSettle` derives the push count from the (unchanged) account
    // metas (one push, matching its one order and paying the right destination)
    // so it passes. Only the finalize reads the data, where it now parses two
    // pushes against two push accounts and rejects the mismatch. This is the
    // account/data disagreement `BeginSettle` structurally can't see.
    finalize.data.extend_from_slice(&[0u8; 9]);

    let instructions = build_settlement(&program_id, &solver.pubkey(), &orders, finalize);
    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::AccountCountNotMatchingPushCount,
    );
}

#[test]
fn rejects_too_few_accounts() {
    let (mut svm, program_id, _payer, solver) = setup_settle_ready();

    // A well-formed no-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders: &[],
    });
    // ...with one of its fixed accounts popped. `BeginSettle` runs first
    // but only reads push destinations off the accounts (finding none, matching
    // its zero orders) so it passes. The finalize then can't even destructure
    // its fixed accounts and raises `NotEnoughAccountKeys`.
    finalize.accounts.pop();

    let instructions = build_settlement(&program_id, &solver.pubkey(), &[], finalize);
    let err = send(&mut svm, &solver, &instructions)
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
    let (mut svm, program_id, payer, solver) = setup_settle_ready();

    let settlable = settlable_intent(&mut svm, &payer, payer.pubkey(), 0);
    // The mint account is a convenient invalid account we can use
    let intent = OrderIntent {
        buy: Asset::from(TokenAsset {
            mint: settlable.buy.mint(),
            token_account: settlable.buy.mint(),
        }),
        ..settlable
    };
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    buffer::ensure_funded(&mut svm, &program_id, &payer, &intent.buy.mint(), 1_000);
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 0,
    }];

    let instructions = finalize(&program_id, &solver.pubkey(), &orders);
    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        InstructionError::InvalidAccountData,
    );
}

#[test]
fn rejects_buy_account_under_a_unsupported_token_program() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let settlable = settlable_intent(&mut svm, &payer, payer.pubkey(), 0);

    let fake_token_program = create_account(&mut svm, &payer.pubkey(), &[]);
    let impostor =
        token::clone_under_new_program(&mut svm, &settlable.buy.account(), &fake_token_program);

    // As above, the impostor passes both instructions' push checks (the push
    // pays `intent.buy.account()` from `intent.buy.mint()`'s buffer), but its
    // owner is no token program, so there is nothing to issue the push against.
    let intent = OrderIntent {
        buy: Asset::from(TokenAsset {
            mint: settlable.buy.mint(),
            token_account: impostor,
        }),
        ..settlable
    };
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    buffer::ensure_funded(&mut svm, &program_id, &payer, &intent.buy.mint(), 1_000);
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 0,
    }];

    let instructions = finalize(&program_id, &solver.pubkey(), &orders);
    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::InvalidTokenProgram,
    );
}

// Similar to `rejects_too_few_accounts`, but pops two accounts instead of one.
// This is because variable-length accounts in the instruction are naturally
// grouped in pairs, so a single missing account could just be an unsuccessful
// pairing rather than accounting for missing accounts.
#[test]
fn rejects_two_too_few_accounts() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 1_000,
    }];

    // A well-formed single-push finalize...
    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders: &orders,
    });
    // ...with that push's whole (source, destination) pair popped, so the data
    // still declares one push while no push accounts remain.
    finalize.accounts.pop();
    finalize.accounts.pop();

    // The paired `Begin` settles no orders, so it never checks the push
    // destinations: the inconsistency is left for the finalize's own
    // account-count check to reject.
    let instructions = build_settlement(&program_id, &solver.pubkey(), &[], finalize);
    assert_finalize_error(
        send(&mut svm, &solver, &instructions),
        SettlementError::AccountCountNotMatchingPushCount,
    );
}

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer, _solver) = setup_settle_ready();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        amount: 100,
    }];

    let mut finalize = Instruction::from(FinalizeSettle {
        program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders: &orders,
    });
    // Drop one byte so the trailing amount is no longer a whole `u64`.
    finalize.data.pop();

    // Send the finalize on its own, with no paired `BeginSettle` ahead of it.
    // In a normal pair `BeginSettle` reads the finalize's push amounts too and
    // would reject this first. `FinalizeSettle` parses (and so validates) its
    // own data before it even checks its counterpart, so on its own it rejects
    // the malformed encoding by itself, at index 0.
    assert_eq!(
        send(&mut svm, &payer, &[finalize]).err(),
        Some(TransactionError::InstructionError(
            0,
            InstructionError::InvalidInstructionData,
        )),
    );
}
