use litesvm_token::{
    get_spl_account,
    spl_token::{
        native_mint,
        state::{Account as TokenAccount, AccountState},
    },
};
use settlement_client::instructions::create_buffer;
use settlement_client::settlement_interface::{
    instruction::create_buffer::create_buffer as create_buffer_ix,
    pda::{buffer::find_buffer_pda, state::find_state_pda},
    SPL_TOKEN_PROGRAM_ID,
};
use solana_sdk::{
    instruction::InstructionError, program_pack::Pack, pubkey::Pubkey, signature::Signer,
    transaction::TransactionError,
};

mod common;

#[test]
fn happy_path_creates_initialized_buffer_token_account() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = find_buffer_pda(&program_id, &mint);
    let (state_pda, _) = find_state_pda(&program_id);

    let ix = create_buffer(&program_id, &payer.pubkey(), &mint);
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
fn happy_path_creates_native_token_buffer() {
    // The native mint is special-cased by the token program: it's recognized by
    // key (no mint-account validation) and the buffer is initialized as a
    // wrapped-SOL account. Since we fund exactly the rent-exempt minimum, the
    // wrapped balance starts at zero.
    let (mut svm, program_id, payer) = common::setup();
    common::token::seed_native_mint(&mut svm);
    let buffer_pda = find_buffer_pda(&program_id, &native_mint::ID);

    let ix = create_buffer(&program_id, &payer.pubkey(), &native_mint::ID);
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
fn rejects_arbitrary_wrong_buffer_pda() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let (state_pda, _) = find_state_pda(&program_id);

    // A buffer address that isn't the state PDA's ATA for the mint. The ATA
    // program re-derives the expected address and rejects the mismatch.
    let wrong_pda = Pubkey::new_unique();
    let ix = create_buffer_ix(&program_id, &payer.pubkey(), &wrong_pda, &state_pda, &mint);
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    let err = svm
        .send_transaction(tx)
        .expect_err("a non-ATA buffer address must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::InvalidSeeds)
        ),
        "expected instruction 0 to fail with InvalidSeeds, got {:?}",
        err.err,
    );
    assert!(
        svm.get_account(&wrong_pda).is_none(),
        "rejected buffer must not have been created"
    );
}

#[test]
fn rejects_non_spl_token_program() {
    let (mut svm, program_id, payer) = common::setup();
    let mint = common::token::create_mint(&mut svm, &payer);
    let buffer_pda = find_buffer_pda(&program_id, &mint);

    // Swap the token-program account for an arbitrary key.
    let mut ix = create_buffer(&program_id, &payer.pubkey(), &mint);
    let token_program_index = 5;
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

    // An account that isn't an initialized SPL mint. Mint validation is
    // delegated down the CPI chain: the Associated Token Account program reads
    // the mint to size the account, and the token program ultimately requires a
    // token-program-owned mint. A non-mint account isn't token-program-owned, so
    // the CPI fails with IncorrectProgramId and the whole instruction reverts —
    // the buffer is never created.
    let not_a_mint = Pubkey::new_unique();
    let buffer_pda = find_buffer_pda(&program_id, &not_a_mint);

    let ix = create_buffer(&program_id, &payer.pubkey(), &not_a_mint);
    let tx = common::signed_tx(&svm, &payer, &payer, ix);

    let err = svm
        .send_transaction(tx)
        .expect_err("a non-mint account must be rejected");
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

    let ix = create_buffer(&program_id, &payer.pubkey(), &mint);
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    svm.send_transaction(tx)
        .expect("first create_buffer should succeed");

    svm.expire_blockhash();

    let ix = create_buffer(&program_id, &payer.pubkey(), &mint);
    let tx = common::signed_tx(&svm, &payer, &payer, ix);
    // The Associated Token Account program's (non-idempotent) `Create` requires
    // the target to still be system-owned; the existing buffer is already owned
    // by the token program, so it rejects the second creation with IllegalOwner.
    let err = svm
        .send_transaction(tx)
        .expect_err("recreating an existing buffer must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::IllegalOwner)
        ),
        "expected instruction 0 to fail with IllegalOwner, got {:?}",
        err.err,
    );
}
