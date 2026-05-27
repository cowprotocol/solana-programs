//! PoC: how SPL Token account ownership gates transfers, and how
//! `SetAuthority(AccountOwner)` reassigns that gate.
//!
//! Standalone — runs the real SPL Token program that `LiteSVM::new()` loads by
//! default, so it neither builds nor loads the settlement `.so`.

use litesvm::LiteSVM;
use litesvm_token::{
    get_spl_account, spl_token::instruction::AuthorityType, spl_token::state::Account,
    CreateAssociatedTokenAccount, CreateMint, MintTo, SetAuthority, Transfer,
};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

fn balance(svm: &LiteSVM, token_account: &Pubkey) -> u64 {
    get_spl_account::<Account>(svm, token_account)
        .expect("token account should exist and decode")
        .amount
}

// There are three user (Alice, Bob, Sink) and a token COW. Alice (setup) has her
// associated token account for COW storing 1000 COW.
// - Alice transfers 100 COW to Sink. Expect success as she's the owner.
// - Bob sends 808 COW to Sink _from Alice's account. Expect failure as she's not the owner.
// - Alice calls SetAuthority on her associated token account and gives ownership of the account to Bob.
//   The account address stays derived from Alice's key, but its owner authority is now Bob.
// - The same two transfers from before are executed, except that success/failure is expected to be inverted.

#[test]
fn token_account_can_change_owner() {
    let mut svm = LiteSVM::new();

    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 1_000_000_000)
        .expect("airdrop to payer should succeed");

    let alice = Keypair::new();
    let bob = Keypair::new();
    let sink = Keypair::new();
    let alice_pk = alice.pubkey();
    let bob_pk = bob.pubkey();
    let sink_pk = sink.pubkey();

    // COW mint with 0 decimals, so amounts are literal whole tokens.
    let cow = CreateMint::new(&mut svm, &payer)
        .decimals(0)
        .send()
        .expect("create COW mint");

    // Alice's account starts with 1000 COW; Sink's starts empty. Both are the
    // mint's associated token account for their respective owner.
    let alice_acct = CreateAssociatedTokenAccount::new(&mut svm, &payer, &cow)
        .owner(&alice_pk)
        .send()
        .expect("create Alice associated token account");
    let sink_acct = CreateAssociatedTokenAccount::new(&mut svm, &payer, &cow)
        .owner(&sink_pk)
        .send()
        .expect("create Sink associated token account");

    MintTo::new(&mut svm, &payer, &cow, &alice_acct, 1000)
        .send()
        .expect("mint 1000 COW to Alice");
    assert_eq!(balance(&svm, &alice_acct), 1000);
    assert_eq!(balance(&svm, &sink_acct), 0);

    // 1. Alice (owner) transfers 100 -> success.
    Transfer::new(&mut svm, &payer, &cow, &sink_acct, 100)
        .source(&alice_acct)
        .owner(&alice)
        .send()
        .expect("Alice transfer should succeed while she owns the account");
    assert_eq!(balance(&svm, &alice_acct), 900);
    assert_eq!(balance(&svm, &sink_acct), 100);

    // 2. Bob (not owner) transfers 808 -> failure. 808 < 900, so the only
    //    possible failure cause is authority, not insufficient funds.
    Transfer::new(&mut svm, &payer, &cow, &sink_acct, 808)
        .source(&alice_acct)
        .owner(&bob)
        .send()
        .expect_err("Bob transfer should fail: he is not the account owner");
    assert_eq!(balance(&svm, &alice_acct), 900);
    assert_eq!(balance(&svm, &sink_acct), 100);

    // 3. Alice reassigns account ownership to Bob.
    SetAuthority::new(&mut svm, &payer, &alice_acct, AuthorityType::AccountOwner)
        .owner(&alice)
        .new_authority(&bob_pk)
        .send()
        .expect("Alice should be able to hand ownership to Bob");

    // Advance the blockhash before each transfer: steps 1/4a and 2/4b are
    // otherwise byte-identical transactions, and LiteSVM would reject the repeat
    // as `AlreadyProcessed` instead of running it (masking the real outcome).
    svm.expire_blockhash();

    // 4. Same two transfers, now with inverted outcomes.

    // 4a. Alice is no longer the owner -> failure.
    Transfer::new(&mut svm, &payer, &cow, &sink_acct, 100)
        .source(&alice_acct)
        .owner(&alice)
        .send()
        .expect_err("Alice transfer should now fail: she no longer owns the account");
    assert_eq!(balance(&svm, &alice_acct), 900);
    assert_eq!(balance(&svm, &sink_acct), 100);

    // 4b. Bob is now the owner -> success.
    Transfer::new(&mut svm, &payer, &cow, &sink_acct, 808)
        .source(&alice_acct)
        .owner(&bob)
        .send()
        .expect("Bob transfer should now succeed: he owns the account");
    assert_eq!(balance(&svm, &alice_acct), 92);
    assert_eq!(balance(&svm, &sink_acct), 908);
}
