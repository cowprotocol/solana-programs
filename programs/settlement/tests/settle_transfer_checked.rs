//! Integration tests for settling through `TransferChecked`: a settlement names
//! the mints whose transfers need it, and its extra accounts ride along on
//! every such transfer.

use crate::common::{
    assert_instruction_error, assert_instruction_error_at,
    benchmark::BenchLabel,
    order::{buy_account, buy_mint, OrderBuilder},
    replace_first_matching_account, send_metered, send_with_signers,
    settlement::{stage_order, StagedOrder, BEGIN_INDEX, FINALIZE_INDEX},
    setup_settle_ready, token,
    token_2022::{Extensions, FEE_BASIS_POINTS},
    transfer_hook::{TransferHook, REJECTED},
    unique_pubkey,
};
use cow_settlement_client::cow_settlement_interface::{
    data::intent::OrderIntent, AccountMeta, Instruction, SettlementError,
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

/// A settled order's mints: which ones its transfers name for
/// `TransferChecked`, and the extra accounts each instruction carries.
#[derive(Default)]
struct Checked<'a> {
    mints: &'a [Pubkey],
    begin_extra_accounts: &'a [AccountMeta],
    finalize_extra_accounts: &'a [AccountMeta],
}

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

/// The `[BeginSettle, FinalizeSettle]` pair settling `order` with `checked`.
fn settlement(
    program_id: &Pubkey,
    solver: &Pubkey,
    order: &StagedOrder,
    checked: &Checked,
) -> Vec<Instruction> {
    let begin = BeginSettle {
        program_id: *program_id,
        solver: *solver,
        finalize_ix_index: FINALIZE_INDEX.into(),
        orders: &[InitializedIntent {
            intent: &order.intent,
            pulls: &order.pulls,
        }],
        transfer_checked_mints: checked.mints,
        extra_accounts: checked.begin_extra_accounts,
        ..Default::default()
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &[FinalizedIntent {
            intent: &order.intent,
            amount: order.amount_out,
        }],
        transfer_checked_mints: checked.mints,
        extra_accounts: checked.finalize_extra_accounts,
        ..Default::default()
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

        let instructions = settlement(
            &program_id,
            &solver.pubkey(),
            &order,
            &Checked {
                mints: &[sell, buy],
                ..Default::default()
            },
        );
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

    let instructions = settlement(
        &program_id,
        &solver.pubkey(),
        &order,
        &Checked {
            mints: &[sell, buy],
            ..Default::default()
        },
    );
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
    let unchecked = settlement(&program_id, &solver.pubkey(), &order, &Checked::default());
    assert_instruction_error(
        send(&mut svm, &payer, &solver, &unchecked),
        InstructionError::Custom(Token2022Error::MintRequiredForTransfer as u32),
    );

    let checked = settlement(
        &program_id,
        &solver.pubkey(),
        &order,
        &Checked {
            mints: &[sell, buy],
            ..Default::default()
        },
    );
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
fn extra_accounts_dont_affect_plain_transfers() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);

    let extra_accounts = [
        AccountMeta::new_readonly(unique_pubkey(), false),
        AccountMeta::new(unique_pubkey(), false),
    ];
    let instructions = settlement(
        &program_id,
        &solver.pubkey(),
        &order,
        &Checked {
            begin_extra_accounts: &extra_accounts,
            finalize_extra_accounts: &extra_accounts,
            ..Default::default()
        },
    );
    send(&mut svm, &payer, &solver, &instructions).expect("extra accounts should be ignored");

    assert_settled(&svm, &order, AMOUNT);
}

/// A settlement of an order whose sell or buy mint executes a transfer hook,
/// set up to the point of choosing what the settlement carries.
struct HookedOrder {
    svm: LiteSVM,
    program_id: Pubkey,
    payer: Keypair,
    solver: Keypair,
    hook: TransferHook,
    hooked_mint: Pubkey,
    order: StagedOrder,
}

impl HookedOrder {
    fn new(hooked_side: Side) -> Self {
        let (mut svm, program_id, payer, solver) = setup_settle_ready();
        let hook = TransferHook::deploy(&mut svm);
        let hooked_mint = hook.create_mint(&mut svm, &payer);
        let other_mint = token::create_mint_under(
            &mut svm,
            &payer,
            &TokenProgram::Token2022.address(),
            Extensions::None,
        );
        let (sell, buy) = match hooked_side {
            Side::Sell => (hooked_mint, other_mint),
            Side::Buy => (other_mint, hooked_mint),
        };
        let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);
        Self {
            svm,
            program_id,
            payer,
            solver,
            hook,
            hooked_mint,
            order,
        }
    }

    fn settle(&mut self, extra_accounts: Side) -> Result<TransactionMetadata, TransactionError> {
        let hook_accounts = self.hook.extra_accounts(&self.hooked_mint);
        let (begin_extra_accounts, finalize_extra_accounts): (&[_], &[_]) = match extra_accounts {
            Side::Sell => (&hook_accounts, &[]),
            Side::Buy => (&[], &hook_accounts),
        };
        let instructions = settlement(
            &self.program_id,
            &self.solver.pubkey(),
            &self.order,
            &Checked {
                mints: &[self.hooked_mint],
                begin_extra_accounts,
                finalize_extra_accounts,
            },
        );
        send(&mut self.svm, &self.payer, &self.solver, &instructions)
    }
}

/// Which side of an order a hooked mint or the hook's extra accounts are on:
/// the sell side settles in `BeginSettle`, the buy side in `FinalizeSettle`.
#[derive(Clone, Copy)]
enum Side {
    Sell,
    Buy,
}

#[test]
fn transfer_hook_runs_on_the_extra_accounts() {
    for side in [Side::Sell, Side::Buy] {
        let mut hooked = HookedOrder::new(side);
        hooked
            .settle(side)
            .expect("the hook's accounts should let the transfer through");
        assert_settled(&hooked.svm, &hooked.order, AMOUNT);
    }
}

#[test]
fn transfer_hook_rejection_reverts_the_settlement() {
    for (side, index) in [(Side::Sell, BEGIN_INDEX), (Side::Buy, FINALIZE_INDEX)] {
        let mut hooked = HookedOrder::new(side);
        hooked.hook.flip_switch(&mut hooked.svm);
        assert_instruction_error_at(
            index,
            hooked.settle(side),
            InstructionError::Custom(REJECTED),
        );
    }
}

#[test]
fn transfer_hook_needs_the_extra_accounts_on_its_own_instruction() {
    // Each instruction's extra accounts reach only its own transfers, so the
    // hook's accounts on the other instruction leave the hooked one short.
    for (side, other, index) in [
        (Side::Sell, Side::Buy, BEGIN_INDEX),
        (Side::Buy, Side::Sell, FINALIZE_INDEX),
    ] {
        let mut hooked = HookedOrder::new(side);
        assert_instruction_error_at(
            index,
            hooked.settle(other),
            InstructionError::MissingAccount,
        );
    }
}

#[test]
fn rejects_a_mint_slot_that_isnt_a_mint() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);
    let checked = Checked {
        mints: &[sell, buy],
        ..Default::default()
    };

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
fn rejects_a_mint_of_the_other_token_program() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);
    let foreign = token::create_mint_under(
        &mut svm,
        &payer,
        &TokenProgram::Token2022.address(),
        Extensions::None,
    );

    let mut instructions = settlement(
        &program_id,
        &solver.pubkey(),
        &order,
        &Checked {
            mints: &[sell],
            ..Default::default()
        },
    );
    replace_first_matching_account(&mut instructions[0], &sell, foreign);
    assert_instruction_error(
        send(&mut svm, &payer, &solver, &instructions),
        SettlementError::InvalidMint,
    );
}

#[test]
fn token_program_rejects_a_mint_other_than_the_transferred_one() {
    let (mut svm, program_id, payer, solver) = setup_settle_ready();
    let sell = token::create_mint(&mut svm, &payer);
    let buy = token::create_mint(&mut svm, &payer);
    let order = staged_order(&mut svm, &program_id, &payer, &sell, &buy);
    let checked = Checked {
        mints: &[sell, buy],
        ..Default::default()
    };

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
