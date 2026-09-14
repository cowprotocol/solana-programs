use cow_settlement_client::cow_settlement_interface::data::state::{
    StateAccount, WIDTH_HEADER, WIDTH_PUBKEY,
};
use cow_settlement_client::instruction::Initialize;
use litesvm::LiteSVM;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;

/// Send `ix` as the settlement's `Initialize`, signed by `payer`.
pub fn initialize(svm: &mut litesvm::LiteSVM, payer: &Keypair, ix: Initialize) {
    let tx = super::signed_tx(svm, payer, payer, ix);
    svm.send_transaction(tx).expect("initialize should succeed");
}

/// Plant `count` extra fake solvers straight into the state PDA's solver list,
/// bypassing `AddSolver`. The fakes are merged with the solvers already stored
/// and written back in the strictly-ascending order the program's binary search
/// relies on, so the account stays a valid state account. Lets a benchmark
/// measure settlement against a realistically populated solver list without
/// sending thousands of `AddSolver` transactions.
pub fn plant_fake_solvers(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    state_pda: &Pubkey,
    count: usize,
) {
    let account = svm.get_account(state_pda).expect("state PDA should exist");

    // Keep the header (discriminator + authorities) byte-for-byte; only the
    // solver list that follows it is rewritten.
    let header = &account.data[..WIDTH_HEADER];
    let mut solvers: Vec<Pubkey> = StateAccount::attach(&account.data[..])
        .expect("state PDA should be a valid state account")
        .solvers()
        .collect();
    solvers.extend(std::iter::repeat_with(super::unique_pubkey).take(count));
    // The program binary-searches this list, so it must be strictly ascending;
    // sorting the fakes in with the real solver and dropping any collision keeps
    // that invariant (see [`assert_solver_invariant`]).
    solvers.sort_unstable();
    solvers.dedup();

    let mut data = Vec::with_capacity(WIDTH_HEADER + solvers.len() * WIDTH_PUBKEY);
    data.extend_from_slice(header);
    for solver in &solvers {
        data.extend_from_slice(&solver.to_bytes());
    }

    super::create_account_at(svm, *state_pda, program_id, &data);
}

/// Assert the solver list's storage invariant: solvers are stored strictly
/// ascending by address (sorted, with no duplicates). This is what lets the
/// program binary-search the list, so every read below re-checks it.
#[track_caller]
pub fn assert_solver_invariant(solvers: &[Pubkey]) {
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
pub fn solvers(svm: &LiteSVM, state_pda: &Pubkey) -> Vec<Pubkey> {
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
