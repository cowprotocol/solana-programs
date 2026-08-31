//! Integration tests for the solver gate on settling: `BeginSettle` and
//! `FinalizeSettle` run only for a solver that both signs the transaction and is
//! in the state PDA's solver list. The gate is checked first, so an
//! unauthorized caller is rejected before any settlement work happens.

use cow_settlement_client::cow_settlement_interface::{Instruction, SettlementError};
use cow_settlement_client::instructions::{BeginSettle, FinalizeSettle};
use solana_sdk::{pubkey::Pubkey, signature::Signer, transaction::Transaction};

use crate::common::{
    assert_instruction_error_at, register_solver,
    settlement::{BEGIN_INDEX, FINALIZE_INDEX},
    setup_init, to_instruction_error, unique_keypair,
};

/// A minimal no-op settlement: a `[BeginSettle, FinalizeSettle]` pair with no
/// orders, naming `solver` as `BeginSettle`'s settling solver. It moves no funds,
/// so it isolates the solver gate from the rest of settlement.
fn noop_settlement(program_id: &Pubkey, solver: &Pubkey) -> Vec<Instruction> {
    let begin = BeginSettle {
        program_id: *program_id,
        solver: *solver,
        finalize_ix_index: FINALIZE_INDEX.into(),
        auction_id: 0,
        orders: &[],
    };
    let finalize = FinalizeSettle {
        program_id: *program_id,
        begin_ix_index: BEGIN_INDEX.into(),
        orders: &[],
    };
    vec![begin.into(), finalize.into()]
}

#[test]
fn registered_signing_solver_may_settle() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair();
    register_solver(&mut svm, &params, &solver.pubkey());

    let tx = Transaction::new_signed_with_payer(
        &noop_settlement(&params.program_id, &solver.pubkey()),
        Some(&params.payer.pubkey()),
        &[&params.payer, &solver],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("a registered, signing solver may settle");
}

#[test]
fn registered_signing_solver_may_settle_and_pay_for_tx_fee_itself() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair();
    register_solver(&mut svm, &params, &solver.pubkey());
    // The solver is the fee payer here, so it needs a balance of its own.
    svm.airdrop(&solver.pubkey(), 1_000_000_000)
        .expect("airdrop to solver should succeed");

    let tx = Transaction::new_signed_with_payer(
        &noop_settlement(&params.program_id, &solver.pubkey()),
        Some(&solver.pubkey()),
        &[&solver],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("a registered, signing solver may settle and pay its own fee");
}

#[test]
fn unregistered_solver_may_not_settle() {
    let (mut svm, params) = setup_init();
    // A solver that signs but was never added to the list.
    let solver = unique_keypair();

    let tx = Transaction::new_signed_with_payer(
        &noop_settlement(&params.program_id, &solver.pubkey()),
        Some(&params.payer.pubkey()),
        &[&params.payer, &solver],
        svm.latest_blockhash(),
    );
    assert_instruction_error_at(
        BEGIN_INDEX,
        svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err),
        to_instruction_error(SettlementError::UnauthorizedSolver),
    );
}

#[test]
fn non_signing_solver_may_not_settle() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair();
    register_solver(&mut svm, &params, &solver.pubkey());

    let mut begin_settle: Instruction = BeginSettle {
        program_id: params.program_id,
        solver: solver.pubkey(),
        finalize_ix_index: 0,
        auction_id: 0,
        orders: &[],
    }
    .into();

    let solver_index = 0;
    assert!(
        begin_settle.accounts[solver_index].is_signer
            && begin_settle.accounts[solver_index].pubkey == solver.pubkey(),
        "sanity check: the solver account should start as a signer"
    );
    begin_settle.accounts[solver_index].is_signer = false;
    let tx = Transaction::new_signed_with_payer(
        &[begin_settle],
        Some(&params.payer.pubkey()),
        &[&params.payer],
        svm.latest_blockhash(),
    );
    assert_instruction_error_at(
        BEGIN_INDEX,
        svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err),
        to_instruction_error(SettlementError::UnauthorizedSolver),
    );
}
