use settlement_client::instructions::{CreateBuffers, Initialize, ReclaimBuffer};
use settlement_client::settlement_interface::{
    instruction::reclaim_buffer::ReclaimBuffer as ReclaimBufferRaw,
    pda::{buffer::find_buffer_pda, state::find_state_pda},
    SettlementError,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};
use spl_associated_token_account_interface::address::get_associated_token_address;

mod common;

/// Initialize the settlement state PDA with `receiver` as the configured
/// receiver.
fn initialize(svm: &mut litesvm::LiteSVM, program_id: &Pubkey, payer: &Keypair, receiver: Pubkey) {
    let ix = Initialize {
        program_id: *program_id,
        payer: payer.pubkey(),
        receiver,
    };
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
fn happy_path_reclaims_funded_buffer() {
    let (mut svm, program_id, payer) = common::setup();
    let receiver = Keypair::new();

    initialize(&mut svm, &program_id, &payer, receiver.pubkey());

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = create_buffer(&mut svm, &program_id, &payer, &mint);

    // Fund the buffer with tokens
    let amount = 1_000;
    common::token::mint_to(&mut svm, &payer, &mint, &buffer_pda, amount);

    // Pre-create the receiver's ATA: the program only validates its address,
    // it doesn't create it.
    let receiver_ata =
        common::token::create_associated_token_account(&mut svm, &payer, &mint, &receiver.pubkey());
    assert_eq!(
        receiver_ata,
        get_associated_token_address(&receiver.pubkey(), &mint),
        "sanity: helper should derive the canonical ATA"
    );

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let receiver_lamports_before = common::lamports(&svm, &receiver.pubkey());

    let ix = ReclaimBuffer {
        program_id,
        receiver: receiver.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &receiver, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    assert_eq!(
        common::token::supply(&svm, &mint),
        0,
        "reclaim_buffer should burn all tokens in the buffer"
    );
    assert_eq!(
        common::lamports(&svm, &receiver.pubkey()) - receiver_lamports_before,
        buffer_lamports_before,
        "receiver must receive exactly the buffer's rent lamports"
    );
}

#[test]
fn happy_path_reclaims_empty_buffer_without_token_transfer() {
    let (mut svm, program_id, payer) = common::setup();
    let receiver = Keypair::new();

    initialize(&mut svm, &program_id, &payer, receiver.pubkey());

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = create_buffer(&mut svm, &program_id, &payer, &mint);

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let receiver_lamports_before = common::lamports(&svm, &receiver.pubkey());

    let ix = ReclaimBuffer {
        program_id,
        receiver: receiver.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &receiver, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    assert_eq!(
        common::lamports(&svm, &receiver.pubkey()) - receiver_lamports_before,
        buffer_lamports_before,
        "receiver must receive exactly the buffer's rent lamports"
    );
}

#[test]
fn reclaims_multiple_buffers_in_one_instruction() {
    let (mut svm, program_id, payer) = common::setup();
    let receiver = Keypair::new();

    initialize(&mut svm, &program_id, &payer, receiver.pubkey());

    let mint_a = common::token::create_mint(&mut svm, &payer);
    let mint_b = common::token::create_mint(&mut svm, &payer);
    let buffer_a = create_buffer(&mut svm, &program_id, &payer, &mint_a);
    let buffer_b = create_buffer(&mut svm, &program_id, &payer, &mint_b);

    // fund one of the buffers with tokens, leave the other empty
    common::token::mint_to(&mut svm, &payer, &mint_b, &buffer_b, 500);

    let ix = ReclaimBuffer {
        program_id,
        receiver: receiver.pubkey(),
        mints: &[mint_a, mint_b],
    };
    let tx = common::signed_tx(&svm, &payer, &receiver, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_a).is_none(),
        "buffer_a must be closed"
    );
    assert!(
        svm.get_account(&buffer_b).is_none(),
        "buffer_b must be closed"
    );
    assert_eq!(
        common::token::supply(&svm, &mint_b),
        0,
        "buffer_b should have been empty and thus burned all tokens"
    );
}

#[test]
fn rejects_when_signer_is_not_the_configured_receiver() {
    let (mut svm, program_id, payer) = common::setup();
    let receiver = Keypair::new();
    let impostor = Keypair::new();
    svm.airdrop(&impostor.pubkey(), 1_000_000_000)
        .expect("airdrop should succeed");

    initialize(&mut svm, &program_id, &payer, receiver.pubkey());

    let mint = common::token::create_mint(&mut svm, &payer);
    create_buffer(&mut svm, &program_id, &payer, &mint);

    // Build the instruction as if `impostor` were the configured receiver.
    let ix = ReclaimBuffer {
        program_id,
        receiver: impostor.pubkey(),
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &impostor, ix);
    common::assert_settlement_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::ReceiverMismatch,
    );
}

#[test]
fn rejects_no_buffers() {
    let (mut svm, program_id, payer) = common::setup();
    let receiver = Keypair::new();
    initialize(&mut svm, &program_id, &payer, receiver.pubkey());

    let (state_pda, _) = find_state_pda(&program_id);
    let ix = ReclaimBufferRaw {
        program_id,
        state_pda,
        receiver: receiver.pubkey(),
        buffers: &[],
    };
    let tx = common::signed_tx(&svm, &payer, &receiver, ix);
    assert!(
        svm.send_transaction(tx).is_err(),
        "an instruction that reclaims no buffers must be rejected"
    );
}
