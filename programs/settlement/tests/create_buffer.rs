use litesvm_token::{
    get_spl_account,
    spl_token::{
        native_mint,
        state::{Account as TokenAccount, AccountState},
    },
};
use settlement_client::instructions::{CreateBuffers, InstructionBuilding};
use settlement_client::settlement_interface::{
    instruction::create_buffer::{CreateBuffers as CreateBuffersRaw, SPL_TOKEN_PROGRAM_ID},
    pda::{
        buffer::{buffer_pda_seeds, find_buffer_pda},
        state::find_state_pda,
    },
};
use solana_sdk::{
    instruction::InstructionError,
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::TransactionError,
};

mod common;

#[test]
fn happy_path_creates_initialized_buffer_token_account() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);
    let (state_pda, _) = find_state_pda(&program_id);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[mint],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("create_buffer should succeed");

    let account = svm
        .get_account(&buffer_pda)
        .expect("buffer PDA should exist after create_buffer");
    assert_eq!(
        account.owner, SPL_TOKEN_PROGRAM_ID,
        "buffer must be owned by the SPL Token program"
    );
    assert_eq!(
        account.data.len(),
        TokenAccount::LEN,
        "buffer must be sized to a token account",
    );

    common::assert_rent_exempt(&svm, &account);

    let TokenAccount {
        mint: token_mint,
        owner,
        amount,
        delegate,
        state,
        is_native,
        delegated_amount,
        close_authority,
    } = get_spl_account::<TokenAccount>(&svm, &buffer_pda)
        .expect("buffer must be an initialized token account");
    assert_eq!(token_mint, mint, "buffer must track the given mint");
    assert_eq!(
        owner, state_pda,
        "buffer authority must be the settlement state PDA"
    );
    assert_eq!(amount, 0, "a fresh buffer must hold no tokens");
    assert!(delegate.is_none(), "a fresh buffer must have no delegate");
    assert_eq!(
        state,
        AccountState::Initialized,
        "buffer must be an initialized token account"
    );
    assert!(
        is_native.is_none(),
        "a buffer for a regular mint must not be native"
    );
    assert_eq!(
        delegated_amount, 0,
        "a fresh buffer must have no delegated amount"
    );
    assert!(
        close_authority.is_none(),
        "a fresh buffer must have no close authority"
    );
}

#[test]
fn buffer_can_receive_tokens() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[mint],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("create_buffer should succeed");

    // Fund a sender by minting into its own token account, then have the sender
    // transfer those tokens into the buffer.
    let sender = Keypair::new();
    svm.airdrop(&sender.pubkey(), 1_000_000_000)
        .expect("airdrop to sender should succeed");
    let sender_account =
        common::token::create_associated_token_account(&mut svm, &sender, &mint, &sender.pubkey());

    let amount = 1_000;
    common::token::mint_to(&mut svm, &payer, &mint, &sender_account, amount);
    common::token::transfer(&mut svm, &sender, &mint, &buffer_pda, amount);

    let token_account = get_spl_account::<TokenAccount>(&svm, &buffer_pda)
        .expect("buffer must be an initialized token account");
    assert_eq!(
        token_account.amount, amount,
        "buffer must hold the tokens transferred to it"
    );
}

#[test]
fn happy_path_creates_native_token_buffer() {
    // The native mint is special-cased by the token program: it's recognized by
    // key (no mint-account validation) and the buffer is initialized as a
    // wrapped-SOL account. Since we fund exactly the rent-exempt minimum, the
    // wrapped balance starts at zero.
    let (mut svm, program_id, payer) = common::setup();
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &native_mint::ID);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[native_mint::ID],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("create_buffer for the native mint should succeed");

    let token_account = get_spl_account::<TokenAccount>(&svm, &buffer_pda)
        .expect("buffer must be an initialized token account");
    assert_eq!(
        token_account.mint,
        native_mint::ID,
        "buffer must track the native mint"
    );
    assert!(
        token_account.is_native(),
        "a native-mint buffer must be marked native"
    );
    assert_eq!(
        token_account.amount, 0,
        "a native buffer funded at the rent minimum starts with zero wrapped balance"
    );
}

#[test]
fn happy_path_creates_multiple_buffers_in_one_instruction() {
    let (mut svm, program_id, payer) = common::setup();
    let (state_pda, _) = find_state_pda(&program_id);

    let mints: Vec<Pubkey> = (0..3)
        .map(|_| common::token::create_mint(&mut svm, &payer))
        .collect();

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &mints,
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("create_buffers should create every buffer at once");

    for mint in &mints {
        let (buffer_pda, _bump) = find_buffer_pda(&program_id, mint);
        let account = svm
            .get_account(&buffer_pda)
            .expect("each buffer PDA should exist after create_buffers");
        assert_eq!(
            account.owner, SPL_TOKEN_PROGRAM_ID,
            "each buffer must be owned by the SPL Token program"
        );
        assert_eq!(
            account.data.len(),
            TokenAccount::LEN,
            "each buffer must be sized to a token account",
        );
        common::assert_rent_exempt(&svm, &account);

        let token_account = get_spl_account::<TokenAccount>(&svm, &buffer_pda)
            .expect("each buffer must be an initialized token account");
        assert_eq!(token_account.mint, *mint, "buffer must track its mint");
        assert_eq!(
            token_account.owner, state_pda,
            "each buffer authority must be the settlement state PDA"
        );
        assert_eq!(
            token_account.state,
            AccountState::Initialized,
            "each buffer must be an initialized token account"
        );
    }
}

#[test]
fn rejects_no_buffers() {
    let (mut svm, program_id, payer) = common::setup();

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    let err = svm
        .send_transaction(tx)
        .expect_err("an instruction that creates no buffers must be rejected");
    let TransactionError::InstructionError(0, ix_err) = err.err else {
        panic!("expected instruction 0 to fail, got {:?}", err.err);
    };
    // Compare against the non-deprecated `ProgramError` variant the program
    // returns; naming the `InstructionError` variant directly would touch a
    // deprecated alias.
    assert_eq!(
        ProgramError::try_from(ix_err),
        Ok(ProgramError::NotEnoughAccountKeys),
    );
}

#[test]
fn rejects_arbitrary_wrong_buffer_pda() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);

    let wrong_pda = Pubkey::new_unique();
    let ix = CreateBuffersRaw {
        program_id,
        payer: payer.pubkey(),
        buffers: &[(wrong_pda, mint)],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_pda);
}

#[test]
fn rejects_non_canonical_bump_pda() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);

    // A buffer derivation that is valid for the seeds but not the canonical
    // address the program signs for.
    let (_bump, non_canonical_pda) =
        common::pda::find_noncanonical_pda(&program_id, buffer_pda_seeds(mint.as_array()));

    let ix = CreateBuffersRaw {
        program_id,
        payer: payer.pubkey(),
        buffers: &[(non_canonical_pda, mint)],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &non_canonical_pda);
}

#[test]
fn rejects_non_spl_token_program() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    // Swap the token-program account for an arbitrary key.
    let mut ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[mint],
    }
    .instruction();
    let token_program_index = 2;
    assert_eq!(
        ix.accounts[token_program_index].pubkey, SPL_TOKEN_PROGRAM_ID,
        "sanity: should replace token program"
    );
    ix.accounts[token_program_index].pubkey = Pubkey::new_unique();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    let err = svm
        .send_transaction(tx)
        .expect_err("a non-SPL-Token program must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::IncorrectProgramId)
        ),
        "expected instruction 0 to fail with IncorrectProgramId, got {:?}",
        err.err,
    );
    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer must not have been created"
    );
}

#[test]
fn rejects_invalid_mint() {
    let (mut svm, program_id, payer) = common::setup();

    // An account that isn't an initialized SPL mint. The handler derives the
    // buffer PDA from it and delegates mint validation to InitializeAccount3,
    // which rejects it: a non-mint account isn't owned by the token program, so
    // the CPI fails with IncorrectProgramId after the buffer was allocated,
    // reverting the whole instruction.
    let not_a_mint = Pubkey::new_unique();
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &not_a_mint);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[not_a_mint],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    let err = svm
        .send_transaction(tx)
        .expect_err("a non-mint account must be rejected");
    // Expected failing line:
    // https://github.com/solana-program/token/blob/7ed1aa8d9eb6d54c0084a9e8475c56a0a868b5bd/program/src/processor.rs#L115
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::IncorrectProgramId)
        ),
        "expected instruction 0 to fail on the invalid mint, got {:?}",
        err.err,
    );
    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "buffer must not have been created when the mint is invalid",
    );
}

#[test]
fn rejects_creating_same_buffer_twice() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[mint],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("first create_buffer should succeed");

    svm.expire_blockhash();

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[mint],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    common::pda::assert_rejected_as_existing(&mut svm, tx);
}

#[test]
fn one_failing_buffer_reverts_the_whole_batch() {
    let (mut svm, program_id, payer) = common::setup();

    // Pre-create a buffer for `existing`.
    let existing = common::token::create_mint(&mut svm, &payer);
    let fresh = common::token::create_mint(&mut svm, &payer);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[existing],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("creating the first buffer should succeed");

    // Batch the fresh mint *before* the already-existing one: the fresh buffer
    // would be allocated first, then the existing one fails. Because the
    // instruction is atomic, the whole batch reverts and the fresh buffer must
    // not survive.
    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[fresh, existing],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    common::pda::assert_rejected_as_existing(&mut svm, tx);

    let (fresh_buffer, _bump) = find_buffer_pda(&program_id, &fresh);
    assert!(
        svm.get_account(&fresh_buffer).is_none(),
        "the fresh buffer must be rolled back when a later buffer in the batch fails"
    );
}

#[test]
fn rejects_same_mint_twice_in_one_instruction() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);

    // Both pairs derive the same buffer PDA: the first creates it, the second
    // tries to recreate the now-existing account and fails, reverting the batch.
    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        mints: &[mint, mint],
    }
    .instruction();
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    common::pda::assert_rejected_as_existing(&mut svm, tx);

    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);
    assert!(
        svm.get_account(&buffer_pda).is_none(),
        "the buffer must not be created in a batch that creates it twice"
    );
}
