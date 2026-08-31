//! Integration tests for removing solvers from the state PDA's list (shrinking
//! the account and refunding rent) and the manager gate on removal. Adding
//! solvers is covered by `add_solvers.rs`; the solver gate on settling by
//! `settle_solver_auth.rs`.

use cow_settlement_client::cow_settlement_interface::{
    data::state::{WIDTH_HEADER, WIDTH_PUBKEY},
    Instruction, SettlementError,
};
use cow_settlement_client::instructions::RemoveSolver;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    lamports, setup_init,
    state::solvers,
    to_instruction_error, unique_keypair, unique_pubkey, InitializedParams,
};

mod common;

/// [`setup_init`] plus a funded, dedicated `rent_recipient` for removals. A
/// removal refunds the rent to this account, and the recipient of a lamport
/// credit must itself end up rent-exempt, so it's airdropped here.
fn setup() -> (LiteSVM, InitializedParams, Pubkey) {
    let (mut svm, params) = setup_init();
    let rent_recipient = unique_pubkey();
    svm.airdrop(&rent_recipient, 1_000_000_000)
        .expect("airdrop to rent recipient should succeed");
    (svm, params, rent_recipient)
}

/// Build a `RemoveSolver` transaction authorized by the manager, refunding the
/// freed rent to `rent_recipient`. Signed by the payer and the manager. Split
/// from [`remove_solver`] so the happy-path test can meter the same transaction.
fn remove_solver_tx(
    svm: &LiteSVM,
    params: &InitializedParams,
    rent_recipient: &Pubkey,
    solver: &Pubkey,
) -> Transaction {
    let ix = RemoveSolver {
        program_id: params.program_id,
        manager: params.manager.pubkey(),
        rent_recipient: *rent_recipient,
        solver: *solver,
    };
    common::signed_tx(svm, &params.payer, &params.manager, ix)
}

/// Send a [`remove_solver_tx`].
fn remove_solver(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    rent_recipient: &Pubkey,
    solver: &Pubkey,
) -> Result<(), TransactionError> {
    let tx = remove_solver_tx(svm, params, rent_recipient, solver);
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

#[test]
fn removes_a_solver() {
    let (mut svm, params, rent_recipient) = setup();
    let keep = unique_keypair().pubkey();
    let drop = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &keep);
    common::register_solver(&mut svm, &params, &drop);

    let recipient_before = lamports(&svm, &rent_recipient);
    let tx = remove_solver_tx(&svm, &params, &rent_recipient, &drop);
    send_transaction_metered(&mut svm, tx, BenchLabel::RemoveSolver)
        .expect("removing a solver should succeed");

    // Only `keep` remains, the account shrank by one solver and stayed exactly
    // rent-exempt, and the freed rent went to the rent recipient.
    assert_eq!(solvers(&svm, &params.state_pda), vec![keep]);
    let account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    assert_eq!(account.data.len(), WIDTH_HEADER + WIDTH_PUBKEY);
    assert_eq!(
        account.lamports,
        svm.minimum_balance_for_rent_exemption(account.data.len()),
    );
    assert!(
        lamports(&svm, &rent_recipient) > recipient_before,
        "the rent recipient received the freed rent",
    );
}

#[test]
fn rejects_removing_absent_solver() {
    let (mut svm, params, rent_recipient) = setup();
    let absent = unique_keypair().pubkey();

    assert_instruction_error(
        remove_solver(&mut svm, &params, &rent_recipient, &absent),
        to_instruction_error(SettlementError::SolverNotFound),
    );
}

#[test]
fn rejects_removing_solver_by_non_manager() {
    let (mut svm, params, rent_recipient) = setup();
    let solver = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &solver);

    let stranger = unique_keypair();
    let ix = RemoveSolver {
        program_id: params.program_id,
        manager: stranger.pubkey(),
        rent_recipient,
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
    let (mut svm, params, rent_recipient) = setup();
    let solver = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &solver);

    // The correct manager, but with its signer flag cleared: authorization must
    // require the manager to actually sign, not just be named.
    let mut ix: Instruction = RemoveSolver {
        program_id: params.program_id,
        manager: params.manager.pubkey(),
        rent_recipient,
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

/// A state PDA holding less than its shrunk rent minimum is rejected with
/// [`InstructionError::AccountNotRentExempt`], not refunded.
/// The flow in this test isn't expected to be reachable unless there are
/// changes to how rent is handled. Still, if it does, this will be less of an
/// issue than it could be.
#[test]
fn rejects_removing_from_a_below_rent_state_pda() {
    let (mut svm, params, rent_recipient) = setup();
    let solver = unique_keypair().pubkey();
    common::register_solver(&mut svm, &params, &solver);

    // Reduce the state PDA to one lamport below the rent minimum for zero
    // solvers (its size after the removal), so it can't hold the rent it needs to
    // exist and the refund's `checked_sub` underflows.
    let below_rent = svm
        .minimum_balance_for_rent_exemption(WIDTH_HEADER)
        .strict_sub(1);
    let mut account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    account.lamports = below_rent;
    svm.set_account(params.state_pda, account)
        .expect("set_account should succeed");

    assert_instruction_error(
        remove_solver(&mut svm, &params, &rent_recipient, &solver),
        InstructionError::AccountNotRentExempt,
    );
}

/// Removing a solver still works, and stays sorted, when the list is already
/// large. This test also benchmarks moving a lot of account data.
#[test]
fn remove_with_many_existing_solvers() {
    let (mut svm, params, rent_recipient) = setup();

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
    let mut expected: Vec<Pubkey> = (0..=EXISTING).map(indexed_solver).collect();
    let mut account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    for solver in &expected {
        account.data.extend_from_slice(&solver.to_bytes());
    }
    account.lamports = svm.minimum_balance_for_rent_exemption(account.data.len());
    svm.set_account(params.state_pda, account)
        .expect("set_account should succeed");

    let sacrifice = indexed_solver(REMOVE_INDEX);
    let tx = remove_solver_tx(&svm, &params, &rent_recipient, &sacrifice);
    send_transaction_metered(&mut svm, tx, BenchLabel::RemoveSolver)
        .expect("removing from a large list should succeed");

    expected.retain(|&s| s != sacrifice);
    assert_eq!(solvers(&svm, &params.state_pda), expected);
}
