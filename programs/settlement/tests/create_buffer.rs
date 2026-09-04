use cow_settlement_client::cow_settlement_interface::{
    instruction::create_buffer::CreateBuffers as CreateBuffersRaw,
    pda::{
        buffer::{buffer_pda_seeds, find_buffer_pda},
        state::find_state_pda,
    },
};
use cow_settlement_client::instructions::CreateBuffers;
use cow_settlement_interface::token_program::TokenProgram;
use litesvm::LiteSVM;
use litesvm_token::{
    get_spl_account,
    spl_token::{
        native_mint,
        state::{Account as TokenAccount, AccountState},
    },
};
use solana_compute_budget::{
    compute_budget::ComputeBudget, compute_budget_limits::MAX_COMPUTE_UNIT_LIMIT,
};
use solana_sdk::{
    instruction::{Instruction, InstructionError},
    program_error::ProgramError,
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::TransactionError,
};

use crate::common::{
    benchmark::{send_transaction_metered, BenchLabel},
    token_2022::Extensions,
    unique_keypair, unique_pubkey,
};

mod common;

common::also_under_token_2022!(happy_path_creates_initialized_buffer_token_account);
#[test]
fn happy_path_creates_initialized_buffer_token_account() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);
    let (state_pda, _) = find_state_pda(&program_id);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::CreateBuffers)
        .expect("create_buffer should succeed");

    let account = svm
        .get_account(&buffer_pda)
        .expect("buffer PDA should exist after create_buffer");
    assert_eq!(
        account.owner,
        common::token::active().address(),
        "buffer must be owned by the token program it was created under"
    );
    assert_eq!(
        account.data.len(),
        common::token::buffer_len(),
        "buffer must be sized to a token account for its mint",
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

common::also_under_token_2022!(buffer_can_receive_tokens);
#[test]
fn buffer_can_receive_tokens() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("create_buffer should succeed");

    // Fund a sender by minting into its own token account, then have the sender
    // transfer those tokens into the buffer.
    let sender = unique_keypair();
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

// Legacy-only: the native mint is the legacy program's, and Token-2022
// recognizes only its own.
#[test]
fn happy_path_creates_native_token_buffer() {
    // `InitializeAccount` special-cases the native mint: it's recognized by key
    // and the buffer is initialized as a wrapped-SOL account. Since we fund
    // exactly the rent-exempt minimum, the wrapped balance starts at zero.
    let (mut svm, program_id, payer) = common::setup();
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &native_mint::ID);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[native_mint::ID],
    };
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

common::also_under_token_2022!(happy_path_creates_multiple_buffers_in_one_instruction);
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
        token_program: TokenProgram::SplToken,
        mints: &mints,
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::CreateBuffers)
        .expect("create_buffers should create every buffer at once");

    for mint in &mints {
        let (buffer_pda, _bump) = find_buffer_pda(&program_id, mint);
        let account = svm
            .get_account(&buffer_pda)
            .expect("each buffer PDA should exist after create_buffers");
        assert_eq!(
            account.owner,
            common::token::active().address(),
            "each buffer must be owned by the token program it was created under"
        );
        assert_eq!(
            account.data.len(),
            common::token::buffer_len(),
            "each buffer must be sized to a token account for its mint",
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

// Legacy-only: this fails while parsing, before the instruction so much as
// looks at its token program.
#[test]
fn rejects_no_buffers() {
    let (mut svm, program_id, payer) = common::setup();

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[],
    };
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

common::also_under_token_2022!(rejects_arbitrary_wrong_buffer_pda);
#[test]
fn rejects_arbitrary_wrong_buffer_pda() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);

    let wrong_pda = unique_pubkey();
    let ix = CreateBuffersRaw {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken.address(),
        buffers: &[(wrong_pda, mint)],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_pda);
}

common::also_under_token_2022!(rejects_non_canonical_bump_pda);
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
        token_program: TokenProgram::SplToken.address(),
        buffers: &[(non_canonical_pda, mint)],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &non_canonical_pda);
}

#[test]
fn rejects_unsupported_token_program() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    // Swap the token-program account for an arbitrary key.
    let mut ix: Instruction = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint],
    }
    .into();
    let token_program_index = 2;
    assert_eq!(
        ix.accounts[token_program_index].pubkey,
        TokenProgram::SplToken.address(),
        "sanity: should replace token program"
    );
    ix.accounts[token_program_index].pubkey = unique_pubkey();
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

common::also_under_token_2022!(rejects_invalid_mint);
#[test]
fn rejects_invalid_mint() {
    let (mut svm, program_id, payer) = common::setup();

    // An account that isn't an initialized SPL mint. The handler derives the
    // buffer PDA from it and delegates mint validation to InitializeAccount3,
    // which rejects it: a non-mint account isn't owned by the token program, so
    // the CPI fails with IncorrectProgramId after the buffer was allocated,
    // reverting the whole instruction. Sizing doesn't get in the way first: an
    // account this short can't be a mint with extensions, so it's sized at the
    // base layout without the token program being consulted.
    let not_a_mint = unique_pubkey();
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &not_a_mint);

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[not_a_mint],
    };
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

common::also_under_token_2022!(creates_buffer_when_address_is_prefunded);
#[test]
fn creates_buffer_when_address_is_prefunded() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    common::pda::assert_security_creation_survives_prefund(&mut svm, &buffer_pda, |svm| {
        let ix = CreateBuffers {
            program_id,
            payer: payer.pubkey(),
            token_program: TokenProgram::SplToken,
            mints: &[mint],
        };
        common::signed_tx(svm, &payer, &payer, ix)
    });
}

common::also_under_token_2022!(recreating_same_buffer_is_idempotent);
#[test]
fn recreating_same_buffer_is_idempotent() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    common::pda::assert_recreate_is_noop(&mut svm, &buffer_pda, |svm| {
        let ix = CreateBuffers {
            program_id,
            payer: payer.pubkey(),
            token_program: TokenProgram::SplToken,
            mints: &[mint],
        };
        common::signed_tx(svm, &payer, &payer, ix)
    });
}

common::also_under_token_2022!(batch_with_existing_buffer_passes_with_no_changes);
#[test]
fn batch_with_existing_buffer_passes_with_no_changes() {
    let (mut svm, program_id, payer) = common::setup();

    let existing = common::token::create_mint(&mut svm, &payer);
    let fresh = common::token::create_mint(&mut svm, &payer);

    // Pre-create a buffer for `existing`.
    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[existing],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("creating the first buffer should succeed");

    let (existing_buffer, _bump) = find_buffer_pda(&program_id, &existing);
    let before = svm
        .get_account(&existing_buffer)
        .expect("the existing buffer should exist before the batch");

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[fresh, existing],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("a batch containing an existing buffer should still succeed");

    let after = svm
        .get_account(&existing_buffer)
        .expect("the existing buffer should still exist after the batch");
    assert_eq!(
        before, after,
        "the existing buffer in the batch must be left unchanged"
    );

    // The other, fresh mint in the same batch must have been created.
    let (fresh_buffer, _bump) = find_buffer_pda(&program_id, &fresh);
    let fresh_account = get_spl_account::<TokenAccount>(&svm, &fresh_buffer)
        .expect("the fresh buffer must be an initialized token account after the batch");
    assert_eq!(
        fresh_account.state,
        AccountState::Initialized,
        "the fresh buffer must be initialized"
    );
}

common::also_under_token_2022!(one_failing_buffer_reverts_the_whole_batch);
#[test]
fn one_failing_buffer_reverts_the_whole_batch() {
    let (mut svm, program_id, payer) = common::setup();

    let fresh = common::token::create_mint(&mut svm, &payer);
    let not_a_mint = unique_pubkey();

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[fresh, not_a_mint],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    let err = svm
        .send_transaction(tx)
        .expect_err("a batch with an invalid mint must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::IncorrectProgramId)
        ),
        "expected instruction 0 to fail on the invalid mint, got {:?}",
        err.err,
    );

    let (fresh_buffer, _bump) = find_buffer_pda(&program_id, &fresh);
    assert!(
        svm.get_account(&fresh_buffer).is_none(),
        "the fresh buffer must be rolled back when a later buffer in the batch fails"
    );
}

common::also_under_token_2022!(same_mint_twice_in_one_instruction_is_idempotent);
#[test]
fn same_mint_twice_in_one_instruction_is_idempotent() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

    // Both pairs derive the same buffer PDA: the first iteration creates and
    // initializes it, the second sees it already owned by the token program and
    // is a no-op, so the instruction succeeds and the buffer exists once.
    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: TokenProgram::SplToken,
        mints: &[mint, mint],
    };
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("a batch listing the same mint twice should succeed");

    assert!(
        svm.get_account(&buffer_pda).is_some(),
        "the buffer must be created once when its mint is listed twice"
    );
}

#[test]
fn sizes_a_token_2022_buffer_to_the_extensions_its_mint_forces() {
    let (mut svm, program_id, payer) = common::setup();

    for extensions in [
        Extensions::CloseAuthorityOnly,
        Extensions::WithNonTransferable,
        Extensions::WithTransferFee,
    ] {
        let mint = common::token_2022::create_mint(&mut svm, &payer, &unique_keypair(), extensions);
        let (buffer_pda, _bump) = find_buffer_pda(&program_id, &mint);

        let ix = CreateBuffers {
            program_id,
            payer: payer.pubkey(),
            token_program: TokenProgram::Token2022,
            mints: &[mint],
        };
        let tx = common::signed_tx(&svm, &payer, &payer, ix);
        svm.send_transaction(tx).unwrap_or_else(|err| {
            panic!(
                "create_buffer should succeed for {extensions:?}: {:?}",
                err.err
            )
        });

        let account = svm
            .get_account(&buffer_pda)
            .expect("buffer PDA should exist after create_buffer");
        assert_eq!(
            account.owner,
            TokenProgram::Token2022.address(),
            "a {extensions:?} buffer must be owned by the Token-2022 program"
        );
        assert_eq!(
            account.data.len(),
            extensions.token_account_len(),
            "a {extensions:?} buffer must be allocated at the length its extensions imply"
        );
        common::assert_rent_exempt(&svm, &account);
    }
}

fn known_max_buffer_count() -> usize {
    match common::token::active() {
        TokenProgram::SplToken => 30,  // Limited by account limit
        TokenProgram::Token2022 => 21, // Limited by CPI call limit
    }
}

/// Largest number of buffers a single ALT-backed `create_buffers` transaction
/// can carry, bounded by the transaction account-lock limit (litesvm and current
/// mainnet both cap this at 64).
///
/// Nothing is created because the sent buffers are non-canonical and the
/// transaction errors out, so the probe leaves no state behind.
fn max_buffers_via_lookup_table(svm: &mut LiteSVM, program_id: &Pubkey, payer: &Keypair) -> usize {
    common::lookup_table::max_items_via_lookup_table(svm, |svm, n| {
        let buffers: Vec<(Pubkey, Pubkey)> =
            (0..n).map(|_| (unique_pubkey(), unique_pubkey())).collect();
        let ix = CreateBuffersRaw {
            program_id: *program_id,
            payer: payer.pubkey(),
            token_program: common::token::active().address(),
            buffers: &buffers,
        };
        common::lookup_table::lookup_table_tx(svm, payer, ix)
    })
}

common::also_under_token_2022!(bench_assert_known_max_buffer_count);
/// This isn't really a test, it's a way to make it visible that a code change
/// has changed the amount of buffer accounts that can be created in the same
/// transaction. If the number increases, great, bump it up! If it decreases and
/// you're ok with the performance hit, then you can bump it down.
#[test]
fn bench_assert_known_max_buffer_count() {
    let (mut svm, program_id, payer) = common::setup();
    let probe = max_buffers_via_lookup_table(&mut svm, &program_id, &payer);
    assert_eq!(
        probe,
        30,
        "the account-lock ceiling has changed under {:?}",
        common::token::active(),
    );
}

common::also_under_token_2022!(max_buffers_in_one_instruction);
/// Pack a single `create_buffers` instruction with as many buffers as a
/// transaction can have. Use Address Lookup Table to reach the real
/// account-lock ceiling. This is a ceiling on how many buffers one transaction
/// can create, and a benchmark for how much a maxed-out instruction costs.
#[test]
fn max_buffers_in_one_instruction() {
    let (mut svm, program_id, payer) = common::setup();
    // A maxed-out batch costs close to the 200k default and the exact cost
    // varies with the random mints' bump searches, so we increase the default
    // compute limit to avoid this edge case.
    // Not doing this leads to a flaky test, not an immediate failure: it works
    // most of the times but sometimes (~2%) execution would fail.
    svm = svm.with_compute_budget(ComputeBudget {
        compute_unit_limit: u64::from(MAX_COMPUTE_UNIT_LIMIT),
        // I'd like to use `ComputeBudget::default()` but it's behind a crate
        // feature `dev-context-only-utils` which has broken in the past after
        // updating Solana packages.
        // This is the closest we get to that without extra features.
        // `false` is about whether the feature SIMD-0268 is active; right now
        // this defaults to false and there isn't an easy way to get this from
        // the node itself in a test. Check with:
        //
        // ```
        // $ solana feature status 6TkHkRmP7JZy1fdM6fg5uXn76wChQBWGokHBJzrLB3mj
        // ```
        //
        // TODO: try to restore ``::default()` once Solana v4 gets more stable.
        ..ComputeBudget::new_with_defaults(false)
    });
    let (state_pda, _) = find_state_pda(&program_id);

    let probe = max_buffers_via_lookup_table(&mut svm, &program_id, &payer);
    // A legacy transaction tops out around 15 buffers (32-byte keys inlined into
    // a 1232-byte packet). The whole point of the lookup table is to beat that;
    // guard against a counterproductive use of lookup tables.
    assert!(
        probe > 15,
        "a lookup-table transaction must exceed the legacy packet limit, got {probe}"
    );
    let max_buffers = known_max_buffer_count();

    let mints: Vec<Pubkey> = (0..max_buffers)
        .map(|_| common::token::create_mint(&mut svm, &payer))
        .collect();

    let ix = CreateBuffers {
        program_id,
        payer: payer.pubkey(),
        token_program: common::token::active(),
        mints: &mints,
    };
    let tx = common::lookup_table::lookup_table_tx(&mut svm, &payer, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::CreateBuffers)
        .expect("a transaction filled to the buffer limit should succeed");

    for mint in &mints {
        let (buffer_pda, _bump) = find_buffer_pda(&program_id, mint);
        let token_account = get_spl_account::<TokenAccount>(&svm, &buffer_pda)
            .expect("each buffer must be an initialized token account");
        assert_eq!(token_account.mint, *mint, "each buffer must track its mint");
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
