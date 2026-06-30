use settlement_client::instructions::{BeginSettle, FinalizeSettle, InstructionBuilding};
use solana_sdk::{
    instruction::{Instruction, InstructionError},
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

mod common;

#[test]
fn program_is_deployed_with_code() {
    let (svm, program_id, _) = common::setup();

    let account = svm
        .get_account(&program_id)
        .expect("program account should exist after loading");
    assert!(
        account.executable,
        "program account should be marked executable"
    );
    assert!(!account.data.is_empty(), "program account should hold code");
}

#[test]
fn program_can_be_invoked() {
    let (mut svm, program_id, payer) = common::setup();

    let tx = Transaction::new_signed_with_payer(
        // Indices encode the BeginSettle/FinalizeSettle pair
        // `Begin` at 0 → finalize_ix=1, `Finalize` at 1 → begin_ix=0.
        &[
            BeginSettle {
                program_id,
                finalize_ix_index: 1,
                orders: &[],
            }
            .instruction(),
            FinalizeSettle {
                program_id,
                begin_ix_index: 0,
            }
            .instruction(),
        ],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );

    svm.send_transaction(tx)
        .expect("settlement instructions should succeed");
}
#[test]
fn rejects_transaction_with_unsupported_discriminator() {
    let (mut svm, program_id, payer) = common::setup();

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id,
            accounts: vec![],
            data: vec![
                0x42, // Invalid discriminator
            ],
        }],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );

    let err = svm
        .send_transaction(tx)
        .expect_err("Transaction with invalid discriminator should be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::InvalidInstructionData),
    );
}
