//! Address Lookup Table helpers for the settlement integration tests.

use std::borrow::Cow;

use litesvm::LiteSVM;
use solana_address_lookup_table_interface::{
    program as address_lookup_table_program,
    state::{AddressLookupTable, LookupTableMeta},
};
use solana_sdk::{
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{TransactionError, VersionedTransaction},
};

/// Seed an Address Lookup Table holding `addresses` directly into the SVM,
/// bypassing the lookup-table program normal flow of first running the Create
/// and then Extend instructions, so that the result is usable in the same slot
/// it's seeded.
///
/// A transaction may only resolve addresses that were added before its
/// `last_extended_slot. A default table has `last_extended_slot = 0` and
/// LiteSVM's genesis slot is 0, so if we were to just send the transaction
/// creating the lookup table, we'd have to artificially bump up the slot number
/// on every test. Instead of that, we just preallocate the ALT table account.
///
/// Returns the in-memory handle `Message::try_compile` uses to assign indices;
/// its `addresses` mirror the on-chain account so the indices line up.
fn seed_lookup_table(svm: &mut LiteSVM, addresses: Vec<Pubkey>) -> AddressLookupTableAccount {
    let meta = LookupTableMeta {
        // All new accounts should be seen as already extended.
        last_extended_slot_start_index: u8::try_from(addresses.len())
            .expect("lookup table address count fits in a u8"),
        ..LookupTableMeta::default()
    };
    let data = AddressLookupTable {
        meta,
        addresses: Cow::Borrowed(&addresses),
    }
    .serialize_for_tests()
    .expect("lookup table serializes");
    let key = super::create_account(svm, &address_lookup_table_program::id(), &data);
    AddressLookupTableAccount { key, addresses }
}

/// Build a transaction that runs the input instruction with as many of its
/// accounts as possible resolved through an Address Lookup Table. We hand the
/// table every account `ix` touches and let `try_compile` sort out what's
/// eligible: it keeps the fee payer, any other signers, and the program id in the
/// static keys (the runtime forbids loading those from a table) and pulls only
/// the rest from the table. So this works for any instruction, regardless of
/// where the payer sits in its account list. Compressing those accounts into
/// 1-byte indices is what lets an instruction's account list grow past the legacy
/// packet limit, up to the account-lock ceiling.
///
/// This code is loosely based on:
/// <https://solana.com/developers/guides/advanced/lookup-tables#how-to-use-an-address-lookup-table-in-a-transaction>
pub fn lookup_table_tx(
    svm: &mut LiteSVM,
    payer: &Keypair,
    ix: impl Into<Instruction>,
) -> VersionedTransaction {
    let ix = ix.into();
    let table_addresses = ix.accounts.iter().map(|meta| meta.pubkey).collect();
    let lookup_table = seed_lookup_table(svm, table_addresses);
    let message = v0::Message::try_compile(
        &payer.pubkey(),
        &[ix],
        &[lookup_table],
        svm.latest_blockhash(),
    )
    .expect("v0 message compiles");
    VersionedTransaction::try_new(VersionedMessage::V0(message), &[payer])
        .expect("versioned transaction signs")
}

/// Largest `n` an ALT-backed transaction built by `build_tx` can carry, bounded
/// by the transaction account-lock limit (litesvm and current mainnet both cap
/// this at 64).
///
/// `build_tx` is asked for the transaction handling `n` items and must build one
/// the program rejects (throwaway, non-canonical PDAs, say), so that a probe
/// which does reach execution leaves no state behind.
pub fn max_items_via_lookup_table(
    svm: &mut LiteSVM,
    mut build_tx: impl FnMut(&mut LiteSVM, usize) -> VersionedTransaction,
) -> usize {
    let mut n: usize = 1;
    loop {
        let tx = build_tx(svm, n);
        match svm.send_transaction(tx) {
            // Over the lock limit: rejected at sanitization, before executing.
            Err(failed) if failed.err == TransactionError::TooManyAccountLocks => {
                return n
                    .checked_sub(1)
                    .expect("Overflow means zero accounts are too many")
            }
            // Within the limit: the transaction reached the program, which, as
            // expected, rejected it.
            Err(failed) if matches!(failed.err, TransactionError::InstructionError(..)) => {
                n = n.strict_add(1)
            }
            // Anything else (an unexpected error, or a transaction that somehow
            // succeeded) means the probe's own setup is broken, not a real signal
            // about the limit. Fail loudly rather than miscount.
            other => panic!("unexpected result probing {n} items: {other:?}"),
        }
    }
}
