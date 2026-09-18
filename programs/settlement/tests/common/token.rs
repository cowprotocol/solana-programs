//! Token helpers for the settlement integration tests.

use crate::common::{active_token, token_2022::Extensions};

use super::{send_with_signers, unique_keypair, unique_pubkey};
use cow_settlement_client::cow_settlement_interface::{
    pda::state::find_state_pda, token_program::TokenProgram,
};
use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::{
    spl_token::{native_mint, state::Mint},
    CreateAssociatedTokenAccount,
};
use solana_program_pack::Pack;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction::create_account as system_create_account;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_token_2022_interface::{
    extension::{account_len::try_calculate_account_len_from_mint_data, StateWithExtensions},
    instruction::{
        approve, close_account, initialize_account3, initialize_mint2, mint_to as mint_to_ix,
        transfer_checked as transfer_checked_ix,
    },
    state::{Account, Mint as Mint2022},
};

/// The token program that owns `account`.
pub fn program_of(svm: &LiteSVM, account: &Pubkey) -> Pubkey {
    svm.get_account(account)
        .unwrap_or_else(|| panic!("{account} should exist on-chain"))
        .owner
}

/// Plant the native mint (wrapped SOL) at its well-known address, owned by the
/// legacy SPL Token program.
///
/// Every cluster carries this mint already; LiteSVM starts without it, so a test
/// that works with wrapped SOL has to put it there — a buffer is created under
/// the program that owns its mint, so the mint has to be readable. Its body is
/// what a real one holds: no authorities, no supply, and the native decimals.
pub fn create_native_mint(svm: &mut LiteSVM) {
    let mut data = vec![0u8; Mint::LEN];
    Mint {
        decimals: native_mint::DECIMALS,
        is_initialized: true,
        ..Default::default()
    }
    .pack_into_slice(&mut data);
    super::create_account_at(
        svm,
        native_mint::ID,
        &TokenProgram::SplToken.address(),
        &data,
    );
}

/// Create a fresh mint under [`active_token::program`], whose mint authority is
/// `payer`, and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    create_mint_at_under(
        svm,
        payer,
        &unique_keypair(),
        &active_token::program().address(),
        Extensions::default(),
    )
}

/// Create a mint with the specified instructions. Extensions are not added if the token program is SPL.
pub fn create_mint_with_extensions(
    svm: &mut LiteSVM,
    payer: &Keypair,
    extensions: Extensions,
) -> Pubkey {
    create_mint_at_under(
        svm,
        payer,
        &unique_keypair(),
        &active_token::program().address(),
        extensions,
    )
}

/// [`create_mint`] under `token_program` rather than under
/// [`active_token::program`], for the tests that build mints under both
/// programs at once.
pub fn create_mint_under(
    svm: &mut LiteSVM,
    payer: &Keypair,
    token_program: &Pubkey,
    extensions: Extensions,
) -> Pubkey {
    create_mint_at_under(svm, payer, &unique_keypair(), token_program, extensions)
}

/// Create a mint at `mint`'s address under `token_program`, whose mint authority
/// is `payer`, and return its address.
pub fn create_mint_at_under(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Keypair,
    token_program: &Pubkey,
    extensions: Extensions,
) -> Pubkey {
    /// `litesvm_token::CreateMint`'s default, kept so the two agree.
    const DECIMALS: u8 = 8;

    // The legacy program has no extensions to make room for, so its mints are
    // always the base length; Token-2022 insists on exactly the length the
    // extensions initialized below need.
    let space = if token_program == &TokenProgram::SplToken.address() {
        Mint::LEN
    } else {
        extensions.mint_len()
    };
    let mut instructions = vec![system_create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        svm.minimum_balance_for_rent_exemption(space),
        space as u64,
        token_program,
    )];

    if token_program != &TokenProgram::SplToken.address() {
        instructions.extend(extensions.initializers(&mint.pubkey(), &payer.pubkey()));
    }

    instructions.push(
        initialize_mint2(
            token_program,
            &mint.pubkey(),
            &payer.pubkey(),
            None,
            DECIMALS,
        )
        .expect("initialize_mint2 should build"),
    );

    send_with_signers(svm, payer, &[mint], &instructions)
        .unwrap_or_else(|error| panic!("mint creation should succeed: {error:?}"));
    mint.pubkey()
}

/// Close `mint`, whose close authority must be `payer`, refunding its rent to `payer`.
pub fn close_mint(svm: &mut LiteSVM, payer: &Keypair, mint: &Pubkey) {
    let ix = close_account(
        &TokenProgram::Token2022.address(),
        mint,
        &payer.pubkey(),
        &payer.pubkey(),
        &[],
    )
    .expect("close_account should build");
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[payer],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("closing the mint should succeed");
    assert!(
        svm.get_account(mint)
            .is_none_or(|account| account.data.is_empty()),
        "a closed mint must leave no data behind at its address",
    );
}

/// The length a token account for `mint` has to be allocated at.
///
/// The `token_program` should be the owner of the provided mint account.
fn token_account_len_for(svm: &LiteSVM, mint: &Pubkey, token_program: &Pubkey) -> usize {
    if token_program == &TokenProgram::SplToken.address() {
        return Account::LEN;
    }
    let data = svm
        .get_account(mint)
        .unwrap_or_else(|| panic!("{mint} should exist on-chain"))
        .data;
    try_calculate_account_len_from_mint_data(&data, &[])
        .expect("the mint should be a valid mint whose extensions have a fixed length")
}

/// Create an initialized SPL token account for `mint` whose SPL owner is
/// `owner`, funded by `payer`, and return its address. Each call produces a
/// fresh account, so the same `owner` can hold several accounts for one `mint`.
pub fn create_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    let token_program = program_of(svm, mint);
    let space = token_account_len_for(svm, mint, &token_program);
    let account = unique_keypair();
    let create = system_create_account(
        &payer.pubkey(),
        &account.pubkey(),
        svm.minimum_balance_for_rent_exemption(space),
        space as u64,
        &token_program,
    );
    let initialize = initialize_account3(&token_program, &account.pubkey(), mint, owner)
        .expect("initialize_account3 should build");
    send_with_signers(svm, payer, &[&account], &[create, initialize])
        .unwrap_or_else(|error| panic!("token account creation should succeed: {error:?}"));
    account.pubkey()
}

/// Create `owner`'s associated token account for `mint`, funded by `payer`, and
/// return its address. The address is the canonical ATA under `mint`'s own
/// program, so `transfer` can source from it without being told where the
/// tokens live.
pub fn create_associated_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    let token_program = program_of(svm, mint);
    CreateAssociatedTokenAccount::new(svm, payer, mint)
        .owner(owner)
        .token_program_id(&token_program)
        .send()
        .expect("associated token account creation should succeed")
}

/// Mint `amount` of `mint` into `destination`, signed by `payer` as the mint
/// authority. `payer` must be the authority `create_mint` assigned to the mint.
pub fn mint_to(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
) {
    let token_program = program_of(svm, mint);
    let instruction = mint_to_ix(
        &token_program,
        mint,
        destination,
        &payer.pubkey(),
        &[],
        amount,
    )
    .expect("mint_to should build");
    send_with_signers(svm, payer, &[], &[instruction])
        .unwrap_or_else(|error| panic!("mint_to should succeed: {error:?}"));
}

/// The decimals `mint` was created with.
///
/// Read through the extension layout, which covers a mint that has extensions
/// and one that doesn't alike, so this answers under either program.
fn decimals_of(svm: &LiteSVM, mint: &Pubkey) -> u8 {
    let account = svm
        .get_account(mint)
        .unwrap_or_else(|| panic!("{mint} should exist on-chain"));
    StateWithExtensions::<Mint2022>::unpack(&account.data)
        .expect("the mint should be a valid mint account")
        .base
        .decimals
}

/// Transfer `amount` of `mint` from `owner`'s associated token account into
/// `destination`, signed by `owner` as the source authority.
pub fn transfer(
    svm: &mut LiteSVM,
    owner: &Keypair,
    mint: &Pubkey,
    destination: &Pubkey,
    amount: u64,
) {
    let token_program = program_of(svm, mint);
    let source =
        get_associated_token_address_with_program_id(&owner.pubkey(), mint, &token_program);
    // Checked rather than plain `Transfer`: Token-2022 refuses the unchecked one
    // for a mint carrying a transfer fee, which [`Extensions::DEFAULT`] does, and
    // the legacy program accepts it just the same.
    let instruction = transfer_checked_ix(
        &token_program,
        &source,
        mint,
        destination,
        &owner.pubkey(),
        &[],
        amount,
        decimals_of(svm, mint),
    )
    .expect("transfer should build");
    send_with_signers(svm, owner, &[], &[instruction])
        .unwrap_or_else(|error| panic!("transfer should succeed: {error:?}"));
}

/// Approve `delegate` to spend up to `amount` from `source`. `owner` must be the
/// token owner of `source`. The transaction will be paid by the owner.
pub fn delegate(
    svm: &mut LiteSVM,
    owner: &Keypair,
    source: &Pubkey,
    delegate: &Pubkey,
    amount: u64,
) {
    let token_program = program_of(svm, source);
    let instruction = approve(
        &token_program,
        source,
        delegate,
        &owner.pubkey(),
        &[],
        amount,
    )
    .expect("approve should build");
    send_with_signers(svm, owner, &[], &[instruction])
        .unwrap_or_else(|error| panic!("approving a delegate should succeed: {error:?}"));
}

/// Re-plant `account`'s bytes at a fresh address under
/// the provided new_program and return it.
pub fn clone_under_new_program(
    svm: &mut LiteSVM,
    new_program: &Pubkey,
    account: &Pubkey,
) -> Pubkey {
    let data = svm
        .get_account(account)
        .unwrap_or_else(|| panic!("{account} should exist on-chain"))
        .data;
    let clone = unique_pubkey();
    super::create_account_at(svm, clone, new_program, &data);
    clone
}

/// Fund `sell_token` with `amount` of its mint and approve the settlement state
/// PDA as its delegate for the same `amount`, so the program can pull from it.
pub fn fund_and_delegate(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    sell_token: &Pubkey,
    amount: u64,
) {
    let mint = mint_of(svm, sell_token);
    mint_to(svm, payer, &mint, sell_token, amount);
    delegate(
        svm,
        payer,
        sell_token,
        &find_state_pda(program_id).0,
        amount,
    );
}

/// Read the token balance of `account`. The two programs share the base layout
/// this reads, so it answers for an account under either.
pub fn balance(svm: &LiteSVM, account: &Pubkey) -> u64 {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Account>(svm, account)
        .expect("account should exist and be a valid SPL token account")
        .amount
}

/// Read the amount `account` has delegated to its delegate.
pub fn delegated_amount(svm: &LiteSVM, account: &Pubkey) -> u64 {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Account>(svm, account)
        .expect("account should exist and be a valid SPL token account")
        .delegated_amount
}

/// Assert that no token instruction issued by the transaction references
/// `account`. Each token transfer the program performs is a CPI recorded in
/// `transaction.inner_instructions`. We can use that to check the token-program
/// instructions, so a settlement that must leave one side untouched can prove
/// no token instruction so much as named it.
///
/// Every supported program counts, not just the one the settlement was expected
/// to use: a transfer issued against the wrong one is exactly the kind of touch
/// this is meant to catch.
#[track_caller]
pub fn assert_no_token_instruction_touching(
    transaction: &TransactionMetadata,
    account_keys: &[Pubkey],
    account: &Pubkey,
) {
    for instruction in transaction
        .inner_instructions
        .iter()
        .flatten()
        .map(|inner| &inner.instruction)
    {
        let program = account_keys[usize::from(instruction.program_id_index)];
        if TokenProgram::try_from(&program).is_err() {
            continue;
        }
        let touches_account = instruction
            .accounts
            .iter()
            .any(|&index| account_keys[usize::from(index)] == *account);
        assert!(
            !touches_account,
            "expected no token instruction touching {account}, but one did",
        );
    }
}

/// Overwrite the account at `address` with a newly created account with the given parameters
pub fn overwrite_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    address: &Pubkey,
    mint: &Pubkey,
) {
    let token_program = program_of(svm, mint);
    let template = create_token_account(svm, payer, mint, &payer.pubkey());
    let data = svm
        .get_account(&template)
        .expect("the freshly created template exists")
        .data;
    super::create_account_at(svm, *address, &token_program, &data);
}

/// Read the mint that `account` holds tokens of.
pub fn mint_of(svm: &LiteSVM, account: &Pubkey) -> Pubkey {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Account>(svm, account)
        .expect("account should exist and be a valid SPL token account")
        .mint
}
