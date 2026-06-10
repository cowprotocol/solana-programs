//! SPL Token helpers for the settlement integration tests.

use litesvm::LiteSVM;
use litesvm_token::{
    spl_token::{native_mint, state::Mint},
    CreateMint,
};
use solana_sdk::{account::Account, program_pack::Pack, pubkey::Pubkey, signature::Keypair};

/// Create a fresh mint owned by `payer` and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    CreateMint::new(svm, payer)
        .send()
        .expect("mint creation should succeed")
}

/// Seed the SPL native-SOL mint (`So111…`) as a real token-program-owned mint.
///
/// LiteSVM doesn't include it by default. When creating a buffer, the Associated
/// Token Account program reads the mint (via the token program's
/// `GetAccountDataSize`) to size the account, so a native-mint buffer can only be
/// created if the native mint account exists — as it does on-chain.
pub fn seed_native_mint(svm: &mut LiteSVM) {
    // SPL Mint layout (82 bytes): [0..36] mint authority `COption` (None, all
    // zeros), [36..44] supply, [44] decimals, [45] is_initialized, [46..82]
    // freeze authority `COption` (None, all zeros). A None/None mint with the
    // native decimals is all we need.
    let mut data = vec![0u8; Mint::LEN];
    data[44] = native_mint::DECIMALS;
    data[45] = 1; // is_initialized = true

    // Convert the token-program addresses through their raw bytes so this
    // doesn't depend on the SPL crates' pubkey type matching `solana_sdk`'s.
    let native_mint_id = Pubkey::new_from_array(native_mint::ID.to_bytes());
    let token_program = Pubkey::new_from_array(litesvm_token::spl_token::ID.to_bytes());

    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        native_mint_id,
        Account {
            lamports,
            data,
            owner: token_program,
            executable: false,
            rent_epoch: 0,
        },
    )
    .expect("seeding the native mint should succeed");
}
