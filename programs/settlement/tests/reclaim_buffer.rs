use settlement_client::instructions::{Initialize, ReclaimBuffer};
use settlement_client::settlement_interface::{pda::state::find_state_pda, SettlementError};
use settlement_interface::Instruction;
use solana_sdk::signature::Signer;

use crate::common::buffer::ensure_buffer_exists;
use crate::common::state::initialize;

mod common;

#[test]
fn funded_buffer_is_skipped() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    // Fund the buffer with tokens
    let amount = 1_000;
    common::token::mint_to(&mut svm, &payer, &mint, &buffer_pda, amount);

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_some(),
        "buffer PDA should have been untouched despite transaction succeeding"
    );
}

#[test]
fn happy_path_reclaims_empty_buffer_to_the_authority_itself() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let reclaim_authority_lamports_before = common::lamports(&svm, &reclaim_authority.pubkey());

    // The authority names itself as the recipient: the same account appears
    // both as the read-only signer and as the writable recipient.
    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    assert_eq!(
        common::lamports(&svm, &reclaim_authority.pubkey()) - reclaim_authority_lamports_before,
        buffer_lamports_before,
        "reclaim_authority must receive exactly the buffer's rent lamports"
    );
}

#[test]
fn reclaims_to_a_recipient_chosen_by_the_authority() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();
    let recipient = common::unique_keypair().pubkey();

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let authority_lamports_before = common::lamports(&svm, &reclaim_authority.pubkey());
    let recipient_lamports_before = common::lamports(&svm, &recipient);

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: recipient,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    assert_eq!(
        common::lamports(&svm, &recipient) - recipient_lamports_before,
        buffer_lamports_before,
        "the chosen recipient must receive exactly the buffer's rent lamports"
    );
    assert_eq!(
        common::lamports(&svm, &reclaim_authority.pubkey()),
        authority_lamports_before,
        "reclaim_authority must not be credited when it named someone else"
    );
}

/// The recipient isn't required to be a system account: closing only moves
/// lamports, so a program-owned data account is credited just the same. The
/// settlement's own state PDA is the sharpest case, since it also occupies the
/// read-only `state_pda` slot of the very same instruction.
#[test]
fn reclaims_to_the_settlements_own_state_pda() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let (recipient, _bump) = find_state_pda(&program_id);

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let recipient_before = svm
        .get_account(&recipient)
        .expect("state PDA must exist before reclaim");

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: recipient,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    let recipient_after = svm
        .get_account(&recipient)
        .expect("state PDA must still exist");
    assert_eq!(
        recipient_after.lamports - recipient_before.lamports,
        buffer_lamports_before,
        "the state PDA must receive exactly the buffer's rent lamports"
    );
    assert_eq!(
        recipient_after.data, recipient_before.data,
        "crediting lamports must not touch the state PDA's data"
    );
}

#[test]
fn reclaims_multiple_buffers_skipping_funded() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let mint_a = common::token::create_mint(&mut svm, &payer);
    let mint_b = common::token::create_mint(&mut svm, &payer);
    let buffer_a = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint_a);
    let buffer_b = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint_b);

    // fund one of the buffers with tokens, leave the other empty
    common::token::mint_to(&mut svm, &payer, &mint_b, &buffer_b, 500);

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        mints: &[mint_a, mint_b],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_a).is_none(),
        "buffer_a must be closed"
    );
    assert!(
        svm.get_account(&buffer_b).is_some(),
        "buffer_b must not be closed (because its funded)"
    );
}

#[test]
fn rejects_when_signer_is_not_the_configured_reclaim_authority() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();
    let impostor = common::unique_keypair();
    svm.airdrop(&impostor.pubkey(), 1_000_000_000)
        .expect("airdrop should succeed");

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let mint = common::token::create_mint(&mut svm, &payer);
    ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    // Build the instruction as if `impostor` were the configured reclaim_authority.
    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: impostor.pubkey(),
        reclaim_recipient: impostor.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &impostor, ix);
    common::assert_settlement_error(
        0,
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::ReclaimAuthorityMismatch,
    );
}

/// Naming the configured authority isn't enough: it has to sign. The builder
/// always marks it as a signer, so this test strips the flag by hand.
#[test]
fn rejects_when_the_reclaim_authority_does_not_sign() {
    let (mut svm, program_id, payer) = common::setup();
    let reclaim_authority = common::unique_keypair();
    let recipient = common::unique_keypair().pubkey();

    initialize(
        &mut svm,
        &payer,
        Initialize {
            program_id,
            payer: payer.pubkey(),
            reclaim_authority: reclaim_authority.pubkey(),
        },
    );

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let mut ix = Instruction::from(ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: recipient,
        mints: &[mint],
    });

    // Clear the signer flag on the authority's meta. The recipient is a
    // different account, so exactly one meta names the authority. Leaving the
    // flag set and simply not signing would have the runtime reject the
    // transaction before the program runs; clearing it is what lets the
    // instruction reach the program's own check.
    let authority_meta = ix
        .accounts
        .iter_mut()
        .find(|meta| meta.pubkey == reclaim_authority.pubkey())
        .expect("instruction should reference the reclaim authority");
    authority_meta.is_signer = false;

    common::assert_settlement_error(
        0,
        common::send(&mut svm, &payer, vec![ix]),
        SettlementError::ReclaimAuthorityMismatch,
    );
    assert!(
        svm.get_account(&buffer_pda).is_some(),
        "buffer PDA must survive a reclaim the authority never signed"
    );
}
