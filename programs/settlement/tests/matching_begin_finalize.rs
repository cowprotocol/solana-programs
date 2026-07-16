use litesvm::{types::FailedTransactionMetadata, LiteSVM};
use settlement_client::instructions::{BeginSettle, FinalizeSettle};
use settlement_client::settlement_interface::{SettlementError, SettlementInstruction};
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
            AbstractInstruction::Init(idx) => BeginSettle {
                program_id: *program_id,
                finalize_ix_index: *idx,
                orders: &[],
            }
            .into(),
            AbstractInstruction::Fin(idx) => FinalizeSettle {
                program_id: *program_id,
                begin_ix_index: *idx,
                orders: &[],
            }
            .into(),
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

    // Each case lists its instructions, the index of the instruction expected
    // to fail (the first one whose processing errors aborts the transaction),
    // and the exact error that instruction should raise.
    let cases: &[(&[AbstractInstruction], (u8, SettlementError))] = &[
        // Only init, pointing to itself
        (
            &[Other, Other, Init(2), Other, Other],
            (2, SettlementError::MismatchedCounterpartDiscriminator),
        ),
        // Only init, pointing to other.
        (
            &[Other, Other, Init(0), Other, Other],
            (2, SettlementError::CounterpartIsExternal),
        ),
        // Only fin, pointing to itself
        (
            &[Other, Other, Fin(2), Other, Other],
            (2, SettlementError::MismatchedCounterpartDiscriminator),
        ),
        // Only fin, pointing to other
        (
            &[Other, Other, Fin(0), Other, Other],
            (2, SettlementError::CounterpartIsExternal),
        ),
        // Init pointing at an index past the end of the transaction
        (
            &[Init(2), Fin(0)],
            (0, SettlementError::MissingCounterpartInstruction),
        ),
        // Fin before Init, but right matching.
        (
            &[Fin(1), Init(0)],
            (1, SettlementError::FinalizeBeforeInitialize),
        ),
        // A valid init/fin pair, plus an extra fin that points to init
        (
            &[Init(3), Other, Other, Fin(0), Other, Fin(0)],
            (5, SettlementError::MismatchedCounterpartDiscriminator),
        ),
        // A valid init/fin pair, plus an extra init that points to fin
        (
            &[Init(5), Other, Other, Init(5), Other, Fin(0)],
            (0, SettlementError::BeginFinalizePairOverlap),
        ),
        // A valid init/fin pair, plus an extra fin that points to init
        (
            &[Init(5), Other, Other, Fin(0), Other, Fin(0)],
            (0, SettlementError::BeginFinalizePairOverlap),
        ),
        // Two valid init/fin pairs, but one inside the other
        (
            &[Init(4), Init(3), Other, Fin(1), Fin(0)],
            (0, SettlementError::BeginFinalizePairOverlap),
        ),
        // Two valid init/fin pairs, distinct but overlapping
        (
            &[Init(3), Init(4), Other, Fin(0), Fin(1)],
            (0, SettlementError::BeginFinalizePairOverlap),
        ),
    ];

    let (mut svm, program_id, payer) = common::setup();
    for (sequence, (failing_index, expected)) in cases {
        let err = run_sequence(&mut svm, &program_id, &payer, sequence)
            .expect_err(&format!("expected {sequence:?} to fail, got Ok"));
        assert_eq!(
            err.err,
            TransactionError::InstructionError(*failing_index, to_instruction_error(*expected)),
            "expected {expected:?} at instruction {failing_index} for {sequence:?}"
        );
    }
}

/// Build a structurally-valid init/fin pair but swap `BeginSettle`'s sysvar
/// account for the payer's keypair. Pinocchio's `Instructions::try_from`
/// should reject the substitution with `UnsupportedSysvar` and revert the
/// transaction.
#[test]
fn rejects_non_instructions_sysvar_account_at_position_zero() {
    let (mut svm, program_id, payer) = common::setup();

    let mut begin: Instruction = BeginSettle {
        program_id,
        finalize_ix_index: 1,
        orders: &[],
    }
    .into();
    begin.accounts[0] = AccountMeta::new_readonly(payer.pubkey(), false);
    let finalize = FinalizeSettle {
        program_id,
        begin_ix_index: 0,
        orders: &[],
    }
    .into();

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
/// Structurally-valid `[Init(1), Fin(0)]` shape, but the `Fin(0)` slot is
/// filled with an instruction that has the same data shape as a begin/finalize
/// settlement instruction but `init.get_program_id() != program_id`.
fn rejects_counterpart_instruction_in_different_program() {
    let (mut svm, program_id, payer) = common::setup();

    let begin = BeginSettle {
        program_id,
        finalize_ix_index: 1,
        orders: &[],
    }
    .into();
    // We build a transaction that looks like a valid finalize_settle but
    // calling a different program. It doesn't really matter what program
    // we use here because execution isn't expected to reach this point.
    let stranger = FinalizeSettle {
        program_id: solana_system_interface::program::ID,
        begin_ix_index: 0,
        orders: &[],
    }
    .into();

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
        .expect_err("expected cross-program counterpart instruction to fail");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(
            expected_failing_instruction_index,
            to_instruction_error(SettlementError::CounterpartIsExternal),
        ),
        "expected CounterpartIsExternal at instruction {expected_failing_instruction_index}"
    );
}

/// Wrap a settlement `Instruction` as a call through the test CPI caller.
///
/// The CPI caller treats `accounts[0]` as the target program and `accounts[1..]`
/// as the accounts to forward, so `ix.program_id` becomes the first account and
/// `ix.accounts` are appended after it.
fn as_cpi_call(cpi_caller_id: Pubkey, ix: impl Into<Instruction>) -> Instruction {
    let ix = ix.into();
    let mut accounts = vec![AccountMeta::new_readonly(ix.program_id, false)];
    accounts.extend(ix.accounts);
    Instruction {
        program_id: cpi_caller_id,
        accounts,
        data: ix.data,
    }
}

/// Build a transaction that uses the test CPI caller to invoke `begin_settle`
/// via CPI.  The settlement program should reject it with `CalledViaCpi`.
#[test]
fn rejects_cpi_call_to_begin_settle() {
    let (mut svm, settlement_id, payer) = common::setup();
    let cpi_caller_id = common::setup_cpi_caller(&mut svm);

    let cpi_caller_ix = as_cpi_call(
        cpi_caller_id,
        BeginSettle {
            program_id: settlement_id,
            finalize_ix_index: 1,
            orders: &[],
        },
    );

    let tx = Transaction::new_signed_with_payer(
        &[cpi_caller_ix],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let err = svm
        .send_transaction(tx)
        .expect_err("CPI call to begin_settle should be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, to_instruction_error(SettlementError::CalledViaCpi)),
        "expected CalledViaCpi when begin_settle is called via CPI"
    );
}

/// Same as `rejects_cpi_call_to_begin_settle` but for `finalize_settle`.
#[test]
fn rejects_cpi_call_to_finalize_settle() {
    let (mut svm, settlement_id, payer) = common::setup();
    let cpi_caller_id = common::setup_cpi_caller(&mut svm);

    let cpi_caller_ix = as_cpi_call(
        cpi_caller_id,
        FinalizeSettle {
            program_id: settlement_id,
            begin_ix_index: 0,
            orders: &[],
        },
    );

    let tx = Transaction::new_signed_with_payer(
        &[cpi_caller_ix],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let err = svm
        .send_transaction(tx)
        .expect_err("CPI call to finalize_settle should be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, to_instruction_error(SettlementError::CalledViaCpi)),
        "expected CalledViaCpi when finalize_settle is called via CPI"
    );
}

/// Build a real `BeginSettle` whose counterpart slot holds an instruction of
/// this program but with empty data, so its discriminator can't be recovered.
/// The abstract harness can't express this: its `Init`/`Fin` always carry a
/// valid discriminator, and `Other` belongs to a different program.
#[test]
fn rejects_counterpart_with_unrecoverable_discriminator() {
    let (mut svm, program_id, payer) = common::setup();

    let begin = BeginSettle {
        program_id,
        finalize_ix_index: 1,
        orders: &[],
    };
    // Uses the settlement program, but no data: `recover_discriminator` fails
    // on the empty payload before any kind check can run.
    let malformed = Instruction {
        program_id,
        accounts: vec![],
        data: vec![],
    };

    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), malformed],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let err = svm
        .send_transaction(tx)
        .expect_err("expected counterpart with no discriminator to fail");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(
            0,
            to_instruction_error(SettlementError::InvalidCounterpartDiscriminator),
        ),
        "expected InvalidCounterpartDiscriminator at instruction 0"
    );
}

/// Build a real `BeginSettle` whose counterpart slot holds an instruction of
/// this program carrying a valid discriminator but no counterpart index, so
/// the back-pointer can't be recovered. Like the test above, this shape is
/// outside what the abstract harness can produce.
#[test]
fn rejects_counterpart_with_unrecoverable_counterpart_index() {
    let (mut svm, program_id, payer) = common::setup();

    let begin = BeginSettle {
        program_id,
        finalize_ix_index: 1,
        orders: &[],
    };
    // Same program as `begin`, with a valid discriminator but no trailing
    // counterpart-index bytes: `recover_discriminator` succeeds, then
    // `recover_counterpart` fails on the truncated remainder.
    let malformed = Instruction {
        program_id,
        accounts: vec![],
        data: vec![SettlementInstruction::FinalizeSettle.discriminator()],
    };

    let tx = Transaction::new_signed_with_payer(
        &[begin.into(), malformed],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );
    let err = svm
        .send_transaction(tx)
        .expect_err("expected counterpart with no counterpart index to fail");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(
            0,
            to_instruction_error(SettlementError::InvalidCounterpartCounterpart),
        ),
        "expected InvalidCounterpartCounterpart at instruction 0"
    );
}
