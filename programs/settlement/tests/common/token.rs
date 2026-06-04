//! SPL Token helpers for the settlement integration tests.

use litesvm::LiteSVM;
use litesvm_token::CreateMint;
use solana_sdk::{pubkey::Pubkey, signature::Keypair};

/// Create a fresh mint owned by `payer` and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    CreateMint::new(svm, payer)
        .send()
        .expect("mint creation should succeed")
}
