//! Compute-unit benchmarking helpers for the settlement integration tests.

use litesvm::{
    types::{TransactionMetadata, TransactionResult},
    LiteSVM,
};
use settlement_interface::{data::intent::OrderKind, Instruction};
use solana_sdk::{
    message::{v0::MessageAddressTableLookup, MessageHeader},
    signature::Keypair,
    transaction::{TransactionError, VersionedTransaction},
};
use std::{env, fmt, fs, io::Write, thread};

/// The kind of transaction a metered test measures. Naming a measurement with a
/// variant rather than a free-form string keeps the same transaction spelled the
/// same way across tests, so `bench-report.json` stays comparable between runs.
///
/// A variant only has to be unique within one test, since [`qualified_label`]
/// appends the test's name. Two transactions of the same kind in one test
/// therefore need distinct variants: that is what [`Self::RecreateAccount`] and
/// [`Self::SettleAgain`] are for.
#[derive(Debug, Clone, Copy)]
pub enum BenchLabel {
    Initialize,
    CreateOrder,
    CreateBuffers,
    ReclaimOrder,
    RecreateAccount,
    Settle,
    SettleDuplicate,
    SettleRemainingFill,
    SettleMixedKinds {
        generous: OrderKind,
        violating: OrderKind,
    },
    NonCanonicalPda,
    UnsupportedDiscriminator,
    CpiCall,
}

impl fmt::Display for BenchLabel {
    /// Spelled out rather than derived from [`Debug`], so the report keeps the
    /// word boundaries a lower-cased `CreateOrder` would lose.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /// Named separately from the variants because it spells a payload, not
        /// a transaction kind.
        fn kind(kind: OrderKind) -> &'static str {
            match kind {
                OrderKind::Sell => "sell",
                OrderKind::Buy => "buy",
            }
        }

        match self {
            Self::Initialize => f.write_str("initialize"),
            Self::CreateOrder => f.write_str("create_order"),
            Self::CreateBuffers => f.write_str("create_buffers"),
            Self::ReclaimOrder => f.write_str("reclaim_order"),
            Self::RecreateAccount => f.write_str("recreate_account"),
            Self::NonCanonicalPda => f.write_str("non_canonical_pda_reverting"),
            Self::CpiCall => f.write_str("cpi_call_reverting"),
            Self::UnsupportedDiscriminator => f.write_str("unsupported_discriminator_reverting"),
            Self::Settle => f.write_str("settle"),
            Self::SettleDuplicate => f.write_str("settle_duplicate_reverting"),
            Self::SettleRemainingFill => f.write_str("settle_remaining_fill"),
            Self::SettleMixedKinds {
                generous,
                violating,
            } => write!(
                f,
                "settle_mixed_kinds_{}_{}",
                kind(*generous),
                kind(*violating)
            ),
        }
    }
}

/// Where `send_transaction_metered` writes its measurements: one single-record
/// file per `<test>/<label>`, which `just bench` merges into `bench-report.json`.
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

/// Wraps [`LiteSVM::send_transaction`] and, when `TEST_BENCHMARK` is set, records
/// the transaction's compute units, locked accounts, and instruction-data bytes
/// under `<test name>/<label>`. Rent allocation/deallocation is not measured.
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
#[allow(
    clippy::disallowed_methods,
    reason = "the metered path clippy.toml points every test at, so this is the call it exists to wrap"
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

    // only write benchmarks if `TEST_BENCHMARK` is set to something
    if env::var("TEST_BENCHMARK").is_ok() {
        record_benchmark(BENCH_REPORT_DIR, &label, tx, &result);
    }

    result
}

/// Assemble `instructions` into a transaction signed by `payer`, submit it
/// metered under `label`, and surface only the transaction-level error on
/// failure (dropping the failure metadata's other fields).
///
/// This is what a test reaches for when it has instructions rather than an
/// assembled transaction, which covers the multi-instruction settlements (a
/// `[BeginSettle, FinalizeSettle]` pair) most of the suite sends.
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

/// Accounts the transaction locks, as `(readable, writable)`: the static keys
/// plus every address resolved through an Address Lookup Table.
fn accounts_locked(tx: &VersionedTransaction) -> (usize, usize) {
    let &MessageHeader {
        // Not needed: signers are already counted in the remaining two entries,
        num_required_signatures: _,
        num_readonly_signed_accounts,
        num_readonly_unsigned_accounts,
    } = tx.message.header();
    let readable_static = usize::from(num_readonly_signed_accounts)
        .strict_add(usize::from(num_readonly_unsigned_accounts));
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
            |(readable, writable),
             MessageAddressTableLookup {
                 // The lookup table's own account isn't one of the
                 // transaction's locked keys, it contributes to neither tally.
                 account_key: _,
                 writable_indexes,
                 readonly_indexes,
             }| {
                (
                    readable.strict_add(readonly_indexes.len()),
                    writable.strict_add(writable_indexes.len()),
                )
            },
        )
}

fn record_benchmark(dir: &str, label: &str, tx: VersionedTransaction, result: &TransactionResult) {
    let (accounts_readable, accounts_writable) = accounts_locked(&tx);
    let ix_bytes_required: usize = tx
        .message
        .instructions()
        .iter()
        .map(|ix| ix.data.len())
        .sum();

    // A revert stops execution partway, so the consumed count is a partial tally
    // rather than the cost of the transaction: report it as null instead.
    let compute_units_consumed = match &result {
        Ok(meta) => meta.compute_units_consumed.to_string(),
        Err(_) => "null".to_owned(),
    };

    fs::create_dir_all(dir)
        .unwrap_or_else(|e| panic!("failed to create benchmark report directory at {dir}: {e}"));
    let path = shard_path(dir, label);

    let line = format!("{{\"label\": \"{label}\", \"accounts_readable\": {accounts_readable}, \"accounts_writable\": {accounts_writable}, \"instruction_bytes\": {ix_bytes_required}, \"compute_units\": {compute_units_consumed}}}");

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
