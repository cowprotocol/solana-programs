//! SPL Token helpers for the settlement integration tests.

use litesvm::LiteSVM;
use litesvm_token::{CreateAssociatedTokenAccount, CreateMint, MintTo, Transfer};
use solana_sdk::{pubkey::Pubkey, signature::Keypair};

/// Create a fresh mint owned by `payer` and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    CreateMint::new(svm, payer)
        .send()
        .expect("mint creation should succeed")
}

/// Create `owner`'s associated token account for `mint` and return its address.
pub fn create_token_account(
    svm: &mut LiteSVM,
    payer: &Keypair,
    owner: &Pubkey,
    mint: &Pubkey,
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
