//! Integration tests for withdrawing lamports from the native SOL buffer.

use cow_settlement_client::instruction::WithdrawNativeSol;
use cow_settlement_interface::{
    instruction::withdraw_native_sol::WithdrawNativeSol as WithdrawNativeSolRaw,
    pda::{buffer::NATIVE_SOL_BUFFER_PDA, state::STATE_PDA},
    Instruction, SettlementError,
};
use solana_sdk::{signature::Signer, transaction::TransactionError};

use crate::common::benchmark::{send_transaction_metered, BenchLabel};
use crate::common::{
    assert_instruction_error, buffer, lamports, send_with_signers, InitializedParams,
};

mod common;

/// Enough that even half of it leaves an unfunded recipient rent-exempt.
const FUNDING: u64 = 10_000_000;

#[test]
fn happy_path_withdraws_to_a_recipient_chosen_by_the_authority() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            settlement_owned_order: authority,
            ..
        },
    ) = common::setup_init();
    let recipient = common::unique_pubkey();
    let native_buffer_balance = buffer::add_native_lamports(&mut svm, FUNDING);
    let authority_before = lamports(&svm, &authority.pubkey());

    let ix = WithdrawNativeSol {
        program_id,
        authority: authority.pubkey(),
        recipient,
        amount: FUNDING,
    };
    let tx = common::signed_tx(&svm, &payer, &authority, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::WithdrawNativeSol)
        .expect("withdrawing everything above rent should succeed");

    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        native_buffer_balance - FUNDING
    );
    assert_eq!(lamports(&svm, &recipient), FUNDING);
    assert_eq!(
        lamports(&svm, &authority.pubkey()),
        authority_before,
        "the authority must not be credited when it named someone else"
    );
    common::assert_rent_exempt(
        &svm,
        &svm.get_account(&NATIVE_SOL_BUFFER_PDA)
            .expect("the native SOL buffer must survive the withdrawal"),
    );
}

#[test]
fn withdraws_to_the_authority_itself() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            settlement_owned_order: authority,
            ..
        },
    ) = common::setup_init();
    let native_buffer_balance = buffer::add_native_lamports(&mut svm, FUNDING);
    let authority_before = lamports(&svm, &authority.pubkey());

    let ix = WithdrawNativeSol {
        program_id,
        authority: authority.pubkey(),
        recipient: authority.pubkey(),
        amount: FUNDING / 2,
    };
    send_with_signers(&mut svm, &payer, &[&authority], &[ix.into()])
        .expect("a partial withdrawal should succeed");

    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        native_buffer_balance - FUNDING / 2
    );
    assert_eq!(
        lamports(&svm, &authority.pubkey()),
        authority_before + FUNDING / 2
    );
}

#[test]
fn rejects_an_unknown_authority() {
    let (
        mut svm,
        InitializedParams {
            program_id, payer, ..
        },
    ) = common::setup_init();
    let native_buffer_balance = buffer::add_native_lamports(&mut svm, FUNDING);
    let impostor = common::unique_keypair();

    let ix = WithdrawNativeSol {
        program_id,
        authority: impostor.pubkey(),
        recipient: impostor.pubkey(),
        amount: 1,
    };
    assert_instruction_error(
        send_with_signers(&mut svm, &payer, &[&impostor], &[ix.into()]),
        SettlementError::UnauthorizedNativeSolWithdrawal,
    );

    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        native_buffer_balance
    );
}

#[test]
fn rejects_dipping_into_rent() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            settlement_owned_order: authority,
            ..
        },
    ) = common::setup_init();
    let native_buffer_balance = buffer::add_native_lamports(&mut svm, FUNDING);

    let ix = WithdrawNativeSol {
        program_id,
        authority: authority.pubkey(),
        recipient: authority.pubkey(),
        amount: FUNDING + 1,
    };
    let err = send_with_signers(&mut svm, &payer, &[&authority], &[ix.into()])
        .expect_err("a withdrawal into the native SOL buffer's rent must be rejected");
    assert!(
        matches!(err, TransactionError::InsufficientFundsForRent { .. }),
        "expected a rent failure, got {err:?}",
    );

    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        native_buffer_balance
    );
}

/// The runtime would happily delete an emptied buffer, and only `Initialize`
/// can create it, so the program refuses.
#[test]
fn rejects_draining_the_buffer() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            settlement_owned_order: authority,
            ..
        },
    ) = common::setup_init();
    let native_buffer_balance = buffer::add_native_lamports(&mut svm, FUNDING);

    let ix = WithdrawNativeSol {
        program_id,
        authority: authority.pubkey(),
        recipient: authority.pubkey(),
        amount: native_buffer_balance,
    };
    assert_instruction_error(
        send_with_signers(&mut svm, &payer, &[&authority], &[ix.into()]),
        SettlementError::NativeSolBufferEmptied,
    );

    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        native_buffer_balance
    );
}

/// Another program-owned account is just as debitable, so the program has to
/// pin the buffer's address itself.
#[test]
fn rejects_a_non_canonical_native_sol_buffer() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            settlement_owned_order: authority,
            ..
        },
    ) = common::setup_init();
    let impostor_buffer = common::create_account(&mut svm, &program_id, &[]);
    let impostor_funded = lamports(&svm, &impostor_buffer) + FUNDING;
    svm.airdrop(&impostor_buffer, FUNDING)
        .expect("airdrop should succeed");

    let ix: Instruction = WithdrawNativeSolRaw {
        program_id,
        state_pda: STATE_PDA,
        authority: authority.pubkey(),
        native_sol_buffer: impostor_buffer,
        recipient: authority.pubkey(),
        amount: 1,
    }
    .into();
    assert_instruction_error(
        send_with_signers(&mut svm, &payer, &[&authority], &[ix]),
        SettlementError::NativeSolBufferMismatch,
    );

    assert_eq!(lamports(&svm, &impostor_buffer), impostor_funded);
}
