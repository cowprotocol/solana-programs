use settlement_client::settlement_interface::settle::{begin_settle, finalize_settle};
use solana_sdk::{signature::Signer, transaction::Transaction};

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
            begin_settle(&program_id, 1),
            finalize_settle(&program_id, 0),
        ],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );

    svm.send_transaction(tx)
        .expect("settlement instructions should succeed");
}
