//! SPL Token helpers for the settlement integration tests.

use cow_settlement_client::cow_settlement_interface::pda::state::find_state_pda;
use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::{
    spl_token::{instruction::initialize_mint2, native_mint, state::Mint},
    Approve, CreateAccount, CreateAssociatedTokenAccount, MintTo, Transfer, TOKEN_ID,
};
use solana_program_pack::Pack;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction::create_account as system_create_account;

use super::unique_keypair;

/// Create a fresh mint owned by `payer` and return its address.
///
/// This open-codes what [`litesvm_token::CreateMint`] does rather than calling
/// it, because that builder generates the mint keypair with `Keypair::new()`
/// internally and offers no way to supply one. A mint address is a seed of its
/// buffer PDA, so a random one makes buffer bumps — and the compute cost of
/// deriving them — vary between runs. See [`super::unique_pubkey`].
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    /// `litesvm_token::CreateMint`'s default, kept so the two agree.
    const DECIMALS: u8 = 8;

    let mint = unique_keypair();
    let create = system_create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        svm.minimum_balance_for_rent_exemption(Mint::LEN),
        Mint::LEN as u64,
        &TOKEN_ID,
    );
    let initialize = initialize_mint2(&TOKEN_ID, &mint.pubkey(), &payer.pubkey(), None, DECIMALS)
        .expect("initialize_mint2 should build");
    let tx = Transaction::new_signed_with_payer(
        &[create, initialize],
        Some(&payer.pubkey()),
        &[payer, &mint],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx)
        .expect("mint creation should succeed");
    mint.pubkey()
}

/// Seed the wrapped-SOL mint account, which `LiteSVM` does not create.
///
/// On chain the native mint is a real, token-program-owned mint account, and
/// the program queries it with `GetAccountDataSize` to size a buffer. That
/// query rejects an account the token program does not own, so a test touching
/// the native mint has to put the account there first.
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
        // Without this the builder generates the address with `Keypair::new()`;
        // see [`create_mint`].
        .account_kp(unique_keypair())
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

/// Assert that no SPL Token instruction issued by the transaction references
/// `account`. Each token transfer the program performs is a CPI recorded in
/// `transaction.inner_instructions`. We can use that to check the token-program
/// instructions, so a settlement that must leave one side untouched can prove
/// no token instruction so much as named it.
#[track_caller]
pub fn assert_no_token_instruction_touching(
    transaction: &TransactionMetadata,
    account_keys: &[Pubkey],
    account: &Pubkey,
) {
    let token_program = Pubkey::new_from_array(litesvm_token::spl_token::ID.to_bytes());
    for instruction in transaction
        .inner_instructions
        .iter()
        .flatten()
        .map(|inner| &inner.instruction)
    {
        if account_keys[usize::from(instruction.program_id_index)] != token_program {
            continue;
        }
        let touches_account = instruction
            .accounts
            .iter()
            .any(|&index| account_keys[usize::from(index)] == *account);
        assert!(
            !touches_account,
            "expected no SPL Token instruction touching {account}, but one did",
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
    let template = create_token_account(svm, payer, mint, &payer.pubkey());
    let data = svm
        .get_account(&template)
        .expect("the freshly created template exists")
        .data;
    let token_program = Pubkey::new_from_array(TOKEN_ID.to_bytes());
    super::create_account_at(svm, *address, &token_program, &data);
}

/// Read the mint that `account` holds tokens of.
pub fn mint_of(svm: &LiteSVM, account: &Pubkey) -> Pubkey {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Account>(svm, account)
        .expect("account should exist and be a valid SPL token account")
        .mint
}
