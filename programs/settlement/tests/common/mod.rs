//! Shared scaffolding for the settlement integration tests.

#![allow(
    dead_code,
    reason = "integration tests compile as separate crates, so items only used by a subset of the test binaries look dead to the others"
)]
pub mod active_token;
pub mod benchmark;
pub mod buffer;
pub mod lookup_table;
pub mod order;
pub mod pda;
pub mod settlement;
pub mod state;
pub mod token;
pub mod token_2022;

#[allow(
    unused_imports,
    reason = "re-exported for the suites that use the macro; the others never name it"
)]
pub(crate) use active_token::also_under_token_2022;

use cow_settlement_client::instruction::{AddSolver, Initialize};
use cow_settlement_interface::pda::state::find_state_pda;
use cow_settlement_interface::Instruction;
use cow_settlement_interface::SettlementError;
use litesvm::{types::TransactionMetadata, LiteSVM};
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use std::cell::Cell;

pub const PROGRAM_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/cow_settlement.so"
);

pub const CPI_CALLER_SO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/deploy/test_cpi_caller.so"
);

thread_local! {
    /// Counter behind [`unique_pubkey`] and [`unique_keypair`], reset by
    /// [`setup`]. Needs to be thread local because parallel executed tests would share the same memory cell
    static NEXT_SEED: Cell<u64> = const { Cell::new(0) };
}

fn next_seed() -> [u8; 32] {
    let n = NEXT_SEED.with(|next| {
        let n = next.get();
        next.set(n.wrapping_add(1));
        n
    });
    solana_sha256_hasher::hashv(&[b"settlement tests seed", &n.to_le_bytes()]).to_bytes()
}

/// A deterministic stand-in for `Pubkey::new_unique`, which is banned in these
/// tests (see `clippy.toml`).
pub fn unique_pubkey() -> Pubkey {
    Pubkey::new_from_array(next_seed())
}

/// A deterministic stand-in for `Keypair::new`, which is banned in these tests
/// (see `clippy.toml`). `Keypair::new` draws from the OS random source
pub fn unique_keypair() -> Keypair {
    Keypair::new_from_array(next_seed())
}

/// Spin up a `LiteSVM`, deploy the compiled `settlement.so` under a freshly
/// generated program ID, and airdrop a payer keypair.
pub fn setup() -> (LiteSVM, Pubkey, Keypair) {
    let mut svm = LiteSVM::new();
    let program_id = unique_pubkey();
    svm.add_program_from_file(program_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

    let payer = unique_keypair();
    svm.airdrop(&payer.pubkey(), 1_000_000_000)
        .expect("airdrop to payer should succeed");

    (svm, program_id, payer)
}

/// A settlement initialized by [`setup_init`], with the manager and
/// reclaim authority held as keypairs the test can sign transfers with.
pub struct InitializedParams {
    pub program_id: Pubkey,
    pub payer: Keypair,
    pub state_pda: Pubkey,
    pub manager: Keypair,
    pub reclaim: Keypair,
}

/// [`setup`] followed by a successful `Initialize` whose manager and reclaim
/// authority are keypairs the test controls, so it can sign on their behalf.
///
/// Returns the SVM and an [`InitializedParams`] bundling the program id, the
/// fee payer, the state PDA, and the manager and reclaim authority keypairs.
pub fn setup_init() -> (LiteSVM, InitializedParams) {
    let (mut svm, program_id, payer) = setup();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let manager = unique_keypair();
    let reclaim = unique_keypair();
    state::initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            manager: manager.pubkey(),
            reclaim_authority: reclaim.pubkey(),
        },
    );

    (
        svm,
        InitializedParams {
            program_id,
            payer,
            state_pda,
            manager,
            reclaim,
        },
    )
}

/// Register `solver` in the state PDA's solver list, authorized by the manager
/// and paid by the fee payer.
pub fn register_solver(svm: &mut LiteSVM, params: &InitializedParams, solver: &Pubkey) {
    let tx = Transaction::new_signed_with_payer(
        &[AddSolver {
            program_id: params.program_id,
            manager: params.manager.pubkey(),
            payer: params.payer.pubkey(),
            solver: *solver,
        }
        .into()],
        Some(&params.payer.pubkey()),
        &[&params.payer, &params.manager],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("registering a solver should succeed");
}

/// [`setup_init`] plus a freshly registered, funded `solver`, returned alongside
/// the fee `payer`. `payer` funds the test's setup transactions (creating orders,
/// funding buffers). `solver` authorizes settlements and must sign them; it is
/// airdropped so it can submit and pay for the settlement itself.
pub fn setup_settle_ready() -> (LiteSVM, Pubkey, Keypair, Keypair) {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair();
    register_solver(&mut svm, &params, &solver.pubkey());
    svm.airdrop(&solver.pubkey(), 1_000_000_000)
        .expect("airdrop to solver should succeed");
    let InitializedParams {
        program_id, payer, ..
    } = params;
    (svm, program_id, payer, solver)
}

/// Adds CPI caller test helper to the given SVM
pub fn setup_cpi_caller(svm: &mut LiteSVM) -> Pubkey {
    let cpi_caller_id = unique_pubkey();
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

/// Assert that the transaction failed with `expected` on its first
/// instruction. Use [`assert_instruction_error_at`] when the failing
/// instruction isn't the first one.
#[track_caller]
pub fn assert_instruction_error<T>(
    result: Result<T, TransactionError>,
    expected: InstructionError,
) {
    assert_instruction_error_at(0, result, expected);
}

#[track_caller]
pub fn assert_instruction_error_at<T>(
    ix_idx: u8,
    result: Result<T, TransactionError>,
    expected: InstructionError,
) {
    assert_eq!(
        result.err(),
        Some(TransactionError::InstructionError(ix_idx, expected))
    );
}

/// Convenience wrapper around [`assert_instruction_error_at`] for asserting a
/// specific [`SettlementError`] at the instruction that produced it: settlements
/// run as a `[BeginSettle, FinalizeSettle]` pair, so the failing instruction
/// isn't always the first.
#[track_caller]
pub fn assert_settlement_error<T>(
    ix_idx: u8,
    result: Result<T, TransactionError>,
    expected: SettlementError,
) {
    assert_instruction_error_at(ix_idx, result, to_instruction_error(expected));
}

pub fn create_account_at(svm: &mut LiteSVM, address: Pubkey, owner: &Pubkey, data: &[u8]) {
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
}

/// Place a fresh, rent-exempt account holding `data` and owned by `owner` at a
/// new address, and return it. Lets a test populate an arbitrary account (e.g.
/// program-owned, with a crafted body or a deliberately wrong size or owner)
/// directly, bypassing the runtime.
pub fn create_account(svm: &mut LiteSVM, owner: &Pubkey, data: &[u8]) -> Pubkey {
    let address = unique_pubkey();
    create_account_at(svm, address, owner, data);
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
#[track_caller]
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
    let mut instructions = [ix.into()];
    active_token::retarget(&mut instructions);
    Transaction::new_signed_with_payer(
        &instructions,
        Some(&fee_payer.pubkey()),
        &[fee_payer, owner],
        svm.latest_blockhash(),
    )
}

/// In `instruction`, repoint the account currently set to `from` at `to`. Tests
/// use it to corrupt one account of an otherwise-valid instruction; it panics if
/// `instruction` doesn't reference `from`, so a stale swap fails loudly rather
/// than silently testing nothing.
#[track_caller]
pub fn replace_first_matching_account(instruction: &mut Instruction, from: &Pubkey, to: Pubkey) {
    let meta = instruction
        .accounts
        .iter_mut()
        .find(|meta| meta.pubkey == *from)
        .unwrap_or_else(|| panic!("instruction should reference {from}"));
    meta.pubkey = to;
}

/// Assemble `instructions` into a transaction with `payer` as fee payer, signed
/// by `payer` and `additional_signers`, and aimed at the active token program.
///
/// Private, so every test submits through [`send`] or [`send_metered`] and a
/// metered test measures exactly the transaction its unmetered twin sends.
fn assemble_tx(
    svm: &LiteSVM,
    payer: &Keypair,
    additional_signers: &[&Keypair],
    instructions: &[Instruction],
) -> Transaction {
    let mut instructions = Vec::from(instructions);
    let mut signers = vec![payer];
    signers.extend_from_slice(additional_signers);
    active_token::retarget(&mut instructions);
    Transaction::new_signed_with_payer(
        &instructions,
        Some(&payer.pubkey()),
        &signers,
        svm.latest_blockhash(),
    )
}

/// Assemble `instructions` into a transaction signed by `payer` and submit it,
/// surfacing only the transaction-level error on failure (dropping the success
/// metadata's error wrapper).
#[track_caller]
pub fn send(
    svm: &mut LiteSVM,
    payer: &Keypair,
    instructions: &[Instruction],
) -> Result<TransactionMetadata, TransactionError> {
    send_with_signers(svm, payer, &[], instructions)
}

/// [`send`], with `additional_signers` signing alongside `payer`, for the
/// instructions that need a signature from an account other than the payer (a
/// freshly created mint or token account signing for its own allocation).
#[track_caller]
pub fn send_with_signers(
    svm: &mut LiteSVM,
    payer: &Keypair,
    additional_signers: &[&Keypair],
    instructions: &[Instruction],
) -> Result<TransactionMetadata, TransactionError> {
    let tx = assemble_tx(svm, payer, additional_signers, instructions);
    svm.send_transaction(tx).map_err(|failed| failed.err)
}

/// [`send`], metered: submits the very same transaction and records it under
/// `label`. Lets a test that assembles a multi-instruction transaction (a
/// `[BeginSettle, FinalizeSettle]` pair) be benchmarked without restating how
/// that transaction is built.
#[track_caller]
pub fn send_metered(
    svm: &mut LiteSVM,
    payer: &Keypair,
    instructions: &[Instruction],
    label: benchmark::BenchLabel,
) -> Result<TransactionMetadata, TransactionError> {
    let tx = assemble_tx(svm, payer, &[], instructions);
    benchmark::send_transaction_metered(svm, tx, label).map_err(|failed| failed.err)
}
