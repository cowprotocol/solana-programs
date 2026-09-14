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
    setup_init, signed_tx, InitializedParams,
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
    assert_instruction_error(res, SettlementError::UnauthorizedAuthorityTransfer);
}

/// Asserts that `signer` may transfer *only* `allowed`: every other role (see
/// [`Role::ALL`]) is rejected with `expected`. Adding a `Role` extends the
/// coverage automatically.
fn assert_transfers_only(
    svm: &mut LiteSVM,
    params: &InitializedParams,
    signer: &Keypair,
    allowed: Role,
) {
    for role in Role::ALL.into_iter().filter(|&role| role != allowed) {
        assert_transfer_rejected(svm, params, role, signer);
    }
}

/// Declares which [`InitializedParams`] keypair holds each [`Role`] after
/// [`setup_init`], and generates the whole authorization matrix from that
/// roster:
///
/// * the manager — the first entry, and the highest authority — may transfer
///   every role, including its own;
/// * every other holder may transfer its own role and no other;
/// * a sanity check that the roster lists exactly [`Role::ALL`], in order.
///
/// "Entry" names a keypair field of [`InitializedParams`]; "Role" is a [`Role`]
/// variant, snake-cased into the test names. Adding a `Role` to the roster
/// extends the coverage automatically.
macro_rules! authority_matrix {
    (
        $manager:ident holds $manager_role:ident,
        $($signer:ident holds $role:ident),+ $(,)?
    ) => {
        pastey::paste! {
            #[test]
            fn sanity_check_roster_lists_exactly_all_roles() {
                assert_eq!([Role::$manager_role, $(Role::$role),+], Role::ALL);
            }

            #[test]
            fn [< $manager _can_transfer_ $manager_role:snake >]() {
                let (mut svm, params) = setup_init();
                let signer = &params.$manager;
                assert_transfers_authority(&mut svm, &params, Role::$manager_role, signer);
            }

            $(
                #[test]
                fn [< $manager _can_transfer_ $role:snake >]() {
                    let (mut svm, params) = setup_init();
                    let signer = &params.$manager;
                    assert_transfers_authority(&mut svm, &params, Role::$role, signer);
                }

                #[test]
                fn [< $role:snake _can_transfer_itself >]() {
                    let (mut svm, params) = setup_init();
                    let signer = &params.$signer;
                    assert_transfers_authority(&mut svm, &params, Role::$role, signer);
                }

                #[test]
                fn [< $role:snake _cannot_transfer_other_roles >]() {
                    let (mut svm, params) = setup_init();
                    assert_transfers_only(&mut svm, &params, &params.$signer, Role::$role);
                }
            )+
        }
    };
}

authority_matrix! {
    manager holds Manager,
    reclaim holds ReclaimAuthority,
    fee_withdrawal holds FeeWithdrawalAuthority,
}

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

    let res = common::send(&mut svm, &payer, &[ix]);
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
