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
    assert_failed_in_cpi, buffer, create_account,
    order::{create_order_pda, sample_intent, OrderBuilder},
    replace_first_matching_account, send, send_expect_failure,
    settlement::{build_settlement, BEGIN_INDEX, FINALIZE_INDEX},
    setup, to_instruction_error, token,
};
use settlement_client::instructions::{FinalizeSettle, FinalizedIntent};
use settlement_client::settlement_interface::{
    instruction::settle::SPL_TOKEN_PROGRAM_ID, pda::state::find_state_pda, Instruction,
    SettlementError,
};
use solana_sdk::{
    instruction::InstructionError, program_error::ProgramError, pubkey::Pubkey, signature::Signer,
    transaction::TransactionError,
};

mod common;

/// Assert the transaction failed in `FinalizeSettle` (at [`FINALIZE_INDEX`])
/// with `expected`.
fn assert_finalize_error<T>(result: Result<T, TransactionError>, expected: InstructionError) {
    assert_eq!(
        result.err(),
        Some(TransactionError::InstructionError(FINALIZE_INDEX, expected)),
    );
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
fn pushes_a_single_order() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer)
        .buy_mint(&mint)
        .build();
    let funding = 1_000;
    let buffer_pda = buffer::ensure_funded(&mut svm, &program_id, &payer, &mint, funding);

    let amount = 400;
    let instructions = finalize(
        &program_id,
        &[FinalizedIntent {
            intent: &intent,
            mint,
            amount,
        }],
    );
    send(&mut svm, &payer, instructions).expect("a single push should be paid");

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
    let instructions = finalize(
        &program_id,
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
    );
    send(&mut svm, &payer, instructions).expect("several pushes from one buffer should be paid");

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
    let instructions = finalize(
        &program_id,
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
    );
    send(&mut svm, &payer, instructions).expect("pushes from different buffers should be paid");

    assert_eq!(token::balance(&svm, &intent0.buy_token_account), amount0);
    assert_eq!(token::balance(&svm, &intent1.buy_token_account), amount1);
    assert_eq!(token::balance(&svm, &buffer0), funding - amount0);
    assert_eq!(token::balance(&svm, &buffer1), funding - amount1);
}

#[test]
fn rejects_push_if_buffer_does_not_match_mint() {
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

    let instructions = finalize(&program_id, &orders);
    assert_finalize_error(
        send(&mut svm, &payer, instructions),
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

    let instructions = build_settlement(&program_id, &orders, finalize);
    assert_finalize_error(
        send(&mut svm, &payer, instructions),
        to_instruction_error(SettlementError::PushSourceNotBuffer),
    );
}

#[test]
fn rejects_wrong_token_program() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 0,
    }];

    let mut instructions = finalize(&program_id, &orders);
    replace_first_matching_account(
        &mut instructions[usize::from(FINALIZE_INDEX)],
        &SPL_TOKEN_PROGRAM_ID,
        Pubkey::new_unique(),
    );

    assert_finalize_error(
        send(&mut svm, &payer, instructions),
        InstructionError::IncorrectProgramId,
    );
}

#[test]
fn rejects_wrong_state_pda() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
        amount: 0,
    }];

    let mut instructions = finalize(&program_id, &orders);
    let (state_pda, _bump) = find_state_pda(&program_id);
    replace_first_matching_account(
        &mut instructions[usize::from(FINALIZE_INDEX)],
        &state_pda,
        Pubkey::new_unique(),
    );

    assert_finalize_error(
        send(&mut svm, &payer, instructions),
        to_instruction_error(SettlementError::StateAccountMismatch),
    );
}

#[test]
fn rejects_push_account_count_mismatch() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
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

    let instructions = build_settlement(&program_id, &orders, finalize);
    assert_finalize_error(
        send(&mut svm, &payer, instructions),
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

    let instructions = build_settlement(&program_id, &[], finalize);
    let err = send(&mut svm, &payer, instructions)
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
    assert_finalize_error(
        send(&mut svm, &payer, instructions),
        to_instruction_error(SettlementError::AccountCountNotMatchingPushCount),
    );
}

#[test]
fn rejects_invalid_buy_token_account() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // The order's buy token account (the push destination) isn't a token
    // account. `BeginSettle` accepts it (the destination still matches the
    // intent's buy token); the buffer for the mint exists, so the push clears
    // `load_verified_from_pda` and the invalid destination only bites in the
    // transfer.
    let not_a_token_account = Pubkey::new_unique();
    let mut intent = sample_intent(payer.pubkey(), sell_token, 0);
    intent.buy_token_account = not_a_token_account;
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    buffer::ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 0,
    }];

    let instructions = finalize(&program_id, &orders);
    let failed = send_expect_failure(&mut svm, &payer, instructions);
    // The rejection now happens one level down, inside the SPL transfer CPI,
    // rather than in our program.
    assert_failed_in_cpi(&failed);
    // Kept deliberately failing: under the old destination-mint semantics our
    // program returned `InvalidBuyTokenAccount` here. The new semantics never
    // reads the destination, so this no longer holds — it marks the change.
    assert_finalize_error(
        Err::<(), _>(failed.err),
        to_instruction_error(SettlementError::InvalidBuyTokenAccount),
    );
}

#[test]
fn rejects_buy_token_account_owned_by_wrong_program() {
    let (mut svm, program_id, payer) = setup();
    let mint = token::create_mint(&mut svm, &payer);
    let sell_token = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());

    // Genuine token-account bytes: right length, a real mint at offset 0.
    // Assign those exact bytes to an account not owned by the token program, so
    // it's not a valid token account.
    let genuine = token::create_token_account(&mut svm, &payer, &mint, &payer.pubkey());
    let token_shaped = svm
        .get_account(&genuine)
        .expect("the genuine token account exists")
        .data;
    let impostor = create_account(&mut svm, &Pubkey::new_unique(), &token_shaped);

    // `BeginSettle` only checks the push destination matches the intent's buy
    // token, so it accepts the impostor; with the buffer present the push
    // reaches the transfer, where the token program rejects the wrong owner.
    let mut intent = sample_intent(payer.pubkey(), sell_token, 0);
    intent.buy_token_account = impostor;
    create_order_pda(&mut svm, &program_id, &payer, &intent);
    buffer::ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);
    let orders = [FinalizedIntent {
        intent: &intent,
        mint,
        amount: 0,
    }];

    let instructions = finalize(&program_id, &orders);
    let failed = send_expect_failure(&mut svm, &payer, instructions);
    // The rejection now happens one level down, inside the SPL transfer CPI,
    // rather than in our program.
    assert_failed_in_cpi(&failed);
    // Kept deliberately failing: under the old destination-mint semantics our
    // program returned `InvalidBuyTokenAccount` here. The new semantics never
    // reads the destination, so this no longer holds — it marks the change.
    assert_finalize_error(
        Err::<(), _>(failed.err),
        to_instruction_error(SettlementError::InvalidBuyTokenAccount),
    );
}

#[test]
fn rejects_partial_push_amount() {
    let (mut svm, program_id, payer) = setup();
    let intent = OrderBuilder::new(&mut svm, &program_id, &payer).build();
    let orders = [FinalizedIntent {
        intent: &intent,
        mint: Pubkey::new_unique(),
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

    let instructions = build_settlement(&program_id, &orders, finalize);
    assert_finalize_error(
        send(&mut svm, &payer, instructions),
        InstructionError::InvalidInstructionData,
    );
}
