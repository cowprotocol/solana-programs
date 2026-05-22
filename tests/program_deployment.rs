use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const PROGRAM_SO: &str = "target/deploy/cow_solana_programs.so";

#[test]
fn program_is_deployed_with_code() {
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build` first");

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
        .expect("compiled program .so not found, run `just build` first");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000)
        .expect("airdrop to payer should succeed");

    let ix = Instruction {
        program_id,
        accounts: vec![],
        data: vec![],
    };
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        svm.latest_blockhash(),
    );

    svm.send_transaction(tx)
        .expect("no-op invocation should succeed");
}
