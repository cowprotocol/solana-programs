//! SPL Token helpers for the settlement integration tests.

use litesvm::LiteSVM;
use litesvm_token::{CreateAccount, CreateMint};
use solana_sdk::{pubkey::Pubkey, signature::Keypair};

/// Create a fresh mint owned by `payer` and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    CreateMint::new(svm, payer)
        .send()
        .expect("mint creation should succeed")
}

/// Create an initialized SPL token account for `mint` whose SPL owner is
/// `owner`, funded by `payer`, and return its address.
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
