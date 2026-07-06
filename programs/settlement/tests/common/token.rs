//! SPL Token helpers for the settlement integration tests.

use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::{
    Approve, CreateAccount, CreateAssociatedTokenAccount, CreateMint, MintTo, Transfer,
};
use settlement_client::settlement_interface::pda::state::find_state_pda;
use solana_sdk::{pubkey::Pubkey, signature::Keypair};

/// Create a fresh mint owned by `payer` and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    CreateMint::new(svm, payer)
        .send()
        .expect("mint creation should succeed")
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
    CreateAccount::new(svm, payer, mint)
        .owner(owner)
        .send()
        .expect("token account creation should succeed")
}

/// Create `owner`'s associated token account for `mint`, funded by `payer`, and
/// return its address. The address is the canonical ATA, so `transfer` can
/// source from it without being told where the tokens live.
pub fn create_associated_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Pubkey {
    CreateAssociatedTokenAccount::new(svm, payer, mint)
        .owner(owner)
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
    MintTo::new(svm, payer, mint, destination, amount)
        .send()
        .expect("mint_to should succeed");
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
    Transfer::new(svm, owner, mint, destination, amount)
        .send()
        .expect("transfer should succeed");
}

/// Approve `delegate` to spend up to `amount` from `source`. `owner` must be the
/// SPL owner of `source`. The transaction will be paid by the owner.
pub fn delegate(
    svm: &mut LiteSVM,
    owner: &Keypair,
    source: &Pubkey,
    delegate: &Pubkey,
    amount: u64,
) {
    Approve::new(svm, owner, delegate, source, amount)
        .send()
        .expect("approving a delegate should succeed");
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

pub fn supply(svm: &LiteSVM, mint: &Pubkey) -> u64 {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Mint>(svm, mint)
        .expect("mint should exist and be a valid SPL token mint")
        .supply
}

/// Read the SPL token balance of `account`.
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

/// Assert that there was no token invocation in the transaction.
pub fn assert_no_spl_token_invocation(transaction: &TransactionMetadata) {
    let token_program = litesvm_token::spl_token::ID.to_string();
    assert!(
        !transaction
            .logs
            .iter()
            .any(|line| line.contains(&token_program) && line.contains("invoke")),
        "expected no SPL Token invocation, but one ran; full tx logs:\n{:#?}",
        transaction.logs,
    );
}

/// Read the mint that `account` holds tokens of.
pub fn mint_of(svm: &LiteSVM, account: &Pubkey) -> Pubkey {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Account>(svm, account)
        .expect("account should exist and be a valid SPL token account")
        .mint
}
