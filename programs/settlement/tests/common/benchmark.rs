//! Compute-unit benchmarking helpers for the settlement integration tests.

use cow_settlement_interface::Instruction;
use litesvm::{
    types::{TransactionMetadata, TransactionResult},
    LiteSVM,
};
use solana_sdk::{
    message::v0::MessageAddressTableLookup,
    signature::Keypair,
    transaction::{TransactionError, VersionedTransaction},
};
use std::{env, fmt, fs, io::Write, thread};

/// The kind of transaction a metered test measures. Naming a measurement with a
/// variant rather than a free-form string keeps the same transaction spelled the
/// same way across tests, so `bench-report.json` stays comparable between runs.
#[derive(Debug, Clone, Copy)]
pub enum BenchLabel {
    Initialize,
    CreateOrder,
    CreateBuffers,
    ReclaimBuffer,
    ReclaimOrder,
    Settle,
    TransferAuthority,
    AddSolver,
}

impl fmt::Display for BenchLabel {
    /// Spelled out rather than derived from [`Debug`], so the report keeps the
    /// word boundaries a lower-cased `CreateOrder` would lose.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Initialize => "initialize",
            Self::CreateOrder => "create_order",
            Self::CreateBuffers => "create_buffers",
            Self::ReclaimBuffer => "reclaim_buffer",
            Self::ReclaimOrder => "reclaim_order",
            Self::Settle => "settle",
            Self::TransferAuthority => "transfer_authority",
            Self::AddSolver => "add_solver",
        })
    }
}

/// Where `send_transaction_metered` writes its measurements: one single-record
/// file per `<label>/<test>`, which `just bench` merges into `bench-report.json`.
const BENCH_REPORT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/bench-report");

/// Qualify `label` with the name of the test recording it, so a test only has to
/// name the transaction it measures and two tests measuring the same kind of
/// transaction can't overwrite each other's shard.
///
/// The test name comes from the name of the thread, as rust's testing framework
/// names it as such.
fn qualified_label(label: BenchLabel) -> String {
    let thread = thread::current();
    let test_name = thread.name().unwrap_or_else(|| {
        panic!("could not read the test name from thread name for benchmarking")
    });
    format!("{label}/{test_name}")
}

/// Wraps [`LiteSVM::send_transaction`] and, when `TEST_BENCHMARK=true`, records
/// the transaction's compute units, locked accounts, and serialised size under
/// `<label>/<test name>`. Rent allocation/deallocation is not measured.
///
/// A reverted transaction still has its accounts and bytes recorded, since those
/// describe the transaction as submitted. Its compute units are recorded as null:
/// execution stopped partway, so the tally is partial.
///
/// This function will panic if the output file already exists. This is designed
/// to prevent accidental reuse of a label, which would otherwise silently
/// overwrite the earlier measurement.
#[allow(
    clippy::result_large_err,
    reason = "mirrors litesvm::LiteSVM::send_transaction's own return type, which we don't control"
)]
#[track_caller]
pub fn send_transaction_metered(
    svm: &mut LiteSVM,
    tx: impl Into<VersionedTransaction>,
    label: BenchLabel,
) -> TransactionResult {
    let tx = tx.into();
    let label = qualified_label(label);

    let result = svm.send_transaction(tx.clone());

    // only write benchmarks if `TEST_BENCHMARK` is set to true
    if let Ok(flag_value) = env::var("TEST_BENCHMARK") {
        if flag_value.to_lowercase() == "true" {
            record_benchmark(BENCH_REPORT_DIR, &label, tx, &result);
        }
    }

    result
}

/// [`super::send`], metered: submits the very same transaction and records it
/// under `label`. Lets a test that assembles a multi-instruction transaction
/// (a `[BeginSettle, FinalizeSettle]` pair) be benchmarked without restating
/// how that transaction is built.
#[track_caller]
pub fn send_metered(
    svm: &mut LiteSVM,
    payer: &Keypair,
    instructions: Vec<Instruction>,
    label: BenchLabel,
) -> Result<TransactionMetadata, TransactionError> {
    let tx = super::payer_signed_tx(svm, payer, instructions);
    send_transaction_metered(svm, tx, label).map_err(|failed| failed.err)
}

fn shard_path(dir: &str, label: &str) -> String {
    let sanitised_label = label.replace("/", "_");
    format!("{dir}/{sanitised_label}.jsonl")
}

/// Returns the total number of accounts locked, including accounts from ALTs
fn accounts_locked(tx: &VersionedTransaction) -> usize {
    let locked_static = tx.message.static_account_keys().len();

    tx.message
        .address_table_lookups()
        .unwrap_or_default()
        .iter()
        .fold(
            locked_static,
            |locked,
             MessageAddressTableLookup {
                 // The lookup table's own account isn't one of the
                 // transaction's locked keys, it contributes to neither tally.
                 account_key: _,
                 writable_indexes,
                 readonly_indexes,
             }| {
                locked
                    .strict_add(readonly_indexes.len())
                    .strict_add(writable_indexes.len())
            },
        )
}

/// The transaction's size on the wire: signatures, header, account keys, lookup
/// tables and instruction data, which together have to fit in a 1232-byte
/// packet.
fn transaction_bytes(tx: &VersionedTransaction) -> u64 {
    bincode::serialized_size(tx)
        .unwrap_or_else(|e| panic!("failed to measure the serialised transaction size: {e}"))
}

fn record_benchmark(dir: &str, label: &str, tx: VersionedTransaction, result: &TransactionResult) {
    let total_accounts = accounts_locked(&tx);
    let transaction_bytes = transaction_bytes(&tx);

    // A revert stops execution partway, so the consumed count is a partial tally
    // rather than the cost of the transaction: report it as null instead.
    let compute_units_consumed = match &result {
        Ok(meta) => meta.compute_units_consumed.to_string(),
        Err(_) => "null".to_owned(),
    };

    fs::create_dir_all(dir)
        .unwrap_or_else(|e| panic!("failed to create benchmark report directory at {dir}: {e}"));
    let path = shard_path(dir, label);

    let line = format!("{{\"label\": \"{label}\", \"accounts\": {total_accounts}, \"transaction_bytes\": {transaction_bytes}, \"compute_units\": {compute_units_consumed}}}");

    fs::OpenOptions::new()
        // Fails if the shard already exists
        .create_new(true)
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| {
            panic!("failed to create benchmarking data at {path} (has the label been reused?): {e}")
        })
        .write_all(line.as_bytes())
        .unwrap_or_else(|e| panic!("failed to write to benchmarking data at {path}: {e}"));
}
