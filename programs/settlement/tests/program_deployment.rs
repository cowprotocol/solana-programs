use litesvm::LiteSVM;
use settlement_client::{begin_settle, finalize_settle};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const PROGRAM_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/settlement.so"
);

#[test]
fn program_is_deployed_with_code() {
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

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
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000)
        .expect("airdrop to payer should succeed");

    let tx = Transaction::new_signed_with_payer(
        &[begin_settle(&program_id), finalize_settle(&program_id)],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );

    svm.send_transaction(tx)
        .expect("settlement instructions should succeed");
}
