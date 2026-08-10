//! Compute-unit benchmarking helpers for the settlement integration tests.

use litesvm::{types::TransactionResult, LiteSVM};
use solana_sdk::transaction::VersionedTransaction;
use std::{env, fs, io::Write};

/// Where `send_transaction_metered` accumulates its measurements: a directory
/// of per-process JSON Lines shards, which `just bench` merges into
/// `target/cu-report.json` once every test binary has exited.
const CU_SHARD_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/cu-report");

/// Wraps svm.send_transaction and captures the compute units consumed by
/// `program_id`'s own execution. The measured usage is recorded to a JSON
/// file at the key specified by `label`.
///
/// Only CUs are captured--not rent allocation/deallocation.
#[allow(
    clippy::result_large_err,
    reason = "mirrors litesvm::LiteSVM::send_transaction's own return type, which we don't control"
)]
#[track_caller]
pub fn send_transaction_metered(
    svm: &mut LiteSVM,
    tx: impl Into<VersionedTransaction>,
    label: &str,
) -> TransactionResult {
    let tx = tx.into();

    let result = svm.send_transaction(tx.clone());

    // only write benchmarks if `TEST_BENCHMARK` is set to something
    if env::var("TEST_BENCHMARK").is_ok() {
        record_compute_units(CU_SHARD_DIR, label, tx, &result);
    }

    result
}

fn shard_path(dir: &str, label: &str) -> String {
    let sanitised_label = label.replace("/", "_");
    format!("{dir}/{sanitised_label}.jsonl")
}

/// Accounts the transaction locks, as `(readable, writable)`: the static keys
/// plus every address resolved through an Address Lookup Table.
///
/// `static_account_keys` alone undercounts an ALT-backed transaction, because
/// the runtime forbids table-loading the fee payer, the other signers, and the
/// program id -- those are all that stays static, so the accounts the
/// instruction actually operates on are invisible there. Note that the table
/// account itself is not among them, matching the runtime's own lock accounting
/// for `TooManyAccountLocks`.
///
/// Static keys carry no per-key flags: the list is ordered by privilege
/// (writable signers, readonly signers, writable non-signers, readonly
/// non-signers) and the header counts only the two readonly runs, so the
/// writable ones are the remainder. Table-loaded addresses arrive already split
/// across the lookup's two index lists.
fn accounts_locked(tx: &VersionedTransaction) -> (usize, usize) {
    let header = tx.message.header();
    let readable_static = usize::from(header.num_readonly_signed_accounts)
        .strict_add(usize::from(header.num_readonly_unsigned_accounts));
    let writable_static = tx
        .message
        .static_account_keys()
        .len()
        .strict_sub(readable_static);

    tx.message
        .address_table_lookups()
        .unwrap_or_default()
        .iter()
        .fold(
            (readable_static, writable_static),
            |(readable, writable), lookup| {
                (
                    readable.strict_add(lookup.readonly_indexes.len()),
                    writable.strict_add(lookup.writable_indexes.len()),
                )
            },
        )
}

fn record_compute_units(
    dir: &str,
    label: &str,
    tx: VersionedTransaction,
    result: &TransactionResult,
) {
    let (accounts_readable, accounts_writable) = accounts_locked(&tx);
    let ix_bytes_required: usize = tx
        .message
        .instructions()
        .iter()
        .map(|ix| ix.data.len())
        .sum();

    let compute_units_consumed = match &result {
        Ok(meta) => meta.compute_units_consumed,
        Err(failed) => failed.meta.compute_units_consumed,
    };

    fs::create_dir_all(dir)
        .unwrap_or_else(|e| panic!("failed to create CU shard directory at {dir}: {e}"));
    let path = shard_path(dir, label);

    // Newline included in the same buffer: it's the single write that's atomic,
    // so terminating the line separately would reintroduce interleaving.
    let line = format!("{{\"label\": \"{label}\", \"accounts_readable\": {accounts_readable}, \"accounts_writable\": {accounts_writable}, \"instruction_bytes\": {ix_bytes_required}, \"compute_units\": {compute_units_consumed}}}");

    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("failed to open benchmarking shard at {path}: {e}"))
        .write_all(line.as_bytes())
        .unwrap_or_else(|e| panic!("failed to write to benchmarking shard at {path}: {e}"));
}
