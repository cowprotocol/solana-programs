//! Integration tests for withdrawing lamports from the native SOL buffer.

use cow_settlement_client::instruction::WithdrawNativeSol;
use cow_settlement_interface::{
    instruction::withdraw_native_sol::WithdrawNativeSol as WithdrawNativeSolRaw,
    pda::{buffer::NATIVE_SOL_BUFFER_PDA, state::STATE_PDA},
    Instruction, SettlementError,
};
use solana_sdk::{instruction::InstructionError, signature::Signer};

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

/// A buffer holding less than its rent-exempt minimum can't be withdrawn from:
/// the cap's `checked_sub` underflows and the withdrawal is rejected with
/// [`InstructionError::AccountNotRentExempt`] rather than silently withdrawing
/// nothing. Not expected to be reachable unless the rent mechanism changes.
#[test]
fn rejects_withdrawing_from_a_below_rent_buffer() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            settlement_owned_order: authority,
            ..
        },
    ) = common::setup_init();

    // Drop the buffer one lamport below its rent-exempt minimum (it holds no
    // data), so the cap's `checked_sub(rent_floor)` underflows. `set_account`
    // bypasses the runtime rent check that would otherwise forbid this state.
    let below_rent = svm.minimum_balance_for_rent_exemption(0).strict_sub(1);
    let mut buffer = svm
        .get_account(&NATIVE_SOL_BUFFER_PDA)
        .expect("the native SOL buffer should exist");
    buffer.lamports = below_rent;
    svm.set_account(NATIVE_SOL_BUFFER_PDA, buffer)
        .expect("set_account should succeed");

    let ix = WithdrawNativeSol {
        program_id,
        authority: authority.pubkey(),
        recipient: authority.pubkey(),
        amount: u64::MAX,
    };
    assert_instruction_error(
        send_with_signers(&mut svm, &payer, &[&authority], &[ix.into()]),
        InstructionError::AccountNotRentExempt,
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

/// Withdraw `requested(native_buffer_balance)` (an amount at or beyond the
/// whole balance) to the authority, and assert it's capped to the lamports
/// above rent: the authority is paid exactly those, and the buffer is left
/// alive at its rent floor rather than the withdrawal reverting.
#[track_caller]
fn assert_overdraw_is_capped(requested: impl FnOnce(u64) -> u64) {
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
        amount: requested(native_buffer_balance),
    };
    send_with_signers(&mut svm, &payer, &[&authority], &[ix.into()])
        .expect("an overdraw should be capped to the balance above rent, not rejected");

    assert_eq!(
        lamports(&svm, &authority.pubkey()),
        authority_before.strict_add(FUNDING)
    );
    assert_eq!(
        lamports(&svm, &NATIVE_SOL_BUFFER_PDA),
        native_buffer_balance.strict_sub(FUNDING)
    );
    common::assert_rent_exempt(
        &svm,
        &svm.get_account(&NATIVE_SOL_BUFFER_PDA)
            .expect("the native SOL buffer must survive the capped withdrawal"),
    );
}

#[test]
fn caps_a_withdrawal_dipping_into_rent() {
    // One lamport past what's above rent.
    assert_overdraw_is_capped(|_native_buffer_balance| FUNDING + 1);
}

#[test]
fn caps_a_withdrawal_exactly_draining_the_buffer() {
    // The buffer's entire balance, rent included.
    assert_overdraw_is_capped(|native_buffer_balance| native_buffer_balance);
}

#[test]
fn caps_a_grossly_oversized_withdrawal() {
    // Far more than the buffer could ever hold.
    assert_overdraw_is_capped(|_native_buffer_balance| u64::MAX);
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
