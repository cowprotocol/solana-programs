//! Integration tests for the solver list stored in the state PDA: adding solvers
//! (kept sorted, growing the account and funding the extra rent) and the manager
//! gate on adding them.

use cow_settlement_client::cow_settlement_interface::{
    data::state::{fixtures::state_account_bytes, StateInitArgs, WIDTH_HEADER, WIDTH_PUBKEY},
    Instruction, SettlementError,
};
use cow_settlement_client::instructions::AddSolver;
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
    to_instruction_error, unique_keypair, InitializedParams,
};

mod common;

/// Build an `AddSolver` transaction authorized by the manager and paid by the
/// payer, both of which sign. Split from [`add_solver`] so the happy-path test
/// can submit the same transaction through the metered send.
fn add_solver_tx(svm: &LiteSVM, params: &InitializedParams, solver: &Pubkey) -> Transaction {
    let ix = AddSolver {
        program_id: params.program_id,
        manager: params.manager.pubkey(),
        payer: params.payer.pubkey(),
        solver: *solver,
    };
    common::signed_tx(svm, &params.payer, &params.manager, ix)
}

/// Send an [`add_solver_tx`].
fn add_solver(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    solver: &Pubkey,
) -> Result<(), TransactionError> {
    let tx = add_solver_tx(svm, params, solver);
    svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)
}

#[test]
fn adds_a_solver() {
    let (mut svm, params) = setup_init();
    let before = lamports(&svm, &params.state_pda);

    assert_eq!(solvers(&svm, &params.state_pda), vec![]);

    let solver = unique_keypair().pubkey();
    let tx = add_solver_tx(&svm, &params, &solver);
    send_transaction_metered(&mut svm, tx, BenchLabel::AddSolver)
        .expect("adding a solver should succeed");

    assert_eq!(solvers(&svm, &params.state_pda), vec![solver]);

    // The account grew by exactly one solver and stayed rent-exempt (its balance
    // rose to the larger rent minimum, funded by the payer).
    let account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    assert_eq!(account.data.len(), WIDTH_HEADER + WIDTH_PUBKEY);
    assert_eq!(
        account.lamports,
        svm.minimum_balance_for_rent_exemption(account.data.len()),
    );
    assert!(account.lamports > before, "the payer funded the extra rent");
}

#[test]
fn adds_a_solver_without_extra_rent() {
    let (mut svm, params) = setup_init();

    // Pre-fund the state PDA so it holds one more than the rent minimum for the
    // grown size.
    let mut account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    account.lamports = svm
        .minimum_balance_for_rent_exemption(WIDTH_HEADER + WIDTH_PUBKEY)
        .strict_add(1);
    svm.set_account(params.state_pda, account)
        .expect("set_account should succeed");
    let before = lamports(&svm, &params.state_pda);

    let solver = unique_keypair().pubkey();
    let tx = add_solver_tx(&svm, &params, &solver);
    svm.send_transaction(tx)
        .expect("adding a solver should succeed");

    assert_eq!(solvers(&svm, &params.state_pda), vec![solver]);

    // The account grew by one solver and its balance is unchanged: it was already
    // rent-exempt for the new size, so the payer funded nothing.
    let account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    assert_eq!(account.data.len(), WIDTH_HEADER + WIDTH_PUBKEY);
    assert_eq!(account.lamports, before, "no extra rent was pulled");
}

/// Solvers are stored sorted no matter the order they're added in. Adds many
/// solvers in hash order (effectively unsorted) and checks the stored list came
/// out sorted.
#[test]
fn keeps_solvers_sorted() {
    let (mut svm, params) = setup_init();

    const COUNT: usize = 50;
    let mut added: Vec<Pubkey> = (0..COUNT).map(|_| unique_keypair().pubkey()).collect();
    for solver in &added {
        add_solver(&mut svm, &params, solver).expect("adding a solver should succeed");
    }

    added.sort();
    assert_eq!(solvers(&svm, &params.state_pda), added);
}

#[test]
fn rejects_adding_an_existing_solver() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair().pubkey();
    add_solver(&mut svm, &params, &solver).expect("first add should succeed");

    // The re-add is an identical message; move past the first transaction's
    // blockhash so it isn't rejected as a duplicate before reaching the program.
    svm.expire_blockhash();
    assert_instruction_error(
        add_solver(&mut svm, &params, &solver),
        to_instruction_error(SettlementError::SolverAlreadyExists),
    );
}

#[test]
fn rejects_adding_solver_if_manager_is_not_signer() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair().pubkey();

    let mut ix: Instruction = AddSolver {
        program_id: params.program_id,
        manager: params.manager.pubkey(),
        payer: params.payer.pubkey(),
        solver,
    }
    .into();
    assert!(
        ix.accounts[MANAGER_INDEX].is_signer && !ix.accounts[MANAGER_INDEX].is_writable,
        "test sanity check failed: MANAGER_INDEX should point to the manager signer"
    );
    ix.accounts[MANAGER_INDEX].is_signer = false;

    let res = common::send(&mut svm, &params.payer, vec![ix]);
    assert_instruction_error(
        res,
        to_instruction_error(SettlementError::UnauthorizedSolverManagement),
    );
}

#[test]
fn rejects_adding_solver_by_non_manager() {
    let (mut svm, params) = setup_init();
    let solver = unique_keypair().pubkey();

    let stranger = unique_keypair();
    let ix = AddSolver {
        program_id: params.program_id,
        manager: stranger.pubkey(),
        payer: params.payer.pubkey(),
        solver,
    };
    let tx = common::signed_tx(&svm, &params.payer, &stranger, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err),
        to_instruction_error(SettlementError::UnauthorizedSolverManagement),
    );
}

/// Adding a solver still works, and stays sorted, when the list is already
/// large. This test also benchmarks moving a lot of account data.
#[test]
fn add_with_many_existing_solvers() {
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

    // Existing solvers 0x0000, 0x0001, … (minus the one added below), written
    // straight into the state PDA via the account fixture, which records the live
    // solver count in the header, rather than added one transaction at a time.
    const EXISTING: u16 = 1_000;
    const NEW_INDEX: u16 = 42;
    let mut expected: Vec<Pubkey> = (0..=EXISTING)
        .filter(|i| *i != NEW_INDEX)
        .map(indexed_solver)
        .collect();
    let init = StateInitArgs {
        manager: params.manager.pubkey(),
        reclaim_authority: params.reclaim.pubkey(),
    };
    let mut account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    account.data = state_account_bytes(&init, &expected);
    account.lamports = svm.minimum_balance_for_rent_exemption(account.data.len());
    svm.set_account(params.state_pda, account)
        .expect("set_account should succeed");

    // Insert 0x0142: it sorts ahead of every existing solver, so it lands at the
    // front and the program shifts the whole list.
    let extra = indexed_solver(NEW_INDEX);
    let tx = add_solver_tx(&svm, &params, &extra);
    send_transaction_metered(&mut svm, tx, BenchLabel::AddSolver)
        .expect("adding into a large list should succeed");

    expected.push(extra);
    expected.sort();
    assert_eq!(solvers(&svm, &params.state_pda), expected);
}

/// Index of the manager account in an `AddSolver` instruction.
const MANAGER_INDEX: usize = 0;

#[test]
fn rejects_adding_solver_if_state_pda_is_uninitialized() {
    let (mut svm, program_id, payer) = common::setup();
    let manager = unique_keypair();
    let solver = unique_keypair().pubkey();

    let ix = AddSolver {
        program_id,
        manager: manager.pubkey(),
        payer: payer.pubkey(),
        solver,
    };
    let tx = common::signed_tx(&svm, &payer, &manager, ix);
    let res = svm.send_transaction(tx).map_err(|e| e.err);
    assert_instruction_error(res, InstructionError::InvalidAccountData);
}
