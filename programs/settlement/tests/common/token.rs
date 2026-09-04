//! Token helpers for the settlement integration tests.
//!
//! Every helper that acts on an existing token works under whichever token
//! program owns it, read back with [`program_of`], so a test settling
//! Token-2022 accounts uses the same calls as one settling legacy ones.
//!
//! Creating a mint is the one thing with nothing to read the program from.
//! [`create_mint`] takes it from [`active`], the program the running test is
//! exercising — the legacy one unless [`super::also_under_token_2022`]
//! generated the test — and [`create_mint_under`] names it outright, for the
//! tests that build mints under both at once.

use cow_settlement_client::cow_settlement_interface::{
    pda::state::find_state_pda, token_program::TokenProgram, Instruction,
};
use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::{
    spl_token::{
        instruction::{
            approve, initialize_account3, initialize_mint2, mint_to as mint_to_ix,
            transfer as transfer_ix,
        },
        native_mint,
        state::{Account, Mint},
    },
    CreateAssociatedTokenAccount, TOKEN_ID,
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
    extension::{transfer_fee::instruction::initialize_transfer_fee_config, ExtensionType},
    instruction::initialize_mint2 as initialize_mint2_2022,
    state::{Account as Account2022, Mint as Mint2022},
};
use std::cell::Cell;

use super::{unique_keypair, SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID};

thread_local! {
    /// The token program [`active`] reports, scoped to one test by
    /// [`under_token_program`]. Thread local because the test harness runs each
    /// test on its own thread, so a per-thread value is a per-test value.
    static ACTIVE: Cell<Pubkey> = const { Cell::new(SPL_TOKEN_PROGRAM_ID) };
}

/// The token program the running test exercises, which is what [`create_mint`]
/// creates under and what [`super::token_programs`] tells a settlement to
/// carry.
pub fn active() -> Pubkey {
    ACTIVE.with(Cell::get)
}

/// Run `test` with `token_program` as the [`active`] one.
///
/// [`super::also_under_token_2022`] is the way tests reach this; call it
/// directly only to nest a differently-programmed section inside a test.
pub fn under_token_program(token_program: Pubkey, test: impl FnOnce()) {
    ACTIVE.replace(token_program);
    test();
}

/// The token program that owns `account`.
///
/// A token account always lives under its mint's program, so this answers for a
/// mint and for the accounts holding it alike — which is what lets the helpers
/// below take the program from the tokens a test already built, rather than
/// from [`active`].
pub fn program_of(svm: &LiteSVM, account: &Pubkey) -> Pubkey {
    svm.get_account(account)
        .unwrap_or_else(|| panic!("{account} should exist on-chain"))
        .owner
}

/// Re-target a token instruction at `token_program`.
///
/// The SPL Token builders refuse to emit an instruction for any program but
/// their own, so the helpers below build against the legacy program and re-point
/// the result. Token-2022 encodes each of these instructions exactly as the
/// legacy program does — the same fact that lets the settlement program issue
/// one transfer against either — so only the program id needs replacing.
fn under(mut instruction: Instruction, token_program: &Pubkey) -> Instruction {
    instruction.program_id = *token_program;
    instruction
}

/// Submit `instructions` as one transaction signed by `payer` and `extra`.
fn send_token_tx(
    svm: &mut LiteSVM,
    payer: &Keypair,
    extra: &[&Keypair],
    instructions: &[Instruction],
    what: &str,
) {
    let mut signers = vec![payer];
    signers.extend_from_slice(extra);
    let tx = Transaction::new_signed_with_payer(
        instructions,
        Some(&payer.pubkey()),
        &signers,
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .unwrap_or_else(|error| panic!("{what} should succeed: {error:?}"));
}

/// Create a fresh mint under [`active`], whose mint authority is `payer`, and
/// return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    create_mint_at_under(svm, payer, &unique_keypair(), &active())
}

/// [`create_mint`] at `mint`'s address rather than a fresh one. Lets a test
/// reclaim an address a Token-2022 mint was just closed at, which is the only
/// way a legacy mint can end up where a Token-2022 one used to be.
pub fn create_mint_at(svm: &mut LiteSVM, payer: &Keypair, mint: &Keypair) -> Pubkey {
    create_mint_at_under(svm, payer, mint, &active())
}

/// [`create_mint`] under `token_program` rather than under [`active`], for the
/// tests that build mints under both programs at once.
pub fn create_mint_under(svm: &mut LiteSVM, payer: &Keypair, token_program: &Pubkey) -> Pubkey {
    create_mint_at_under(svm, payer, &unique_keypair(), token_program)
}

/// Create a mint at `mint`'s address under `token_program`, whose mint authority
/// is `payer`, and return its address. Every later helper reads the program back
/// off the mint, so the wrappers above are the only place a test names it.
fn create_mint_at_under(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Keypair,
    token_program: &Pubkey,
) -> Pubkey {
    /// `litesvm_token::CreateMint`'s default, kept so the two agree.
    const DECIMALS: u8 = 8;

    let create = system_create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        svm.minimum_balance_for_rent_exemption(Mint::LEN),
        Mint::LEN as u64,
        token_program,
    );
    // A mint with no extension data, which is every legacy mint and the shape a
    // Token-2022 mint takes when nothing asks for more. That is what keeps a
    // buffer for it at the base layout under either program; see
    // [`create_transfer_fee_mint`] for the other shape.
    let initialize = under(
        initialize_mint2(&TOKEN_ID, &mint.pubkey(), &payer.pubkey(), None, DECIMALS)
            .expect("initialize_mint2 should build"),
        token_program,
    );
    send_token_tx(svm, payer, &[mint], &[create, initialize], "mint creation");
    mint.pubkey()
}

/// Seed the wrapped-SOL mint account, which `LiteSVM` does not create.
pub fn create_native_mint(svm: &mut LiteSVM) {
    /// The native mint's fixed decimals, matching `spl_token::native_mint`.
    const DECIMALS: u8 = 9;

    let mut data = vec![0u8; Mint::LEN];
    Mint {
        mint_authority: None.into(),
        supply: 0,
        decimals: DECIMALS,
        is_initialized: true,
        freeze_authority: None.into(),
    }
    .pack_into_slice(&mut data);
    let token_program = Pubkey::new_from_array(TOKEN_ID.to_bytes());
    super::create_account_at(svm, native_mint::ID, &token_program, &data);
}

/// Create a Token-2022 mint carrying a transfer-fee config and return its
/// address.
///
/// This is the shape of mint that motivates mint-dependent buffer sizing: a
/// `TransferFeeConfig` on the mint requires a `TransferFeeAmount` on every
/// account holding it, so its token accounts must be longer than the base
/// layout. Not parameterized over the extension — one such mint is enough to
/// exercise the longer-than-base path.
///
/// Unlike [`create_mint_under`], this needs no retargeting: the Token-2022
/// interface crate's builders accept the Token-2022 program id directly.
pub fn create_transfer_fee_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    /// Matches [`create_mint_under`], so the two kinds of mint differ only in
    /// their extensions.
    const DECIMALS: u8 = 8;

    let length = mint_len_with_transfer_fee();
    let mint = unique_keypair();
    let create = system_create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        svm.minimum_balance_for_rent_exemption(length),
        length as u64,
        &TOKEN_2022_PROGRAM_ID,
    );
    // The fee config has to be initialized before the mint itself, while the
    // account is still uninitialized.
    let configure = initialize_transfer_fee_config(
        &TOKEN_2022_PROGRAM_ID,
        &mint.pubkey(),
        Some(&payer.pubkey()),
        Some(&payer.pubkey()),
        // Arbitrary: no test here transfers a fee-bearing token, it only needs
        // the extension to be present.
        50,
        1_000,
    )
    .expect("initialize_transfer_fee_config should build");
    let initialize = initialize_mint2_2022(
        &TOKEN_2022_PROGRAM_ID,
        &mint.pubkey(),
        &payer.pubkey(),
        None,
        DECIMALS,
    )
    .expect("initialize_mint2 should build");
    send_token_tx(
        svm,
        payer,
        &[&mint],
        &[create, configure, initialize],
        "transfer-fee mint creation",
    );
    mint.pubkey()
}

/// The data length of a mint carrying a transfer-fee config.
pub fn mint_len_with_transfer_fee() -> usize {
    ExtensionType::try_calculate_account_len::<Mint2022>(&[ExtensionType::TransferFeeConfig])
        .expect("a transfer-fee mint length should compute")
}

/// The data length a token account for [`create_transfer_fee_mint`]'s mint has
/// to be: the base layout plus the `TransferFeeAmount` the mint requires.
///
/// Computed from the Token-2022 interface rather than written as a literal, so
/// the expectation can't drift from what the token program actually wants.
pub fn token_account_len_with_transfer_fee() -> usize {
    ExtensionType::try_calculate_account_len::<Account2022>(&[ExtensionType::TransferFeeAmount])
        .expect("a transfer-fee token account length should compute")
}

/// Create an initialized token account for `mint` whose token owner is `owner`,
/// funded by `payer`, and return its address. The account is created under
/// `mint`'s own token program. Each call produces a fresh account, so the same
/// `owner` can hold several accounts for one `mint`.
///
/// Open-coded for the same reason as [`create_mint_under`]: the builder picks
/// the account address itself, and it would build against the legacy program
/// whatever the mint lives under.
pub fn create_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    let token_program = program_of(svm, mint);
    let account = unique_keypair();
    let create = system_create_account(
        &payer.pubkey(),
        &account.pubkey(),
        svm.minimum_balance_for_rent_exemption(Account::LEN),
        Account::LEN as u64,
        &token_program,
    );
    let initialize = under(
        initialize_account3(&TOKEN_ID, &account.pubkey(), mint, owner)
            .expect("initialize_account3 should build"),
        &token_program,
    );
    send_token_tx(
        svm,
        payer,
        &[&account],
        &[create, initialize],
        "token account creation",
    );
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
    let instruction = under(
        mint_to_ix(&TOKEN_ID, mint, destination, &payer.pubkey(), &[], amount)
            .expect("mint_to should build"),
        &token_program,
    );
    send_token_tx(svm, payer, &[], &[instruction], "mint_to");
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
    let instruction = under(
        transfer_ix(
            &TOKEN_ID,
            &source,
            destination,
            &owner.pubkey(),
            &[],
            amount,
        )
        .expect("transfer should build"),
        &token_program,
    );
    send_token_tx(svm, owner, &[], &[instruction], "transfer");
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
    let instruction = under(
        approve(&TOKEN_ID, source, delegate, &owner.pubkey(), &[], amount)
            .expect("approve should build"),
        &token_program,
    );
    send_token_tx(svm, owner, &[], &[instruction], "approving a delegate");
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
