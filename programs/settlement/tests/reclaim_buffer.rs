use cow_settlement_client::instructions::ReclaimBuffer;
use cow_settlement_interface::token_program::TokenProgram;
use cow_settlement_interface::Instruction;
use cow_settlement_interface::{
    instruction::reclaim_buffer::ReclaimBuffer as ReclaimBufferRaw, pda::buffer::find_buffer_pda,
    pda::state::find_state_pda, SettlementError,
};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use crate::common::benchmark::{send_transaction_metered, BenchLabel};
use crate::common::buffer::ensure_buffer_exists;
use crate::common::token_2022::Extensions;
use crate::common::{
    assert_instruction_error, to_instruction_error, unique_pubkey, InitializedParams,
};

mod common;

common::also_under_token_2022!(happy_path_reclaims_to_a_recipient_chosen_by_the_authority);
#[test]
fn happy_path_reclaims_to_a_recipient_chosen_by_the_authority() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();
    let recipient = common::unique_keypair().pubkey();

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
        token_program: TokenProgram::SplToken,
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

common::also_under_token_2022!(happy_path_reclaims_empty_buffer_to_the_authority_itself);
#[test]
fn happy_path_reclaims_empty_buffer_to_the_authority_itself() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let reclaim_authority_lamports_before = common::lamports(&svm, &reclaim_authority.pubkey());

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::ReclaimBuffer)
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

common::also_under_token_2022!(funded_buffer_is_skipped);
#[test]
fn funded_buffer_is_skipped() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let amount = 1_000;
    common::token::mint_to(&mut svm, &payer, &mint, &buffer_pda, amount);

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::ReclaimBuffer)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_some(),
        "buffer PDA should have been untouched despite transaction succeeding"
    );
}

common::also_under_token_2022!(reclaims_to_the_settlements_own_state_pda);
/// The recipient isn't required to be a system account: closing only moves
/// lamports, so a program-owned data account is credited just the same. The
/// settlement's own state PDA is the sharpest case, since it also occupies the
/// read-only `state_pda` slot of the very same instruction.
#[test]
fn reclaims_to_the_settlements_own_state_pda() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

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
        token_program: TokenProgram::SplToken,
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

common::also_under_token_2022!(reclaims_multiple_buffers_skipping_funded);
#[test]
fn reclaims_multiple_buffers_skipping_funded() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

    let mint_a = common::token::create_mint(&mut svm, &payer);
    let mint_b = common::token::create_mint(&mut svm, &payer);
    let buffer_a = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint_a);
    let buffer_b = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint_b);

    // Only `buffer_b` is funded; `buffer_a` stays empty and closable.
    common::token::mint_to(&mut svm, &payer, &mint_b, &buffer_b, 500);

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint_a, mint_b],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::ReclaimBuffer)
        .expect("reclaim_buffer should succeed");

    assert!(
        svm.get_account(&buffer_a).is_none(),
        "buffer_a must be closed"
    );
    assert!(
        svm.get_account(&buffer_b).is_some(),
        "buffer_b must not be closed (because it's funded)"
    );
}

common::also_under_token_2022!(rejects_the_same_buffer_twice_in_one_instruction);
#[test]
fn rejects_the_same_buffer_twice_in_one_instruction() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

    let mint = common::token::create_mint(&mut svm, &payer);
    ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);
    let recipient = common::unique_keypair().pubkey();

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: recipient,
        token_program: TokenProgram::SplToken,
        mints: &[mint, mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        InstructionError::InvalidAccountData,
    );
}

common::also_under_token_2022!(rejects_when_signer_is_not_the_configured_reclaim_authority);
#[test]
fn rejects_when_signer_is_not_the_configured_reclaim_authority() {
    let (
        mut svm,
        InitializedParams {
            program_id, payer, ..
        },
    ) = common::setup_init();
    let impostor = common::unique_keypair();
    svm.airdrop(&impostor.pubkey(), 1_000_000_000)
        .expect("airdrop should succeed");

    let mint = common::token::create_mint(&mut svm, &payer);
    ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    // Build the instruction as if `impostor` were the configured reclaim_authority.
    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: impostor.pubkey(),
        reclaim_recipient: impostor.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &impostor, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        to_instruction_error(SettlementError::ReclaimAuthorityMismatch),
    );
}

common::also_under_token_2022!(rejects_when_the_reclaim_authority_does_not_sign);
/// Naming the configured authority isn't enough: it has to sign. The builder
/// always marks it as a signer, so this test strips the flag by hand.
#[test]
fn rejects_when_the_reclaim_authority_does_not_sign() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();
    let recipient = common::unique_keypair().pubkey();

    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);

    let mut ix = Instruction::from(ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: recipient,
        token_program: TokenProgram::SplToken,
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

    assert_instruction_error(
        common::send(&mut svm, &payer, vec![ix]),
        to_instruction_error(SettlementError::ReclaimAuthorityMismatch),
    );
    assert!(
        svm.get_account(&buffer_pda).is_some(),
        "buffer PDA must survive a reclaim the authority never signed"
    );
}

/// Set up a Token-2022 mint with a close authority and its buffer, then close
/// the mint so `reopen` can claim the address. Returns the mint and its buffer.
fn buffer_whose_mint_was_reopened(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    reopen: impl FnOnce(&mut LiteSVM, &Keypair, &Keypair),
) -> (Pubkey, Pubkey) {
    let mint_keypair = common::unique_keypair();
    let mint =
        common::token_2022::create_mint(svm, payer, &mint_keypair, Extensions::CloseAuthorityOnly);
    let buffer_pda = common::buffer::ensure_buffer_exists(svm, program_id, payer, &mint);

    common::token_2022::close_mint(svm, payer, &mint);
    reopen(svm, payer, &mint_keypair);

    (mint, buffer_pda)
}

#[test]
fn reclaims_a_buffer_whose_mint_was_reopened_with_another_extension() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

    let (mint, buffer_pda) = buffer_whose_mint_was_reopened(
        &mut svm,
        &program_id,
        &payer,
        |svm, payer, mint_keypair| {
            common::token_2022::create_mint(
                svm,
                payer,
                mint_keypair,
                Extensions::WithNonTransferable,
            );
        },
    );

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let recipient_before = common::lamports(&svm, &reclaim_authority.pubkey());

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        token_program: TokenProgram::Token2022,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed for a reopened mint");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    assert_eq!(
        common::lamports(&svm, &reclaim_authority.pubkey()) - recipient_before,
        buffer_lamports_before,
        "the rent of a buffer stranded by a reopened mint must still be recoverable"
    );
}

#[test]
fn reclaims_a_buffer_whose_mint_was_reopened_as_a_legacy_mint() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();

    let (mint, buffer_pda) = buffer_whose_mint_was_reopened(
        &mut svm,
        &program_id,
        &payer,
        |svm, payer, mint_keypair| {
            common::token::create_mint_at(svm, payer, mint_keypair);
        },
    );
    assert_eq!(
        svm.get_account(&mint)
            .expect("the reopened mint should exist")
            .owner,
        TokenProgram::SplToken.address(),
        "sanity: the mint must now belong to the legacy program"
    );

    let buffer_lamports_before = svm
        .get_account(&buffer_pda)
        .expect("buffer must exist before reclaim")
        .lamports;
    let recipient_before = common::lamports(&svm, &reclaim_authority.pubkey());

    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        token_program: TokenProgram::Token2022,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &reclaim_authority, ix);
    svm.send_transaction(tx)
        .expect("reclaim_buffer should succeed for a mint reopened as legacy");

    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer PDA must be closed after reclaim"
    );
    assert_eq!(
        common::lamports(&svm, &reclaim_authority.pubkey()) - recipient_before,
        buffer_lamports_before,
        "the rent of a buffer whose mint turned legacy must still be recoverable"
    );
}

fn max_buffers_reclaim_via_lookup_table(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    reclaim_authority: &Keypair,
) -> usize {
    let (state_pda, _bump) = find_state_pda(program_id);
    common::lookup_table::max_items_via_lookup_table(svm, |svm, n| {
        let buffers: Vec<(Pubkey, Pubkey)> =
            (0..n).map(|_| (unique_pubkey(), unique_pubkey())).collect();
        let ix = ReclaimBufferRaw {
            program_id: *program_id,
            state_pda,
            reclaim_authority: reclaim_authority.pubkey(),
            reclaim_recipient: reclaim_authority.pubkey(),
            token_program: common::token::active().address(),
            buffers: &buffers,
        };
        common::lookup_table::lookup_table_tx(svm, reclaim_authority, ix)
    })
}

common::also_under_token_2022!(bench_assert_known_max_buffer_count);
/// This isn't really a test, it's a way to make it visible that a code change
/// has changed the amount of buffer accounts that can be reclaimed in the same
/// transaction. If the number increases, great, bump it up! If it decreases and
/// you're ok with the performance hit, then you can bump it down.
#[test]
fn bench_assert_known_max_buffer_count() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();
    svm.airdrop(&reclaim_authority.pubkey(), 100_000_000)
        .expect("airdrop should succeed");
    let max_buffers =
        max_buffers_reclaim_via_lookup_table(&mut svm, &program_id, &reclaim_authority);

    assert_eq!(
        max_buffers,
        30,
        "Max buffers that can be reclaimed has changed under {:?}",
        common::token::active(),
    );
}

common::also_under_token_2022!(max_buffers_in_one_instruction);
/// Pack a single `reclaim_buffer` instruction with as many buffers as a
/// transaction can have, all of them empty and therefore closable. Use an
/// Address Lookup Table to reach the real account-lock ceiling. This is a
/// ceiling on how many buffers one transaction can reclaim, and a benchmark for
/// how much a maxed-out instruction costs.
#[test]
fn max_buffers_in_one_instruction() {
    let (
        mut svm,
        InitializedParams {
            program_id,
            payer,
            reclaim: reclaim_authority,
            ..
        },
    ) = common::setup_init();
    svm.airdrop(&reclaim_authority.pubkey(), 100_000_000)
        .expect("airdrop should succeed");
    let max_buffers =
        max_buffers_reclaim_via_lookup_table(&mut svm, &program_id, &reclaim_authority);

    assert!(
        max_buffers > 15,
        "a lookup-table transaction must exceed the legacy packet limit, got {max_buffers}"
    );

    let mints: Vec<Pubkey> = (0..max_buffers)
        .map(|_| {
            let mint = common::token::create_mint(&mut svm, &payer);
            ensure_buffer_exists(&mut svm, &program_id, &payer, &mint);
            mint
        })
        .collect();

    let reclaimable_rent: u64 = mints
        .iter()
        .map(|mint| {
            svm.get_account(&find_buffer_pda(&program_id, mint).0)
                .expect("every buffer must exist before the reclaim")
                .lamports
        })
        .sum();

    let authority_before = common::lamports(&svm, &reclaim_authority.pubkey());
    let ix = ReclaimBuffer {
        program_id,
        reclaim_authority: reclaim_authority.pubkey(),
        reclaim_recipient: reclaim_authority.pubkey(),
        token_program: common::token::active(),
        mints: &mints,
    };
    let tx = common::lookup_table::lookup_table_tx(&mut svm, &reclaim_authority, ix);
    let txresult = send_transaction_metered(&mut svm, tx, BenchLabel::ReclaimBuffer)
        .expect("a transaction filled to the buffer limit should succeed");

    for mint in &mints {
        let (buffer_pda, _bump) = find_buffer_pda(&program_id, mint);
        assert!(
            svm.get_account(&buffer_pda).is_none(),
            "every buffer in a maxed-out batch must be closed"
        );
    }
    assert_eq!(
        common::lamports(&svm, &reclaim_authority.pubkey()) + txresult.fee - authority_before,
        reclaimable_rent,
        "the recipient must receive the rent of every closed buffer"
    );
}
