//! Shared scaffolding for the settlement integration tests.

#![allow(
    dead_code,
    reason = "integration tests compile as separate crates, so items only used by a subset of the test binaries look dead to the others"
)]

pub mod benchmark;
pub mod buffer;
pub mod lookup_table;
pub mod order;
pub mod pda;
pub mod token;

use litesvm::LiteSVM;
use settlement_client::settlement_interface::SettlementError;
use settlement_interface::Instruction;
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

pub const PROGRAM_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/cow_settlement.so"
);

pub const CPI_CALLER_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/test_cpi_caller.so"
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

/// Adds CPI caller test helper to the given SVM
pub fn setup_cpi_caller(svm: &mut LiteSVM) -> Pubkey {
    let cpi_caller_id = Pubkey::new_unique();
    svm.add_program_from_file(cpi_caller_id, CPI_CALLER_SO)
        .expect("test-cpi-caller .so not found, run `just build-program` first");
    cpi_caller_id
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

pub fn assert_instruction_error<T>(
    result: Result<T, TransactionError>,
    expected: InstructionError,
) {
    assert_eq!(
        result.err(),
        Some(TransactionError::InstructionError(0, expected))
    );
}
pub fn assert_settlement_error<T>(result: Result<T, TransactionError>, expected: SettlementError) {
    assert_instruction_error(result, to_instruction_error(expected));
}

/// Place a fresh, rent-exempt account holding `data` and owned by `owner` at a
/// new address, and return it. Lets a test populate an arbitrary account (e.g.
/// program-owned, with a crafted body or a deliberately wrong size or owner)
/// directly, bypassing the runtime.
pub fn create_account(svm: &mut LiteSVM, owner: &Pubkey, data: &[u8]) -> Pubkey {
    let address = Pubkey::new_unique();
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        address,
        Account {
            lamports,
            data: data.to_vec(),
            owner: *owner,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("set_account should succeed");
    address
}

pub fn set_unix_timestamp(svm: &mut LiteSVM, unix_timestamp: i64) {
    let mut clock = svm.get_sysvar::<Clock>();
    clock.unix_timestamp = unix_timestamp;
    svm.set_sysvar::<Clock>(&clock);
}

/// Read the lamports balance of an account, or 0 if the account doesn't
/// exist.
pub fn lamports(svm: &LiteSVM, address: &Pubkey) -> u64 {
    svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
}

/// Assert that `account` holds exactly the rent-exempt minimum for its current
/// data size. The size is taken from `account.data` rather than passed in, so
/// the check can't drift from the account it's checking.
pub fn assert_rent_exempt(svm: &LiteSVM, account: &Account) {
    let rent = svm.minimum_balance_for_rent_exemption(account.data.len());
    assert_eq!(
        account.lamports, rent,
        "account must hold exactly its rent-exempt minimum",
    );
}

/// Sign `ix` with `fee_payer` as the transaction fee payer and
/// `owner` as the keypair filling the `owner` slot. Tests pass
/// two distinct keypairs to keep these roles independent.
pub fn signed_tx(
    svm: &LiteSVM,
    fee_payer: &Keypair,
    owner: &Keypair,
    ix: impl Into<Instruction>,
) -> Transaction {
    Transaction::new_signed_with_payer(
        &[ix.into()],
        Some(&fee_payer.pubkey()),
        &[fee_payer, owner],
        svm.latest_blockhash(),
    )
}
