//! Compute-unit benchmarking helpers for the settlement integration tests.
//!
//! More benchmarking utilities land here in a follow-up PR.

use litesvm::{types::TransactionResult, LiteSVM};
use solana_sdk::{pubkey::Pubkey, transaction::VersionedTransaction};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::ErrorKind,
    path::Path,
    thread,
    time::Duration,
};

/// Where `send_transaction_metered` accumulates its measurements: a JSON
/// object mapping each label passed at the call site to the CU it consumed.
const CU_REPORT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/cu-report.json");

/// Wraps svm.send_transaction and captures the compute units consumed by
/// `program_id`'s own execution. The measured usage is recorded to a JSON
/// file at the key specified by `label`.
///
/// Only CUs are captured--not rent allocation/deallocation.
#[allow(
    clippy::result_large_err,
    reason = "mirrors litesvm::LiteSVM::send_transaction's own return type, which we don't control"
)]
pub fn send_transaction_metered(
    svm: &mut LiteSVM,
    tx: impl Into<VersionedTransaction>,
    label: &str,
    program_id: &Pubkey,
) -> TransactionResult {
    let result = svm.send_transaction(tx);
    let logs = match &result {
        Ok(meta) => &meta.logs,
        Err(failed) => &failed.meta.logs,
    };
    let compute_units_consumed = compute_units_by_program(logs)
        .get(program_id)
        .copied()
        .unwrap_or(0);

    record_compute_units(CU_REPORT_PATH, label, compute_units_consumed);

    result
}

/// Parse the runtime's own program-invocation trace out of a transaction's text
/// `logs` — lines of the form:
/// ```text
/// Program <id> invoke [<depth>]
/// Program <id> consumed <n> of <m> compute units
/// Program <id> success  (or: Program <id> failed: <err>)
/// ```
/// — and return each program's *own* CU cost, excluding any CPI it makes.
fn compute_units_by_program(logs: &[String]) -> HashMap<Pubkey, u64> {
    struct Frame<'a> {
        program_id: &'a str,
        consumed: u64,
        children_consumed: u64,
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut self_cu: HashMap<Pubkey, u64> = HashMap::new();

    for log in logs {
        match log.split(' ').collect::<Vec<_>>().as_slice() {
            // start of a program invocation frame
            ["Program", program_id, "invoke", _depth] => {
                stack.push(Frame {
                    program_id,
                    consumed: 0,
                    children_consumed: 0,
                });
            }
            //  record of the current program frame CU cost
            ["Program", _program_id, "consumed", n, "of", _budget, "compute", "units"] => {
                let consumed: u64 = n.parse().expect("consumed CU count should parse");
                if let Some(frame) = stack.last_mut() {
                    frame.consumed = consumed;
                }
            }
            // end of a program invocation frame
            ["Program", program_id, "success"] | ["Program", program_id, "failed:", ..] => {
                let Some(frame) = stack.pop() else {
                    continue;
                };
                debug_assert_eq!(frame.program_id, *program_id);
                let own_consumed = frame.consumed.saturating_sub(frame.children_consumed);
                let existing = self_cu
                    .entry(
                        program_id
                            .parse::<Pubkey>()
                            .expect("failed to parse program ID from transaction trace"),
                    )
                    .or_default();
                *existing = existing.saturating_add(own_consumed);
                if let Some(parent) = stack.last_mut() {
                    parent.children_consumed =
                        parent.children_consumed.saturating_add(frame.consumed);
                }
            }
            _ => {} // an invocation-shaped line we don't need (e.g. a precompile)
        }
    }

    self_cu
}

/// Merge `(label, compute_units_consumed)` into a shared CU report.
/// It reads the file, modifies with the newly reported value, and
/// then overwrites.
///
/// Since tests run in parallel, a lock file is used to mutex
/// and prevent race conditions.
fn record_compute_units(file_path: &str, label: &str, compute_units_consumed: u64) {
    let path = Path::new(file_path);
    let lock_path = path.with_extension("json.lock");

    let lock = loop {
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(file) => break file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("failed to acquire CU report lock at {lock_path:?}: {e}"),
        }
    };

    let mut report: BTreeMap<String, u64> = fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default();
    report.insert(label.to_string(), compute_units_consumed);

    fs::write(
        path,
        serde_json::to_string_pretty(&report).expect("CU report should serialize"),
    )
    .expect("CU report should be writable");

    drop(lock);
    fs::remove_file(&lock_path).expect("CU report lock should be removable");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `meta.logs` captured verbatim from a real run of `create_buffer.rs`'s
    /// `happy_path_creates_initialized_buffer_token_account`
    #[test]
    fn excludes_a_cpi_callee_that_logs_its_own_consumed_line() {
        let logs = [
            "Program 11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs invoke [1]",
            "Program 11111111111111111111111111111111 invoke [2]",
            "Program 11111111111111111111111111111111 success",
            "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA invoke [2]",
            "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA consumed 235 of 189927 compute units",
            "Program TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA success",
            "Program 11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs consumed 10322 of 200000 compute units",
            "Program 11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs success",
        ]
        .map(String::from);

        let settlement: Pubkey = "11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs"
            .parse()
            .expect("test fixture id should parse");
        let system_program: Pubkey = "11111111111111111111111111111111"
            .parse()
            .expect("test fixture id should parse");
        let token_program: Pubkey = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
            .parse()
            .expect("test fixture id should parse");

        let cu = compute_units_by_program(&logs);

        assert_eq!(
            cu.get(&token_program),
            Some(&235),
            "token program's own cost should be its logged 'consumed' figure"
        );
        assert_eq!(
            cu.get(&system_program),
            Some(&0),
            "native builtins never log a 'consumed' line, so their own cost can't be recovered"
        );
        assert_eq!(
            cu.get(&settlement),
            Some(&10087),
            "settlement's own cost should exclude only the CPI callee whose cost was logged (10322 - 235)"
        );
    }

    /// `err.meta.logs` captured verbatim from a real run of
    /// `create_buffer.rs`'s `rejects_no_buffers`.
    #[test]
    fn attributes_cu_up_to_a_failed_invocation() {
        let logs = [
            "Program 11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs invoke [1]",
            "Program 11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs consumed 126 of 200000 compute units",
            "Program 11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs failed: insufficient account keys for instruction",
        ]
        .map(String::from);

        let settlement: Pubkey = "11157t3sqMV725NVRLrVQbAu98Jjfk1uCKehJnXXQs"
            .parse()
            .expect("test fixture id should parse");

        let cu = compute_units_by_program(&logs);

        assert_eq!(cu.get(&settlement), Some(&126));
    }
}
