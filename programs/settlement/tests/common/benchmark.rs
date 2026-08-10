//! Compute-unit benchmarking helpers for the settlement integration tests.

use litesvm::{
    types::{TransactionMetadata, TransactionResult},
    LiteSVM,
};
use settlement_interface::Instruction;
use solana_sdk::{
    signature::Keypair,
    transaction::{TransactionError, VersionedTransaction},
};
use std::{cell::Cell, env, fs, io::Write};

/// Where `send_transaction_metered` accumulates its measurements: a directory
/// of per-process JSON Lines shards, which `just bench` merges into
/// `target/cu-report.json` once every test binary has exited.
const CU_SHARD_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/cu-report");

/// Declare a `#[test]` whose transactions may be metered. The test's own name
/// becomes the first half of every label it records (the other half being supplied as `label` to `_metered` functions below)
#[macro_export]
macro_rules! bench_test {
    ($(#[$attr:meta])* $name:ident, $body:block) => {
        $(#[$attr])*
        #[test]
        // Clippy exempts test functions from this restriction lint, but stops
        // recognising them as tests once the `#[test]` comes out of a macro, so
        // the exemption has to be restated for `bench_test!` to stay a drop-in
        // replacement for `#[test]`.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "an overflow in a test's own bookkeeping should panic, which is what these tests want anyway"
        )]
        fn $name() {
            $crate::common::benchmark::set_test_name(stringify!($name));
            $body
        }
    };
}

thread_local! {
    /// The enclosing [`bench_test`]'s name, qualifying every label that test
    /// records. Thread local because libtest runs each test on its own thread,
    /// which is also what keeps concurrently running tests from seeing each
    /// other's name.
    static TEST_NAME: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Register the enclosing test's name. An implementation detail of
/// [`bench_test`], which is the only thing that should call it.
pub fn set_test_name(name: &'static str) {
    TEST_NAME.set(Some(name));
}

/// Qualify `label` with the enclosing test's name, panicking if the test wasn't
/// declared with [`bench_test`]. Evaluated whether or not `TEST_BENCHMARK` is
/// set, so a plain `just test` already catches the missing declaration.
#[track_caller]
fn qualified_label(label: &str) -> String {
    let test_name = TEST_NAME.get().unwrap_or_else(|| {
        panic!("metering `{label}` requires the enclosing test to be declared with `bench_test!`")
    });
    format!("{test_name}/{label}")
}

/// Wraps svm.send_transaction and captures the compute units consumed by
/// `program_id`'s own execution. The measured usage is recorded to a JSON
/// file under the enclosing [`bench_test`]'s name joined with `label`.
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
    let label = qualified_label(label);

    let result = svm.send_transaction(tx.clone());

    // only write benchmarks if `TEST_BENCHMARK` is set to something
    if env::var("TEST_BENCHMARK").is_ok() {
        record_compute_units(CU_SHARD_DIR, &label, tx, &result);
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
    label: &str,
) -> Result<TransactionMetadata, TransactionError> {
    let tx = super::payer_signed_tx(svm, payer, instructions);
    send_transaction_metered(svm, tx, label).map_err(|failed| failed.err)
}

fn shard_path(dir: &str, label: &str) -> String {
    let sanitised_label = label.replace("/", "_");
    format!("{dir}/{sanitised_label}.jsonl")
}

/// Accounts the transaction locks, as `(readable, writable)`: the static keys
/// plus every address resolved through an Address Lookup Table.
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
