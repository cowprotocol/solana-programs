//! Integration tests for the solver list stored in the state PDA: adding solvers
//! (kept sorted, growing the account and funding the extra rent) and the manager
//! gate on adding them.

use cow_settlement_client::cow_settlement_interface::{
    data::state::{StateAccount, WIDTH_HEADER, WIDTH_PUBKEY},
    Instruction, SettlementError,
};
use cow_settlement_client::instruction::AddSolver;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::MAX_PERMITTED_DATA_LENGTH;

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    create_account_at, lamports, setup_init, to_instruction_error, unique_keypair,
    InitializedParams,
};

mod common;

/// Assert the solver list's storage invariant: solvers are stored strictly
/// ascending by address (sorted, with no duplicates). This is what lets the
/// program binary-search the list, so every read below re-checks it.
#[track_caller]
fn assert_solver_invariant(solvers: &[Pubkey]) {
    assert!(
        // We use `is_sorted_by` here instead of `is_sorted` because that
        // doesn't catch duplicates.
        solvers.is_sorted_by(|a, b| a < b),
        "invariant violated: solver list must be strictly ascending by address: {solvers:?}",
    );
}

/// The solver list currently stored in the state PDA, in stored order. Reading it
/// also re-checks the storage invariant (see [`assert_solver_invariant`]), so
/// every test that inspects the list enforces it, not just the ones that compare
/// against a sorted expectation.
#[track_caller]
fn solvers(svm: &LiteSVM, state_pda: &Pubkey) -> Vec<Pubkey> {
    let data = svm
        .get_account(state_pda)
        .expect("state PDA should exist")
        .data;
    let solvers: Vec<Pubkey> = StateAccount::attach(&data[..])
        .expect("state PDA should be a valid state account")
        .solvers()
        .collect();
    assert_solver_invariant(&solvers);
    solvers
}

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

    // Existing solvers 0x0143, 0x0144, …, written straight into the state PDA after
    // its header rather than added one transaction at a time.
    const EXISTING: u16 = 1_000;
    const NEW_INDEX: u16 = 42;
    let mut expected: Vec<Pubkey> = (0..=EXISTING)
        .filter(|i| *i != NEW_INDEX)
        .map(indexed_solver)
        .collect();
    let mut account = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists");
    for solver in &expected {
        account.data.extend_from_slice(&solver.to_bytes());
    }
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

#[test]
fn rejects_growing_beyond_the_max_account_size() {
    let (mut svm, params) = setup_init();

    let mut data = svm
        .get_account(&params.state_pda)
        .expect("state PDA exists")
        .data;
    data.resize(MAX_PERMITTED_DATA_LENGTH as usize, 0);
    create_account_at(&mut svm, params.state_pda, &params.program_id, &data);

    let solver = unique_keypair().pubkey();
    let tx = add_solver_tx(&svm, &params, &solver);
    let err = svm
        .send_transaction(tx)
        .expect_err("growing past the max account size should revert")
        .err;

    // The revert is the runtime enforcing its account-size limit, not our program:
    // our settlement errors surface as `InstructionError::Custom`, whereas this is a
    // plain `InvalidArgument` from the rent-exemption sizing check.
    assert!(
        !matches!(
            err,
            TransactionError::InstructionError(_, InstructionError::Custom(_))
        ),
        "the revert must not be one of our program's errors: {err:?}",
    );
    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::InvalidArgument),
    );
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
