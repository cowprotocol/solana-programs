//! Shared helper for PDA-related tests.

use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::{error::SystemError, program::ID as SYSTEM_PROGRAM_ID};

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

/// Send `tx` (which is expected to recreate an already-existing PDA) and assert
/// it's rejected because the account exists. The `CreateAccount` CPI fails with
/// `AccountAlreadyInUse`; since that custom code is `0` and thus ambiguous
/// with a program-level `Custom(0)`, we also confirm the failing inner
/// instruction really is a system-program call.
pub fn assert_rejected_as_existing(svm: &mut LiteSVM, tx: Transaction) {
    // Keep the compiled message's `account_keys` so we can resolve the
    // `program_id_index` of the failing inner instruction below.
    let account_keys = tx.message.account_keys.clone();
    let err = svm
        .send_transaction(tx)
        .expect_err("recreating an existing PDA must be rejected");

    let expected = TransactionError::InstructionError(
        0,
        InstructionError::Custom(SystemError::AccountAlreadyInUse as u32),
    );
    assert_eq!(err.err, expected);

    let last_cpi = err.meta.inner_instructions[0]
        .last()
        .expect("system-program CPI should be available");
    let failing_program = account_keys[last_cpi.instruction.program_id_index as usize];
    assert_eq!(failing_program, SYSTEM_PROGRAM_ID);
}
