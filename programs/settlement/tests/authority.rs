//! Integration tests for the authority proposal instruction.

use cow_settlement_client::cow_settlement_interface::{
    data::state::{EncodedStateAccount, StateAccount},
    pda::state::find_state_pda,
    Instruction, Role, SettlementError,
};
use cow_settlement_client::instructions::{Initialize, ProposeAuthority};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    signed_tx, to_instruction_error,
};

mod common;

/// A settlement initialized by [`setup_initialized`], with the manager and
/// reclaim authority held as keypairs the test can sign transfers with.
struct InitializedParams {
    program_id: Pubkey,
    payer: Keypair,
    state_pda: Pubkey,
    manager: Keypair,
    reclaim: Keypair,
}

/// Spin up an initialized settlement whose manager and reclaim authority are
/// keypairs the test controls, so it can sign transfers on their behalf.
///
/// Returns the SVM and an [`InitializedParams`] bundling the program id, the
/// fee payer, the state PDA, and the manager and reclaim authority keypairs.
fn setup_initialized() -> (LiteSVM, InitializedParams) {
    let (mut svm, program_id, payer) = common::setup();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let manager = common::unique_keypair();
    let reclaim = common::unique_keypair();

    let ix = Initialize {
        program_id,
        payer: payer.pubkey(),
        manager: manager.pubkey(),
        reclaim_authority: reclaim.pubkey(),
    };
    let tx = signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx).expect("initialize should succeed");

    (
        svm,
        InitializedParams {
            program_id,
            payer,
            state_pda,
            manager,
            reclaim,
        },
    )
}

fn read_state(svm: &LiteSVM, state_pda: &Pubkey) -> StateAccount {
    let account = svm
        .get_account(state_pda)
        .expect("state PDA should exist after initialize");
    let bytes: [u8; EncodedStateAccount::SIZE] = account
        .data
        .try_into()
        .expect("state PDA data should be exactly the encoded size");
    StateAccount::try_from(bytes).expect("state PDA should decode")
}

/// Runs a `ProposeAuthority` that should succeed: `proposer` proposes a fresh
/// holder for `role`. Asserts the proposal is recorded in the role's pending
/// slot and that the current holder is unchanged (the transfer is two-step).
fn assert_records_pending_proposal(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    role: Role,
    proposer: &Keypair,
) {
    let new_authority = common::unique_keypair();

    let propose = ProposeAuthority {
        program_id: params.program_id,
        signer: proposer.pubkey(),
        role,
        new_authority: new_authority.pubkey(),
    };
    let tx = signed_tx(svm, &params.payer, proposer, propose);
    send_transaction_metered(svm, tx, BenchLabel::ProposeAuthority)
        .expect("propose should succeed");

    let state = read_state(svm, &params.state_pda);
    assert_eq!(
        state.pending(role),
        new_authority.pubkey(),
        "proposal should be recorded as the role's pending holder"
    );
    assert_eq!(
        state.authority(role),
        role_holder(role, params).pubkey(),
        "the current holder should be unchanged until acceptance"
    );
}

/// Runs a `ProposeAuthority` that should be rejected: `proposer` tries to
/// propose a new holder for `role`, and the program returns `expected`.
fn assert_proposal_rejected(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    role: Role,
    proposer: &Keypair,
    expected: SettlementError,
) {
    let new_authority = common::unique_keypair();

    let propose = ProposeAuthority {
        program_id: params.program_id,
        signer: proposer.pubkey(),
        role,
        new_authority: new_authority.pubkey(),
    };
    let tx = signed_tx(svm, &params.payer, proposer, propose);
    let res = svm.send_transaction(tx).map_err(|e| e.err);
    assert_instruction_error(res, to_instruction_error(expected));
}

/// The keypair currently holding `role` in a freshly [`setup_initialized`]
/// settlement.
fn role_holder(role: Role, params: &InitializedParams) -> &Keypair {
    match role {
        Role::Manager => &params.manager,
        Role::ReclaimAuthority => &params.reclaim,
    }
}

/// Generates one integration test per row, `<proposer> proposes <role>`. Two
/// options:
///
/// - "Entry proposes Role", for successes
/// - "Entry proposes Role, error Error", for reverts
///
/// "Entry" names a keypair field of [`InitializedParams`].
/// "Error" is the expected [`SettlementError`].
macro_rules! propose_authority_tests {
    ($($name:ident: $proposer:ident proposes $role:expr;)*) => {$(
        #[test]
        fn $name() {
            let (mut svm, params) = setup_initialized();
            assert_records_pending_proposal(&mut svm, &params, $role, &params.$proposer);
        }
    )*};

    ($($name:ident: $proposer:ident proposes $role:expr, error $err:expr;)*) => {$(
        #[test]
        fn $name() {
            let (mut svm, params) = setup_initialized();
            assert_proposal_rejected(&mut svm, &params, $role, &params.$proposer, $err);
        }
    )*};
}

// The manager (the highest authority) may propose a new holder for any role; a
// role's current holder may propose its own replacement.
propose_authority_tests! {
    manager_can_propose_new_manager: manager proposes Role::Manager;
    manager_can_propose_new_reclaim_authority: manager proposes Role::ReclaimAuthority;
    reclaim_authority_can_propose_replacement: reclaim proposes Role::ReclaimAuthority;
}

// A non-manager authority may not touch the manager role.
propose_authority_tests! {
    reclaim_authority_cannot_change_the_manager:
        reclaim proposes Role::Manager, error SettlementError::UnauthorizedAuthorityProposal;
}

/// Index of the signer account in a `ProposeAuthority` instruction.
const SIGNER_INDEX: usize = 0;

#[test]
fn signer_must_sign_the_transaction() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            manager,
            ..
        },
    ) = setup_initialized();
    let new_manager = common::unique_keypair();

    let mut ix: Instruction = ProposeAuthority {
        program_id,
        signer: manager.pubkey(),
        role: Role::Manager,
        new_authority: new_manager.pubkey(),
    }
    .into();
    // The proposer's account is present but no longer flagged as a signer, so
    // the program rejects the proposal even though it comes from the manager.
    assert!(
        ix.accounts[SIGNER_INDEX].is_signer,
        "test sanity check failed: SIGNER_INDEX should point to signer"
    );
    ix.accounts[SIGNER_INDEX].is_signer = false;

    let res = common::send(&mut svm, &payer, vec![ix]);
    assert_instruction_error(res, InstructionError::MissingRequiredSignature);
}

#[test]
fn proposing_against_uninitialized_state_pda_fails() {
    // No `Initialize`, so the canonical state PDA holds no account data.
    let (mut svm, program_id, payer) = common::setup();
    let signer = common::unique_keypair();
    let new_authority = common::unique_keypair();

    let propose = ProposeAuthority {
        program_id,
        signer: signer.pubkey(),
        role: Role::Manager,
        new_authority: new_authority.pubkey(),
    };
    let tx = signed_tx(&svm, &payer, &signer, propose);
    let res = svm.send_transaction(tx).map_err(|e| e.err);
    assert_instruction_error(res, InstructionError::InvalidAccountData);
}
