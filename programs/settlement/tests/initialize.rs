use pinocchio::Address;
use settlement_client::instructions::Initialize;
use settlement_client::settlement_interface::{
    data::state::EncodedStateAccount, instruction::initialize::Initialize as InitializeRaw,
    pda::state::find_state_pda,
};
use solana_sdk::signature::Signer;
use solana_sdk::pubkey::Pubkey;

use crate::common::{unique_keypair, unique_pubkey};

mod common;

#[test]
fn happy_path_initializes_state_pda_with_expected_data() {
    let (mut svm, program_id, payer) = common::setup();
    let (state_pda, _bump) = find_state_pda(&program_id);
    let reclaim_authority = Pubkey::new_unique();

    // `payer` is both the transaction fee payer and the account funding the
    // state PDA's rent.
    let ix = Initialize {
        program_id,
        payer: payer.pubkey(),
        reclaim_authority,
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx).expect("initialize should succeed");

    let account = svm
        .get_account(&state_pda)
        .expect("state PDA should exist after initialize");
    assert_eq!(
        account.owner, program_id,
        "state PDA must be owned by the settlement program"
    );
    assert_eq!(
        account.data[0],
        EncodedStateAccount::DISCRIMINATOR,
        "state PDA must hold the discriminator as the starting byte"
    );

    assert_eq!(
        &account.data[1..],
        &reclaim_authority.to_bytes()[..],
        "state PDA must store exactly the configured reclaim_authority"
    );

    let rent = svm.minimum_balance_for_rent_exemption(EncodedStateAccount::SIZE);
    assert_eq!(
        account.lamports, rent,
        "state PDA must hold exactly the rent minimum: {} != {}",
        account.lamports, rent,
    );
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
    };
    let tx = common::signed_tx(&svm, &fee_payer, &funder, ix);
    svm.send_transaction(tx).expect("initialize should succeed");

    // The rent came out of the funder, not the fee payer: the funder paid no
    // transaction fee, so its balance dropped by exactly the PDA rent.
    let rent = svm.minimum_balance_for_rent_exemption(EncodedStateAccount::SIZE);
    assert_eq!(
        common::lamports(&svm, &funder.pubkey()),
        funder_airdrop - rent,
        "funder should have paid exactly the PDA rent",
    );
}

#[test]
fn rejects_arbitrary_wrong_state_pda() {
    let (mut svm, program_id, payer) = common::setup();

    // The program only signs for the canonical PDA, so the lower-level interface
    // builder lets us point the instruction at a deliberately wrong address.
    let wrong_pda = unique_pubkey();
    let ix = InitializeRaw {
        program_id,
        payer: payer.pubkey(),
        state_pda: wrong_pda,
        reclaim_authority: unique_pubkey(),
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_pda);
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
        };
        common::signed_tx(svm, &payer, &payer, ix)
    });
}
