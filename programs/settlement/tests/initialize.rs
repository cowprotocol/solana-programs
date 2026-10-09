use cow_settlement_client::cow_settlement_interface::{
    data::state::WIDTH_HEADER,
    instruction::initialize::{Initialize as InitializeRaw, INITIALIZER},
    pda::{
        buffer::{find_native_sol_buffer_pda, NATIVE_SOL_BUFFER_PDA, NATIVE_SOL_BUFFER_PDA_SEEDS},
        state::{STATE_PDA, STATE_PDA_SEEDS},
    },
    SettlementError,
};
use cow_settlement_client::instruction::Initialize;
use cow_settlement_client::pda::state::DecodedStateAccount;
use cow_settlement_interface::pda::state::STATE_PDA_AND_BUMP;
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::{Instruction, InstructionError},
    message::Message,
    pubkey::Pubkey,
    signature::Signer,
    transaction::Transaction,
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    pda::find_noncanonical_pda,
    unique_pubkey, PROGRAM_SO,
};

mod common;

#[test]
fn happy_path_initializes_state_pda_with_expected_data() {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    let manager = unique_pubkey();
    let solver_authority = unique_pubkey();
    let reclaim_authority = unique_pubkey();
    let settlement_owned_order_authority = unique_pubkey();

    let ix = Initialize {
        program_id,
        payer: INITIALIZER,
        manager,
        solver_authority,
        reclaim_authority,
        settlement_owned_order_authority,
    };
    let tx = common::initializer_tx(&svm, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::Initialize)
        .expect("initialize should succeed");

    let account = svm
        .get_account(&STATE_PDA)
        .expect("state PDA should exist after initialize");
    assert_eq!(
        account.owner, program_id,
        "state PDA must be owned by the settlement program"
    );
    let decoded = DecodedStateAccount::try_from(&account.data[..])
        .expect("state PDA must decode as a settlement state account");
    assert_eq!(
        decoded,
        DecodedStateAccount {
            manager,
            solver_authority,
            reclaim_authority,
            settlement_owned_order_authority,
        },
        "state PDA body must record the authorities"
    );
    assert_eq!(
        account.data.len(),
        WIDTH_HEADER,
        "a freshly initialized state PDA is exactly a header long"
    );

    let rent = svm.minimum_balance_for_rent_exemption(WIDTH_HEADER);
    assert_eq!(
        account.lamports, rent,
        "state PDA must hold exactly the rent minimum: {} != {}",
        account.lamports, rent,
    );

    assert_eq!(
        find_native_sol_buffer_pda(&program_id).0,
        NATIVE_SOL_BUFFER_PDA
    );
    let native_sol_buffer = svm
        .get_account(&NATIVE_SOL_BUFFER_PDA)
        .expect("native SOL buffer should exist after initialize");
    assert_eq!(
        native_sol_buffer.owner, program_id,
        "the native SOL buffer is owned by the settlement so it can move its lamports",
    );
    assert!(
        native_sol_buffer.data.is_empty(),
        "the native SOL buffer holds no data"
    );
    assert_eq!(
        native_sol_buffer.lamports,
        svm.minimum_balance_for_rent_exemption(0),
        "the native SOL buffer must hold exactly the rent minimum",
    );
}

fn initialize_with_prefund(account: &Pubkey) {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    common::pda::assert_security_creation_survives_prefund(&mut svm, account, |svm| {
        let ix = Initialize {
            program_id,
            payer: INITIALIZER,
            manager: unique_pubkey(),
            solver_authority: unique_pubkey(),
            reclaim_authority: unique_pubkey(),
            settlement_owned_order_authority: unique_pubkey(),
        };
        common::initializer_tx(svm, ix)
    });
}

#[test]
fn initializes_state_pda_when_address_is_prefunded() {
    initialize_with_prefund(&STATE_PDA);
}

#[test]
fn initializes_native_sol_buffer_when_address_is_prefunded() {
    initialize_with_prefund(&NATIVE_SOL_BUFFER_PDA);
}

#[test]
fn initializer_funds_the_rent_while_another_account_pays_the_fee() {
    let (mut svm, program_id, fee_payer) = common::setup();
    svm.with_sigverify(false);

    let initializer_before = common::lamports(&svm, &INITIALIZER);

    let ix = Initialize {
        program_id,
        payer: INITIALIZER,
        manager: unique_pubkey(),
        solver_authority: unique_pubkey(),
        reclaim_authority: unique_pubkey(),
        settlement_owned_order_authority: unique_pubkey(),
    };
    // `common::initializer_tx`, but with `fee_payer` paying the fee.
    let blockhash = svm.latest_blockhash();
    let message = Message::new_with_blockhash(&[ix.into()], Some(&fee_payer.pubkey()), &blockhash);
    let mut tx = Transaction::new_unsigned(message);
    tx.partial_sign(&[&fee_payer], blockhash);
    svm.send_transaction(tx).expect("initialize should succeed");

    // The rent came out of the initializer, not the fee payer: the initializer paid
    // no transaction fee, so its balance dropped by exactly the PDAs' rent.
    let rent = svm.minimum_balance_for_rent_exemption(WIDTH_HEADER)
        + svm.minimum_balance_for_rent_exemption(0);
    assert_eq!(
        common::lamports(&svm, &INITIALIZER),
        initializer_before - rent,
        "initializer should have paid exactly the PDAs' rent",
    );
}

#[test]
fn rejects_a_payer_other_than_the_initializer() {
    // Signature checks stay on: the impostor signs for real.
    let (mut svm, program_id, payer) = common::setup();

    let ix = Initialize {
        program_id,
        payer: payer.pubkey(),
        manager: unique_pubkey(),
        solver_authority: unique_pubkey(),
        reclaim_authority: unique_pubkey(),
        settlement_owned_order_authority: unique_pubkey(),
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        InstructionError::MissingRequiredSignature,
    );
    assert!(svm.get_account(&STATE_PDA).is_none());
}

#[test]
fn rejects_the_initializer_as_a_nonsigner() {
    let (mut svm, program_id, payer) = common::setup();

    let mut ix: Instruction = Initialize {
        program_id,
        payer: INITIALIZER,
        manager: unique_pubkey(),
        solver_authority: unique_pubkey(),
        reclaim_authority: unique_pubkey(),
        settlement_owned_order_authority: unique_pubkey(),
    }
    .into();
    ix.accounts[0].is_signer = false;
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        InstructionError::MissingRequiredSignature,
    );
    assert!(svm.get_account(&STATE_PDA).is_none());
}

/// An `Initialize` against `program_id` that creates `state_pda`.
fn initialize_at(svm: &LiteSVM, program_id: Pubkey, state_pda: Pubkey) -> Transaction {
    initialize_with(svm, program_id, state_pda, NATIVE_SOL_BUFFER_PDA)
}

/// An `Initialize` against `program_id` that creates `state_pda` and
/// `native_sol_buffer`.
fn initialize_with(
    svm: &LiteSVM,
    program_id: Pubkey,
    state_pda: Pubkey,
    native_sol_buffer: Pubkey,
) -> Transaction {
    let ix = InitializeRaw {
        program_id,
        payer: INITIALIZER,
        state_pda,
        native_sol_buffer,
        manager: unique_pubkey(),
        solver_authority: unique_pubkey(),
        reclaim_authority: unique_pubkey(),
        settlement_owned_order_authority: unique_pubkey(),
    };
    common::initializer_tx(svm, ix)
}

#[test]
fn rejects_arbitrary_wrong_state_pda() {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    // The lower-level interface builder lets us point the instruction at a
    // deliberately wrong address.
    let wrong_pda = unique_pubkey();
    let tx = initialize_at(&svm, program_id, wrong_pda);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        SettlementError::StateAccountMismatch,
    );
    assert!(svm.get_account(&wrong_pda).is_none());
}

#[test]
fn rejects_arbitrary_wrong_native_sol_buffer() {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    let wrong_buffer = unique_pubkey();
    let tx = initialize_with(&svm, program_id, STATE_PDA, wrong_buffer);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_buffer);
    assert!(svm.get_account(&STATE_PDA).is_none());
}

#[test]
fn rejects_the_native_sol_buffer_of_a_non_canonical_bump() {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    let (_bump, noncanonical_buffer) =
        find_noncanonical_pda(&program_id, NATIVE_SOL_BUFFER_PDA_SEEDS);
    let tx = initialize_with(&svm, program_id, STATE_PDA, noncanonical_buffer);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &noncanonical_buffer);
    assert!(svm.get_account(&STATE_PDA).is_none());
}

#[test]
fn rejects_the_state_pda_of_a_non_canonical_bump() {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    // The lower-level interface builder lets us point the instruction at a
    // deliberately wrong address.
    let (noncanonical_bump, noncanonical_state_pda) =
        find_noncanonical_pda(&program_id, STATE_PDA_SEEDS);
    assert_ne!(noncanonical_bump, STATE_PDA_AND_BUMP.1);
    let tx = initialize_at(&svm, program_id, noncanonical_state_pda);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        SettlementError::StateAccountMismatch,
    );
    assert!(svm.get_account(&noncanonical_state_pda).is_none());
}

/// This is effectively a test that the STATE_PDA constant must be checked as expected by the
/// Initialize instruction, as changing the program ID changes the input to the instruction without
/// changing the actual constant value.
#[test]
fn rejects_the_state_pda_of_an_undeclared_program_id() {
    let (mut svm, _, _) = common::setup();
    svm.with_sigverify(false);

    let undeclared_id = unique_pubkey();
    svm.add_program_from_file(undeclared_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");
    let (derived_pda, _) = Pubkey::find_program_address(&STATE_PDA_SEEDS, &undeclared_id);

    let tx = initialize_at(&svm, undeclared_id, derived_pda);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        SettlementError::StateAccountMismatch,
    );
    assert!(svm.get_account(&derived_pda).is_none());
}

#[test]
fn rejects_the_pinned_state_pda_under_an_undeclared_program_id() {
    let (mut svm, _, _) = common::setup();
    svm.with_sigverify(false);

    let undeclared_id = unique_pubkey();
    svm.add_program_from_file(undeclared_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

    let tx = initialize_at(&svm, undeclared_id, STATE_PDA);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &STATE_PDA);
}

#[test]
fn rejects_initializing_twice() {
    let (mut svm, program_id, _) = common::setup();
    svm.with_sigverify(false);

    common::pda::assert_recreate_is_rejected(&mut svm, &STATE_PDA, |svm| {
        let ix = Initialize {
            program_id,
            payer: INITIALIZER,
            manager: unique_pubkey(),
            solver_authority: unique_pubkey(),
            reclaim_authority: unique_pubkey(),
            settlement_owned_order_authority: unique_pubkey(),
        };
        common::initializer_tx(svm, ix)
    });
}
