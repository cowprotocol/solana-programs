//! SPL Token helpers for the settlement integration tests.
//!
//! Every helper that acts on an existing token works under whichever token
//! program owns it, read back with [`program_of`], so a test settling
//! Token-2022 accounts uses the same calls as one settling legacy ones. Only
//! [`create_mint_under`] has to be told, there being nothing yet to read it
//! from.

use cow_settlement_client::cow_settlement_interface::{pda::state::find_state_pda, Instruction};
use litesvm::{types::TransactionMetadata, LiteSVM};
use litesvm_token::{
    spl_token::{
        instruction::{approve, initialize_account3, initialize_mint2, mint_to as mint_to_ix},
        state::{Account, Mint},
    },
    CreateAssociatedTokenAccount, Transfer, TOKEN_ID,
};
use solana_program_pack::Pack;
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use solana_system_interface::instruction::create_account as system_create_account;

use super::unique_keypair;

/// The token program that owns `account`.
///
/// A token account always lives under its mint's program, so this answers for a
/// mint and for the accounts holding it alike — which is what lets the helpers
/// below take the program from the tokens a test already built.
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

/// Create a fresh mint under the legacy SPL Token program, owned by `payer`,
/// and return its address.
pub fn create_mint(svm: &mut LiteSVM, payer: &Keypair) -> Pubkey {
    create_mint_under(svm, payer, &TOKEN_ID)
}

/// Create a fresh mint under `token_program`, whose mint authority is `payer`,
/// and return its address. Every later helper reads the program back off the
/// mint, so this is the only place a test names it.
///
/// This open-codes what [`litesvm_token::CreateMint`] does rather than calling
/// it, because that builder generates the mint keypair with `Keypair::new()`
/// internally and offers no way to supply one. A mint address is a seed of its
/// buffer PDA, so a random one makes buffer bumps — and the compute cost of
/// deriving them — vary between runs. See [`super::unique_pubkey`].
pub fn create_mint_under(svm: &mut LiteSVM, payer: &Keypair, token_program: &Pubkey) -> Pubkey {
    /// `litesvm_token::CreateMint`'s default, kept so the two agree.
    const DECIMALS: u8 = 8;

    let mint = unique_keypair();
    let create = system_create_account(
        &payer.pubkey(),
        &mint.pubkey(),
        svm.minimum_balance_for_rent_exemption(Mint::LEN),
        Mint::LEN as u64,
        token_program,
    );
    // A mint with no extension data, which is every legacy mint and the shape a
    // Token-2022 mint takes when nothing asks for more. That is what keeps a
    // buffer for it at the base layout under either program.
    let initialize = under(
        initialize_mint2(&TOKEN_ID, &mint.pubkey(), &payer.pubkey(), None, DECIMALS)
            .expect("initialize_mint2 should build"),
        token_program,
    );
    send_token_tx(svm, payer, &[&mint], &[create, initialize], "mint creation");
    mint.pubkey()
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

/// Read the mint that `account` holds tokens of.
pub fn mint_of(svm: &LiteSVM, account: &Pubkey) -> Pubkey {
    litesvm_token::get_spl_account::<litesvm_token::spl_token::state::Account>(svm, account)
        .expect("account should exist and be a valid SPL token account")
        .mint
}
