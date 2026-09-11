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

/// Expands the callback macro `$macro!` over every [`Role`] variant, in
/// the same order as [`Role::ALL`].
macro_rules! for_each_role {
    ($macro:ident) => {
        $macro! { Manager, ReclaimAuthority, FeeWithdrawalAuthority }
    };
}

#[test]
fn sanity_check_for_each_role_lists_exactly_all_roles() {
    macro_rules! as_array {
        ($($role:ident),+ $(,)?) => { [$(Role::$role),+] };
    }
    assert_eq!(for_each_role!(as_array), Role::ALL);
}

/// Emits one `#[test]` named `$name`: `$signer` (a keypair field of
/// [`InitializedParams`]) may transfer `$role`.
macro_rules! transfers_authority_test {
    ($name:ident, $signer:ident, $role:ident) => {
        #[test]
        fn $name() {
            let (mut svm, params) = setup_init();
            assert_transfers_authority(&mut svm, &params, Role::$role, &params.$signer);
        }
    };
}

/// Generates the pair of transfer tests for a non-manager holder, deriving each
/// test's name from the [`Role`] variant. The holder may transfer its own role
/// but no other.
///
/// "Entry" names a keypair field of [`InitializedParams`]; "Role" is a [`Role`]
/// variant, snake-cased into the test name.
macro_rules! transfer_authority_tests {
    ($signer:ident holds $role:ident) => {
        pastey::paste! {
            transfers_authority_test!([< $role:snake _can_transfer_itself >], $signer, $role);

            #[test]
            fn [< $role:snake _cannot_transfer_other_roles >]() {
                let (mut svm, params) = setup_init();
                assert_transfers_only(&mut svm, &params, &params.$signer, Role::$role);
            }
        }
    };
}

/// Generates test `manager_can_transfer_<role>`: the manager (the highest
/// authority) may transfer every role, including its own.
macro_rules! manager_transfer_tests {
    ($($role:ident),+ $(,)?) => {
        pastey::paste! { $(
            transfers_authority_test!([< manager_can_transfer_ $role:snake >], manager, $role);
        )+ }
    };
}

// Generate one test for each role, showing that the manager can transfer each
// of them.
for_each_role!(manager_transfer_tests);

// A non-manager authority may transfer only its own role; every other role is
// rejected.
transfer_authority_tests!(reclaim holds ReclaimAuthority);
transfer_authority_tests!(fee_withdrawal holds FeeWithdrawalAuthority);

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
