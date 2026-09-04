use cow_settlement_client::cow_settlement_interface::data::state::StateAccount;
use cow_settlement_client::instruction::Initialize;
use litesvm::LiteSVM;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;

/// Send `ix` as the settlement's `Initialize`, signed by `payer`.
pub fn initialize(svm: &mut litesvm::LiteSVM, payer: &Keypair, ix: Initialize) {
    let tx = super::signed_tx(svm, payer, payer, ix);
    svm.send_transaction(tx).expect("initialize should succeed");
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
