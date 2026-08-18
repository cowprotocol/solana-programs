use cow_settlement_client::cow_settlement_interface::data::state::EncodedStateAccount;
use solana_sdk::pubkey::Pubkey;

use super::create_account_at;

/// Preallocate the settlement state PDA directly in the SVM, bypassing the
/// on-chain `Initialize` instruction.
///
/// The real `Initialize` creates the state account with a single system-program
/// CPI, and a CPI can only grow an account by up to 10 KiB in one instruction.
/// With the solver region widened to 1000 slots the account is ~32 KiB, so that
/// CPI can no longer allocate it and `Initialize` is broken. The tests that rely
/// on an initialized settlement don't exercise initialization itself: they only
/// need a correctly sized, program-owned state account to measure the other
/// instructions against. So we write it straight into the SVM with the exact
/// bytes a working `Initialize` would have produced — discriminator, authorities,
/// no pending transfers, and the `0xff` solver placeholder filling out the size.
pub fn preallocate(
    svm: &mut litesvm::LiteSVM,
    program_id: &Pubkey,
    state_pda: &Pubkey,
    manager: Pubkey,
    reclaim_authority: Pubkey,
) {
    let mut body = [0u8; EncodedStateAccount::SIZE];
    EncodedStateAccount::write_initial(&mut body, &manager, &reclaim_authority);
    create_account_at(svm, *state_pda, program_id, &body);
}
