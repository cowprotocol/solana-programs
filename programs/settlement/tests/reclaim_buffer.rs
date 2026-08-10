use settlement_client::instructions::{CreateBuffers, Initialize, ReclaimBuffer};
use settlement_client::settlement_interface::{
    pda::{buffer::find_buffer_pda, state::find_state_pda},
    SettlementError,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

mod common;

/// Send `ix` as the settlement's `Initialize`, signed by `payer`. Taken by
/// value because building the instruction consumes the builder; `payer` is
/// separate because the builder only carries its address, not its keypair.
fn initialize(svm: &mut litesvm::LiteSVM, payer: &Keypair, ix: Initialize) {
    let tx = common::signed_tx(svm, payer, payer, ix);
    svm.send_transaction(tx).expect("initialize should succeed");
}

/// Create a buffer for `mint`, return its PDA.
fn create_buffer(
    svm: &mut litesvm::LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    mint: &Pubkey,
) -> Pubkey {
    let (buffer_pda, _bump) = find_buffer_pda(program_id, mint);
    let ix = CreateBuffers {
        program_id: *program_id,
        payer: payer.pubkey(),
        mints: &[*mint],
    };
    let tx = common::signed_tx(svm, payer, payer, ix);
    svm.send_transaction(tx)
        .expect("create_buffer should succeed");
    buffer_pda
}

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
    let buffer_pda = create_buffer(&mut svm, &program_id, &payer, &mint);

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
    let buffer_pda = create_buffer(&mut svm, &program_id, &payer, &mint);

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
    let buffer_pda = create_buffer(&mut svm, &program_id, &payer, &mint);

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
    let buffer_pda = create_buffer(&mut svm, &program_id, &payer, &mint);

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
    let buffer_a = create_buffer(&mut svm, &program_id, &payer, &mint_a);
    let buffer_b = create_buffer(&mut svm, &program_id, &payer, &mint_b);

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
    create_buffer(&mut svm, &program_id, &payer, &mint);

    // Build the instruction as if `impostor` were the configured reclaim_authority.
    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: impostor.pubkey(),
        reclaim_recipient: impostor.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &impostor, ix);
    common::assert_settlement_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::ReclaimAuthorityMismatch,
    );
}
