//! Shared scaffolding for the settlement integration tests.

#![allow(
    dead_code,
    reason = "integration tests compile as separate crates, so items only used by a subset of the test binaries look dead to the others"
)]

use litesvm::LiteSVM;
use settlement_client::settlement_interface::SettlementError;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

pub const PROGRAM_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/cow_settlement.so"
);

/// Spin up a `LiteSVM`, deploy the compiled `settlement.so` under a freshly
/// generated program ID, and airdrop a payer keypair.
pub fn setup() -> (LiteSVM, Pubkey, Keypair) {
    let mut svm = LiteSVM::new();
    let program_id = Pubkey::new_unique();
    svm.add_program_from_file(program_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000)
        .expect("airdrop to payer should succeed");

    (svm, program_id, payer)
}

/// Wrap a `SettlementError` in the runtime-side `InstructionError::Custom`
/// shape that the validator records and `TransactionError::InstructionError`
/// carries. The cross-crate conversion isn't provided by the interface, so
/// tests asserting on a failed instruction's error code use this helper.
///
/// This is mostly here to make the one-way relationship between the two more
/// explicit.
pub fn to_instruction_error(e: SettlementError) -> InstructionError {
    InstructionError::Custom(e.into())
}
