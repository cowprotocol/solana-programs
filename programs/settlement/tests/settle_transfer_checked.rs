//! Integration tests for settling through `TransferChecked`: a settlement names
//! the mints whose transfers need it.

use crate::common::{
    assert_instruction_error, assert_instruction_error_at,
    benchmark::BenchLabel,
    order::{buy_account, buy_mint, OrderBuilder},
    replace_first_matching_account, send_metered, send_with_signers,
    settlement::{stage_order, StagedOrder, BEGIN_INDEX, FINALIZE_INDEX},
    setup_settle_ready, token,
    token_2022::{Extensions, FEE_BASIS_POINTS},
    unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{
    data::intent::OrderIntent, Instruction, SettlementError,
};
use cow_settlement_client::instruction::{
    BeginSettle, FinalizeSettle, FinalizedIntent, InitializedIntent, TokenProgram,
};
use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::spl_token::error::TokenError;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::TransactionError,
};
use spl_token_2022_interface::error::TokenError as Token2022Error;

mod common;

/// The amount every order here sells and buys.
const AMOUNT: u64 = 1_000;

/// An order selling [`AMOUNT`] of `sell_mint` for [`AMOUNT`] of `buy_mint`,
/// staged so a settlement can pull all of it and push all of its proceeds.
fn staged_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    sell_mint: &Pubkey,
    buy_mint: &Pubkey,
) -> StagedOrder {
    let intent = OrderBuilder::new(svm, program_id, payer)
        .sell_mint(sell_mint)
        .buy_mint(buy_mint)
        .sell_amount(AMOUNT)
        .buy_amount(AMOUNT)
        .build();
    stage_order(svm, program_id, payer, &intent, &[AMOUNT], AMOUNT)
}

/// The `[BeginSettle, FinalizeSettle]` pair settling `order`, with
/// `TransferChecked` for the transfers of `transfer_checked_mints`.
fn settlement(
    program_id: &Pubkey,
    solver: &Pubkey,
    order: &StagedOrder,
    transfer_checked_mints: &[Pubkey],
) -> Vec<Instruction> {
    let begin = BeginSettle {
        program_id: *program_id,
        solver: *solver,
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        only_token_program: None,
        orders: &[InitializedIntent {
            intent: &order.intent,
            pulls: &order.pulls,
            use_transfer_checked: transfer_checked_mints.contains(&order.intent.sell.mint),
        }],
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        only_token_program: None,
        orders: &[FinalizedIntent {
            intent: &order.intent,
            amount: order.amount_out,
            use_transfer_checked: transfer_checked_mints.contains(&buy_mint(&order.intent)),
        }],
    };
    vec![begin.into(), finalize.into()]
}

fn send(
    svm: &mut LiteSVM,
    payer: &Keypair,
    solver: &Keypair,
    instructions: &[Instruction],
) -> Result<TransactionMetadata, TransactionError> {
    send_with_signers(svm, payer, &[solver], instructions)
}

/// The order's sell side was pulled in full and its buy side paid `received`.
#[track_caller]
fn assert_settled(svm: &LiteSVM, order: &StagedOrder, received: u64) {
    assert_eq!(token::balance(svm, &order.intent.sell.token_account), 0);
    assert_eq!(token::balance(svm, &order.pulls[0].destination), AMOUNT);
    assert_eq!(token::balance(svm, &buy_account(&order.intent)), received);
}

fn sell_mint(intent: &OrderIntent) -> Pubkey {
    intent.sell.mint
}

#[test]
fn settles_both_token_programs_with_transfer_checked() {
    for program in TokenProgram::ALL {
        let (mut svm, program_id, payer, solver) = setup_settle_ready();
        let sell = token::create_mint_under(&mut svm, &payer, &program.address(), Extensions::None);
        let buy = token::create_mint_under(&mut svm, &payer, &program.address(), Extensions::None);
        let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);

        let instructions = settlement(&program_id, &solver.pubkey(), &order, &[sell, buy]);
        send(&mut svm, &payer, &solver, &instructions)
            .unwrap_or_else(|error| panic!("{program:?} should settle checked: {error:?}"));

        assert_settled(&svm, &order, AMOUNT);
    }
}

/// The checked counterpart of `settles_a_single_order`, for comparing the cost
/// of `TransferChecked` against `Transfer`.
#[test]
fn settles_a_single_order_with_transfer_checked() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);

    let instructions = settlement(&program_id, &solver.pubkey(), &order, &[sell, buy]);
    send_metered(&mut svm, &solver, &instructions, BenchLabel::Settle)
        .expect("a checked settlement should settle");

    assert_settled(&svm, &order, AMOUNT);
}

#[test]
fn transfer_fee_mints_settle_only_with_transfer_checked() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let token_2022 = TokenProgram::Token2022.address();
    let fee = Extensions::CloseAuthorityAndTransferFee;
    let sell = token::create_mint_under(&mut svm, &payer, &token_2022, fee);
    let buy = token::create_mint_under(&mut svm, &payer, &token_2022, fee);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);

    // Token-2022 refuses a plain `Transfer` of a mint charging a fee.
    let unchecked = settlement(&program_id, &solver.pubkey(), &order, &[]);
    assert_instruction_error(
        send(&mut svm, &payer, &solver, &unchecked),
        InstructionError::Custom(Token2022Error::MintRequiredForTransfer as u32),
    );

    let checked = settlement(&program_id, &solver.pubkey(), &order, &[sell, buy]);
    send(&mut svm, &payer, &solver, &checked).expect("a checked settlement should pay the fee");

    // The fee is withheld from what each transfer delivers, the pull's included.
    let fee = AMOUNT * FEE_BASIS_POINTS / 10_000;
    assert_eq!(token::balance(&svm, &order.intent.sell.token_account), 0);
    assert_eq!(
        token::balance(&svm, &order.pulls[0].destination),
        AMOUNT - fee
    );
    assert_eq!(
        token::balance(&svm, &buy_account(&order.intent)),
        AMOUNT - fee
    );
}

#[test]
fn rejects_a_mint_slot_that_isnt_a_mint() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);
    let checked = [sell, buy];

    // The sell mint only appears in `BeginSettle`, the buy mint only in
    // `FinalizeSettle`, each as that instruction's mint slot.
    for (index, mint) in [(BEGIN_INDEX, sell), (FINALIZE_INDEX, buy)] {
        let mut instructions = settlement(&program_id, &solver.pubkey(), &order, &checked);
        replace_first_matching_account(
            &mut instructions[usize::from(index)],
            &mint,
            unique_pubkey(),
        );
        assert_instruction_error_at(
            index,
            send(&mut svm, &payer, &solver, &instructions),
            SettlementError::InvalidMint,
        );
    }
}

#[test]
fn token_program_rejects_a_mint_other_than_the_transferred_one() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);
    let checked = [sell, buy];

    // Swapping in the order's other mint: a real mint of the right program,
    // just not the one the transfer moves.
    for (index, mint, other) in [
        (BEGIN_INDEX, sell, buy_mint(&order.intent)),
        (FINALIZE_INDEX, buy, sell_mint(&order.intent)),
    ] {
        let mut instructions = settlement(&program_id, &solver.pubkey(), &order, &checked);
        replace_first_matching_account(&mut instructions[usize::from(index)], &mint, other);
        assert_instruction_error_at(
            index,
            send(&mut svm, &payer, &solver, &instructions),
            InstructionError::Custom(TokenError::MintMismatch as u32),
        );
    }
}
