use litesvm::{types::FailedTransactionMetadata, LiteSVM};
use settlement_client::settlement_interface::settle::{begin_settle, finalize_settle};
use settlement_client::settlement_interface::SettlementError;
use solana_sdk::{
    instruction::{AccountMeta, Instruction, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::instruction as system_instruction;

use crate::common::to_instruction_error;

mod common;

#[derive(Clone, Copy, Debug)]
/// Abstract representation of instructions in a settlement transaction.
enum AbstractInstruction {
    // BeginSettlement pointing at the specified Finalize instruction.
    Init(u16),
    // FinalizeSettlement pointing at the specified Begin instruction.
    Fin(u16),
    // Some other instruction.
    Other,
}

/// Based on a sequence of abstract instructions, creates an actual transaction
/// and see if it succeeds.
fn run_sequence(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    sequence: &[AbstractInstruction],
) -> Result<(), Box<FailedTransactionMetadata>> {
    let instructions: Vec<Instruction> = sequence
        .iter()
        .map(|spec| match spec {
            AbstractInstruction::Init(idx) => begin_settle(program_id, *idx),
            AbstractInstruction::Fin(idx) => finalize_settle(program_id, *idx),
            // 0-lamport self-transfer: a side-effect-free instruction that
            // (unlike Compute Budget) Solana allows to appear multiple times
            // in the same transaction.
            AbstractInstruction::Other => {
                system_instruction::transfer(&payer.pubkey(), &payer.pubkey(), 0)
            }
        })
        .collect();

    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).map(|_| ()).map_err(Box::new)
}

#[test]
fn valid_sequences() {
    use AbstractInstruction::*;

    let cases: &[&[AbstractInstruction]] = &[
        &[Init(1), Fin(0)],
        &[Init(4), Other, Other, Other, Fin(0)],
        &[Other, Init(2), Fin(1), Other, Other, Other],
        // Two settlement pairs in the same transaction are fine, as long as
        // they don't overlap.
        &[
            Other,
            Init(3),
            Other,
            Fin(1),
            Other,
            Other,
            Init(9),
            Other,
            Other,
            Fin(6),
        ],
    ];

    let (mut svm, program_id, payer) = common::setup();
    for sequence in cases {
        let result = run_sequence(&mut svm, &program_id, &payer, sequence);
        assert!(
            result.is_ok(),
            "expected {sequence:?} to succeed, got {result:?}"
        );
    }
}

#[test]
fn invalid_sequences() {
    use AbstractInstruction::*;

    let cases: &[&[AbstractInstruction]] = &[
        // Only init, pointing to itself
        &[Other, Other, Init(2), Other, Other],
        // Only init, pointing to other.
        &[Other, Other, Init(0), Other, Other],
        // Only fin, pointing to itself
        &[Other, Other, Fin(2), Other, Other],
        // Only fin, pointing to other
        &[Other, Other, Fin(0), Other, Other],
        // Fin before end, but right matching
        &[Fin(1), Init(0)],
        // A valid init/fin pair, plus an extra fin that points to init
        &[Init(3), Other, Other, Fin(0), Other, Fin(0)],
        // A valid init/fin pair, plus an extra init that points to fin
        &[Init(5), Other, Other, Init(5), Other, Fin(0)],
        // Two valid init/fin pairs, but one inside the other
        &[Init(4), Init(3), Other, Fin(1), Fin(0)],
        // Two valid init/fin pairs, distinct but overlapping
        &[Init(3), Init(4), Other, Fin(0), Fin(1)],
    ];

    let (mut svm, program_id, payer) = common::setup();
    for sequence in cases {
        let result = run_sequence(&mut svm, &program_id, &payer, sequence);
        assert!(result.is_err(), "expected {sequence:?} to fail, got Ok");
    }
}

/// Build a structurally-valid init/fin pair but swap `BeginSettle`'s sysvar
/// account for the payer's keypair. Pinocchio's `Instructions::try_from`
/// should reject the substitution with `UnsupportedSysvar` and revert the
/// transaction.
#[test]
fn rejects_non_sysvar_account_at_position_zero() {
    let (mut svm, program_id, payer) = common::setup();

    let mut begin = begin_settle(&program_id, 1);
    begin.accounts[0] = AccountMeta::new_readonly(payer.pubkey(), false);
    let finalize = finalize_settle(&program_id, 0);

    let tx = Transaction::new_signed_with_payer(
        &[begin, finalize],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let err = svm
        .send_transaction(tx)
        .expect_err("expected non-sysvar account to fail");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::UnsupportedSysvar),
        "expected UnsupportedSysvar at instruction 0"
    );
}

#[test]
/// Structurally-valid `[Fin(1), Init(0)]` shape, but the `Init(0)` slot is
/// filled with an instruction that has the same data shape as a begin/finalize
/// settlement instruction but `init.get_program_id() != program_id`.
fn rejects_paired_instruction_in_different_program() {
    let (mut svm, program_id, payer) = common::setup();

    let begin = begin_settle(&program_id, 1);
    // We build a transaction that looks like a valid finalize_settle but
    // calling a different program. It doesn't really matter what program
    // we use here because execution isn't expected to reach this point.
    let stranger = finalize_settle(&solana_system_interface::program::ID, 0);

    let instructions = [begin, stranger];
    let expected_failing_instruction_index = 0;
    let tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let err = svm
        .send_transaction(tx)
        .expect_err("expected cross-program paired instruction to fail");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(
            expected_failing_instruction_index,
            to_instruction_error(SettlementError::MismatchingSettlePair),
        ),
        "expected MismatchingSettlePair at instruction {expected_failing_instruction_index}"
    );
}
