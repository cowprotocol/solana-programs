//! Integration tests for removing solvers from the state PDA's list and the
//! manager gate on removal. Adding solvers is covered by `add_solvers.rs`; the
//! solver gate on settling by `settle_solver_auth.rs`.
//!
//! EXPERIMENT: removal no longer resizes the account or refunds rent, so the
//! state PDA keeps its size and lamports and its data only ever grows. The
//! now-stale trailing slot left behind means the on-account solver list is no
//! longer trustworthy to read back, so these tests assert the size/lamports
//! behavior rather than the list contents.

use cow_settlement_client::cow_settlement_interface::{Instruction, SettlementError};
use cow_settlement_client::instructions::RemoveSolver;
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    setup_init, to_instruction_error, unique_keypair, InitializedParams,
};

mod common;

/// Build a `RemoveSolver` transaction authorized by the manager. Signed by the
/// payer and the manager. Split from [`remove_solver`] so the happy-path test
/// can meter the same transaction.
fn remove_solver_tx(svm: &LiteSVM, params: &InitializedParams, solver: &Pubkey) -> Transaction {
    let ix = RemoveSolver {
        program_id: params.program_id,
        manager: params.manager.pubkey(),
        solver: *solver,
    };
    common::signed_tx(svm, &params.payer, &params.manager, ix)
}

/// Send a [`remove_solver_tx`].
fn remove_solver(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    solver: &Pubkey,
) -> Result<(), TransactionError> {
    let tx = remove_solver_tx(svm, params, solver);
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

#[test]
fn removes_a_solver() {
    let (mut svm, params) = setup_init();
    let keep = unique_keypair().pubkey();
    let drop = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &keep);
    common::register_solver(&mut svm, &params, &drop);

    let before = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    let tx = remove_solver_tx(&svm, &params, &drop);
    send_transaction_metered(&mut svm, tx, BenchLabel::RemoveSolver)
        .expect("removing a solver should succeed");

    // EXPERIMENT: the account is neither shrunk nor refunded, so it keeps its
    // size and lamports. A stale trailing slot is left behind, so the on-account
    // list is intentionally not read back here.
    let after = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    assert_eq!(after.data.len(), before.data.len());
    assert_eq!(after.lamports, before.lamports);
}

#[test]
fn rejects_removing_absent_solver() {
    let (mut svm, params) = setup_init();
    let absent = unique_keypair().pubkey();

    assert_instruction_error(
        remove_solver(&mut svm, &params, &absent),
        to_instruction_error(SettlementError::SolverNotFound),
    );
}

#[test]
fn rejects_removing_solver_by_non_manager() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &solver);

    let stranger = unique_keypair();
    let ix = RemoveSolver {
        program_id: params.program_id,
        manager: stranger.pubkey(),
        solver,
    };
    let tx = common::signed_tx(&svm, &params.payer, &stranger, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err),
        to_instruction_error(SettlementError::UnauthorizedSolverManagement),
    );
}

#[test]
fn rejects_removing_solver_if_manager_is_not_signer() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &solver);

    // The correct manager, but with its signer flag cleared: authorization must
    // require the manager to actually sign, not just be named.
    let mut ix: Instruction = RemoveSolver {
        program_id: params.program_id,
        manager: params.manager.pubkey(),
        solver,
    }
    .into();

    /// Index of the manager account in a `RemoveSolver` instruction.
    const MANAGER_INDEX: usize = 0;
    assert!(
        ix.accounts[MANAGER_INDEX].is_signer
            && ix.accounts[MANAGER_INDEX].pubkey == params.manager.pubkey(),
        "sanity check: MANAGER_INDEX should point to the manager signer"
    );
    ix.accounts[MANAGER_INDEX].is_signer = false;

    let res = common::send(&mut svm, &params.payer, vec![ix]);
    assert_instruction_error(
        res,
        to_instruction_error(SettlementError::UnauthorizedSolverManagement),
    );
}

/// Removing a solver still succeeds when the list is already large. This test
/// also benchmarks moving a lot of account data.
#[test]
fn remove_with_many_existing_solvers() {
    let (mut svm, params) = setup_init();

    /// A deterministic solver address holding `index` big-endian in its leading
    /// two bytes and the rest zero, so their relative order is their index's
    /// order.
    fn indexed_solver(index: u16) -> Pubkey {
        let mut bytes = [0u8; 32];
        let leading = index.to_be_bytes();
        bytes[0] = leading[0];
        bytes[1] = leading[1];
        Pubkey::new_from_array(bytes)
    }

    // Existing solvers 0x0000, 0x0001, …, written straight into the state PDA after
    // its header rather than added one transaction at a time.
    const EXISTING: u16 = 1_000;
    const REMOVE_INDEX: u16 = 42;
    let existing: Vec<Pubkey> = (0..=EXISTING).map(indexed_solver).collect();
    let mut account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    for solver in &existing {
        account.data.extend_from_slice(&solver.to_bytes());
    }
    account.lamports = svm.minimum_balance_for_rent_exemption(account.data.len());
    let data_len_before = account.data.len();
    svm.set_account(params.state_pda, account)
        .expect("set_account should succeed");

    let sacrifice = indexed_solver(REMOVE_INDEX);
    let tx = remove_solver_tx(&svm, &params, &sacrifice);
    send_transaction_metered(&mut svm, tx, BenchLabel::RemoveSolver)
        .expect("removing from a large list should succeed");

    // EXPERIMENT: the account isn't resized, so it keeps its (large) size.
    let after = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    assert_eq!(after.data.len(), data_len_before);
}
