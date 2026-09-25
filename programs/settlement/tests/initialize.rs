use cow_settlement_client::cow_settlement_interface::{
    data::state::WIDTH_HEADER,
    instruction::initialize::Initialize as InitializeRaw,
    pda::state::{find_state_pda, STATE_PDA},
    SettlementError,
};
use cow_settlement_client::instruction::Initialize;
use cow_settlement_client::pda::state::DecodedStateAccount;
use litesvm::LiteSVM;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    unique_keypair, unique_pubkey, PROGRAM_SO,
};

mod common;

#[test]
fn happy_path_initializes_state_pda_with_expected_data() {
    let (mut svm, program_id, payer) = common::setup();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let manager = unique_pubkey();
    let reclaim_authority = unique_pubkey();
    let self_order_authority = unique_pubkey();

    // `payer` is both the transaction fee payer and the account funding the
    // state PDA's rent.
    let ix = Initialize {
        program_id,
        payer: payer.pubkey(),
        manager,
        reclaim_authority,
        self_order_authority,
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::Initialize)
        .expect("initialize should succeed");

    let account = svm
        .get_account(&state_pda)
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
            reclaim_authority,
            self_order_authority,
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
}

#[test]
fn initializes_state_pda_when_address_is_prefunded() {
    let (mut svm, program_id, payer) = common::setup();
    let (state_pda, _bump) = find_state_pda(&program_id);

    common::pda::assert_security_creation_survives_prefund(&mut svm, &state_pda, |svm| {
        let ix = Initialize {
            program_id,
            payer: payer.pubkey(),
            manager: unique_pubkey(),
            reclaim_authority: unique_pubkey(),
            self_order_authority: unique_pubkey(),
        };
        common::signed_tx(svm, &payer, &payer, ix)
    });
}

#[test]
fn funding_payer_can_differ_from_fee_payer() {
    let (mut svm, program_id, fee_payer) = common::setup();
    let (_, _bump) = find_state_pda(&program_id);

    let funder = unique_keypair();
    let funder_airdrop = 1_000_000_000;
    svm.airdrop(&funder.pubkey(), funder_airdrop)
        .expect("airdrop to funder should succeed");

    let ix = Initialize {
        program_id,
        payer: funder.pubkey(),
        reclaim_authority: unique_pubkey(),
        manager: unique_pubkey(),
        self_order_authority: unique_pubkey(),
    };
    let tx = common::signed_tx(&svm, &fee_payer, &funder, ix);
    svm.send_transaction(tx).expect("initialize should succeed");

    // The rent came out of the funder, not the fee payer: the funder paid no
    // transaction fee, so its balance dropped by exactly the PDA rent.
    let rent = svm.minimum_balance_for_rent_exemption(WIDTH_HEADER);
    assert_eq!(
        common::lamports(&svm, &funder.pubkey()),
        funder_airdrop - rent,
        "funder should have paid exactly the PDA rent",
    );
}

/// An `Initialize` against `program_id` that creates `state_pda`.
fn initialize_at(
    svm: &LiteSVM,
    program_id: Pubkey,
    payer: &Keypair,
    state_pda: Pubkey,
) -> Transaction {
    let ix = InitializeRaw {
        program_id,
        payer: payer.pubkey(),
        state_pda,
        reclaim_authority: unique_pubkey(),
        manager: unique_pubkey(),
        self_order_authority: unique_pubkey(),
    };
    common::signed_tx(svm, payer, payer, ix)
}

#[test]
fn rejects_arbitrary_wrong_state_pda() {
    let (mut svm, program_id, payer) = common::setup();

    // The lower-level interface builder lets us point the instruction at a
    // deliberately wrong address.
    let wrong_pda = unique_pubkey();
    let tx = initialize_at(&svm, program_id, &payer, wrong_pda);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        SettlementError::StateAccountMismatch,
    );
    assert!(svm.get_account(&wrong_pda).is_none());
}

/// This is effectively a test that the STATE_PDA constant must be checked as expected by the
/// Initialize instruction, as changing the program ID changes the input to the instruction without
/// changing the actual constant value.
#[test]
fn rejects_the_state_pda_of_an_undeclared_program_id() {
    let (mut svm, _, payer) = common::setup();
    let undeclared_id = unique_pubkey();
    svm.add_program_from_file(undeclared_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");
    let (derived_pda, _) = find_state_pda(&undeclared_id);

    let tx = initialize_at(&svm, undeclared_id, &payer, derived_pda);

    assert_instruction_error(
        svm.send_transaction(tx).map_err(|meta| meta.err),
        SettlementError::StateAccountMismatch,
    );
    assert!(svm.get_account(&derived_pda).is_none());
}

#[test]
fn rejects_the_pinned_state_pda_under_an_undeclared_program_id() {
    let (mut svm, _, payer) = common::setup();
    let undeclared_id = unique_pubkey();
    svm.add_program_from_file(undeclared_id, PROGRAM_SO)
        .expect("compiled program .so not found, run `just build-program` first");

    let tx = initialize_at(&svm, undeclared_id, &payer, STATE_PDA);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &STATE_PDA);
}

#[test]
fn rejects_initializing_twice() {
    let (mut svm, program_id, payer) = common::setup();
    let (state_pda, _bump) = find_state_pda(&program_id);

    common::pda::assert_recreate_is_rejected(&mut svm, &state_pda, |svm| {
        let ix = Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: unique_pubkey(),
            manager: unique_pubkey(),
            self_order_authority: unique_pubkey(),
        };
        common::signed_tx(svm, &payer, &payer, ix)
    });
}
