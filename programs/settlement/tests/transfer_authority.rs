//! Integration tests for the authority transfer instruction.

use cow_settlement_client::cow_settlement_interface::{
    data::state::StateAccount, instruction::transfer_authority::fixtures::ROLE_OFFSET, Instruction,
    Role, SettlementError,
};
use cow_settlement_client::instruction::TransferAuthority;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    setup_init, signed_tx, to_instruction_error, InitializedParams,
};

mod common;

fn read_authority(svm: &LiteSVM, state_pda: &Pubkey, role: Role) -> Pubkey {
    let account = svm
        .get_account(state_pda)
        .expect("state PDA should exist after initialize");
    StateAccount::attach(&account.data[..])
        .expect("state PDA should be a valid state account")
        .authority(role)
}

/// Runs a `TransferAuthority` that should succeed: `signer` transfers `role` to
/// a fresh account. Asserts the role's holder is updated immediately.
fn assert_transfers_authority(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    role: Role,
    signer: &Keypair,
) {
    let new_authority = common::unique_keypair();

    let transfer = TransferAuthority {
        program_id: params.program_id,
        signer: signer.pubkey(),
        role,
        new_authority: new_authority.pubkey(),
    };
    let tx = signed_tx(svm, &params.payer, signer, transfer);
    send_transaction_metered(svm, tx, BenchLabel::TransferAuthority)
        .expect("transfer should succeed");

    assert_eq!(
        read_authority(svm, &params.state_pda, role),
        new_authority.pubkey(),
        "the role's holder should be updated to the new authority"
    );
}

/// Runs a `TransferAuthority` that should be rejected: `signer` tries to
/// transfer `role`, and the program returns `expected`.
fn assert_transfer_rejected(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    role: Role,
    signer: &Keypair,
    expected: SettlementError,
) {
    let new_authority = common::unique_keypair();

    let transfer = TransferAuthority {
        program_id: params.program_id,
        signer: signer.pubkey(),
        role,
        new_authority: new_authority.pubkey(),
    };
    let tx = signed_tx(svm, &params.payer, signer, transfer);
    let res = svm.send_transaction(tx).map_err(|e| e.err);
    assert_instruction_error(res, to_instruction_error(expected));
}

/// Asserts that `signer` may transfer *only* `allowed`: every other role (see
/// [`Role::ALL`]) is rejected with `expected`. Adding a `Role` extends the
/// coverage automatically.
fn assert_transfers_only(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    signer: &Keypair,
    allowed: Role,
    expected: SettlementError,
) {
    for role in Role::ALL.into_iter().filter(|&role| role != allowed) {
        assert_transfer_rejected(svm, params, role, signer, expected);
    }
}

/// Generates one integration test. Two forms:
///
/// - "Entry transfers Role" — asserts that transfer succeeds.
/// - "Entry transfers only Role, error Error" — asserts every *other* role is
///   rejected with Error.
///
/// "Entry" names a keypair field of [`InitializedParams`]; "Error" is the
/// expected [`SettlementError`].
macro_rules! transfer_authority_test {
    ($name:ident: $signer:ident transfers $role:expr) => {
        #[test]
        fn $name() {
            let (mut svm, params) = setup_init();
            assert_transfers_authority(&mut svm, &params, $role, &params.$signer);
        }
    };

    ($name:ident: $signer:ident transfers only $allowed:expr, error $err:expr) => {
        #[test]
        fn $name() {
            let (mut svm, params) = setup_init();
            assert_transfers_only(&mut svm, &params, &params.$signer, $allowed, $err);
        }
    };
}

// The manager (the highest authority) may transfer any role; a role's current
// holder may transfer it to a replacement.
transfer_authority_test!(manager_can_transfer_manager: manager transfers Role::Manager);
transfer_authority_test!(manager_can_transfer_reclaim_authority: manager transfers Role::ReclaimAuthority);
transfer_authority_test!(reclaim_authority_can_transfer_itself: reclaim transfers Role::ReclaimAuthority);

// A non-manager authority may transfer only its own role; every other role is
// rejected.
transfer_authority_test!(reclaim_authority_cannot_transfer_other_roles: reclaim transfers only Role::ReclaimAuthority, error SettlementError::UnauthorizedAuthorityTransfer);

/// Index of the signer account in a `TransferAuthority` instruction.
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
    ) = setup_init();
    let new_manager = common::unique_keypair();

    let mut ix: Instruction = TransferAuthority {
        program_id,
        signer: manager.pubkey(),
        role: Role::Manager,
        new_authority: new_manager.pubkey(),
    }
    .into();
    // The signer's account is present but no longer flagged as a signer, so the
    // program rejects the transfer even though it comes from the manager.
    assert!(
        ix.accounts[SIGNER_INDEX].is_signer,
        "test sanity check failed: SIGNER_INDEX should point to signer"
    );
    ix.accounts[SIGNER_INDEX].is_signer = false;

    let res = common::send(&mut svm, &payer, vec![ix]);
    assert_instruction_error(res, InstructionError::MissingRequiredSignature);
}

#[test]
fn rejects_out_of_bounds_role_id() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            manager,
            ..
        },
    ) = setup_init();
    let new_authority = common::unique_keypair();

    let mut ix: Instruction = TransferAuthority {
        program_id,
        signer: manager.pubkey(),
        role: Role::Manager,
        new_authority: new_authority.pubkey(),
    }
    .into();
    ix.data[ROLE_OFFSET] = 0xff;

    let tx = signed_tx(&svm, &payer, &manager, ix);
    let res = svm.send_transaction(tx).map_err(|e| e.err);
    assert_instruction_error(res, InstructionError::InvalidInstructionData);
}

#[test]
fn transferring_against_uninitialized_state_pda_fails() {
    // No `Initialize`, so the canonical state PDA holds no account data.
    let (mut svm, program_id, payer) = common::setup();
    let signer = common::unique_keypair();
    let new_authority = common::unique_keypair();

    let transfer = TransferAuthority {
        program_id,
        signer: signer.pubkey(),
        role: Role::Manager,
        new_authority: new_authority.pubkey(),
    };
    let tx = signed_tx(&svm, &payer, &signer, transfer);
    let res = svm.send_transaction(tx).map_err(|e| e.err);
    assert_instruction_error(res, InstructionError::InvalidAccountData);
}
