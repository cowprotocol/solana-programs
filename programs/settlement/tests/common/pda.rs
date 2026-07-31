//! Shared helper for PDA-related tests.

use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    transaction::{Transaction, TransactionError},
};

/// Find a non-canonical PDA for `seeds` under `program_id`: a bump strictly
/// below the canonical one that still derives an off-curve address. Such a PDA
/// is a legitimate derivation for the seed scheme, just not the canonical
/// address the program signs for. Returns the bump and its address.
pub fn find_noncanonical_pda<const N: usize>(
    program_id: &Pubkey,
    seeds: [&[u8]; N],
) -> (u8, Pubkey) {
    let (_canonical_pda, canonical_bump) = Pubkey::find_program_address(&seeds, program_id);
    (0..canonical_bump)
        .rev()
        .find_map(|bump| {
            let bump_seed = [bump];
            let mut signer_seeds = seeds.to_vec();
            signer_seeds.push(&bump_seed);
            Pubkey::create_program_address(&signer_seeds, program_id)
                .ok()
                .map(|address| (bump, address))
        })
        .expect("seeds must have a non-canonical off-curve bump")
}

/// Send `tx` and assert it's rejected because `pda` is not the canonical
/// address the program signs for. The runtime grants the PDA signature only
/// for the canonical address, so signing `CreateAccount` for any other `pda`
/// fails the CPI with `PrivilegeEscalation` and leaves `pda` uncreated.
pub fn assert_rejected_as_noncanonical(svm: &mut LiteSVM, tx: Transaction, pda: &Pubkey) {
    let err = svm
        .send_transaction(tx)
        .expect_err("non-canonical PDA must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation)
        ),
        "expected instruction 0 to fail, got {:?}",
        err.err,
    );
    assert!(
        svm.get_account(pda).is_none(),
        "rejected PDA must not have been created"
    );
}

/// Send the transaction built by `make_tx` twice and assert the second run is
/// a no-op for the input account: both transactions succeed and the input
/// account is byte-for-byte unchanged compared to the second.
pub fn assert_recreate_is_noop(
    svm: &mut LiteSVM,
    account: &Pubkey,
    make_tx: impl Fn(&LiteSVM) -> Transaction,
) {
    let tx = make_tx(svm);
    svm.send_transaction(tx)
        .expect("first creation should succeed");
    let before = svm
        .get_account(account)
        .expect("account should exist after the first creation");

    // We don't want to send the exact same transaction twice in the same block
    // because Solana doesn't allow it. We force the transactions to be
    // different by expiring the block hash
    svm.expire_blockhash();

    let tx = make_tx(svm);
    svm.send_transaction(tx)
        .expect("recreating an existing account should succeed as a no-op");
    let after = svm
        .get_account(account)
        .expect("account should still exist");

    assert_eq!(
        before, after,
        "redundant create must not modify the account"
    );
}
