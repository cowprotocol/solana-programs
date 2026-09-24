use cow_settlement_client::cow_settlement_interface::{
    data::{
        intent::{EncodedOrderIntent, Flags, OrderIntent},
        order::SIZE,
    },
    instruction::{cancel_order::CancelOrder, create_order::CreateOrder},
    pda::order::find_order_pda,
    Instruction, SettlementError,
};
use cow_settlement_client::pda::order::DecodedOrderAccount;
use solana_sdk::{
    instruction::InstructionError,
    pubkey::Pubkey,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::common::{
    benchmark::{send_transaction_metered, BenchLabel},
    order::{create_order_pda, read_order, sample_intent},
    signed_tx, unique_keypair, unique_pubkey,
};

mod common;

fn encode_and_derive(
    intent: &OrderIntent,
    program_id: &Pubkey,
) -> ([u8; EncodedOrderIntent::SIZE], Pubkey, u8) {
    let encoded = EncodedOrderIntent::from(intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, bump) = find_order_pda(program_id, &encoded.hash());
    (bytes, pda, bump)
}

/// The canonical encoded body of `intent`'s order once cancelled, funded by
/// `created_by` and otherwise unfilled.
fn expected_cancelled_body(intent: &OrderIntent, bump: u8, created_by: Pubkey) -> [u8; SIZE] {
    DecodedOrderAccount {
        bump,
        cancelled: true,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by,
        intent: intent.clone(),
    }
    .encode()
}

#[test]
fn cancels_existing_active_order() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, bump) = encode_and_derive(&intent, &program_id);

    let created_by = unique_keypair();
    svm.airdrop(&created_by.pubkey(), 1_000_000_000)
        .expect("airdrop to created_by should succeed");
    let create = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: created_by.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let create_tx = Transaction::new_signed_with_payer(
        &[create.into()],
        Some(&owner.pubkey()),
        &[&owner, &created_by],
        svm.latest_blockhash(),
    );
    svm.send_transaction(create_tx)
        .expect("create_order should succeed");
    assert!(!read_order(&svm, &pda).cancelled, "order must start active");

    // Cancel it with a `created_by` unrelated to both the owner and the order's
    // creator: for an existing order it's ignored, and in particular must not
    // overwrite the stored creator.
    let unrelated = unique_keypair();
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: unrelated.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };
    let tx = Transaction::new_signed_with_payer(
        &[cancel.into()],
        Some(&owner.pubkey()),
        &[&owner, &unrelated],
        svm.latest_blockhash(),
    );
    send_transaction_metered(&mut svm, tx, BenchLabel::CancelOrder)
        .expect("cancel_order should succeed");

    let account = svm
        .get_account(&pda)
        .expect("order PDA must remain after cancellation");
    assert_eq!(account.owner, program_id, "PDA must stay program-owned");
    assert_eq!(
        account.data,
        expected_cancelled_body(&intent, bump, created_by.pubkey()),
        "account data doesn't match after cancellation"
    );
    let order = read_order(&svm, &pda);
    assert!(order.cancelled, "order must be cancelled");
    assert_eq!(
        order.created_by,
        created_by.pubkey(),
        "cancellation must preserve the original created_by"
    );
    common::assert_rent_exempt(&svm, &account);
}

#[test]
fn cancels_existing_order_without_created_by_signature() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, bump) = encode_and_derive(&intent, &program_id);

    create_order_pda(&mut svm, &program_id, &owner, &intent);

    // The builder marks `created_by` a signer by default, but the program only
    // needs that signature to create. Drop it to show cancelling an existing
    // order doesn't require it, even with a `created_by` that isn't the owner.
    let created_by = unique_pubkey();
    let mut cancel = Instruction::from(CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by,
        order_pda: pda,
        intent_bytes: Some(encoded),
    });
    for meta in &mut cancel.accounts {
        if meta.pubkey == created_by {
            meta.is_signer = false;
        }
    }
    svm.send_transaction(signed_tx(&svm, &owner, &owner, cancel))
        .expect("cancelling an existing order must not require created_by to sign");

    // The stored `created_by` stays the original creator, not the account
    // passed to the cancellation.
    let account = svm.get_account(&pda).expect("order PDA must remain");
    assert_eq!(
        account.data,
        expected_cancelled_body(&intent, bump, owner.pubkey()),
        "account data doesn't match after cancellation"
    );
}

#[test]
fn cancels_existing_order_without_intent_bytes() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (_encoded, pda, bump) = encode_and_derive(&intent, &program_id);

    create_order_pda(&mut svm, &program_id, &owner, &intent);
    assert!(!read_order(&svm, &pda).cancelled, "order must start active");

    // Omit the intent bytes entirely: for an order that already exists, the
    // instruction recovers its data from the PDA. `created_by` is unused here
    // and doesn't sign in this mode.
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: unique_pubkey(),
        order_pda: pda,
        intent_bytes: None,
    };
    let tx = Transaction::new_signed_with_payer(
        &[cancel.into()],
        Some(&owner.pubkey()),
        &[&owner],
        svm.latest_blockhash(),
    );
    send_transaction_metered(&mut svm, tx, BenchLabel::CancelOrder)
        .expect("cancelling an existing order must not require its intent bytes");

    // The recovered order is cancelled and otherwise byte-for-byte its old self,
    // including the original creator.
    let account = svm.get_account(&pda).expect("order PDA must remain");
    assert_eq!(
        account.data,
        expected_cancelled_body(&intent, bump, owner.pubkey()),
        "recovering from the PDA must cancel it while preserving its stored body"
    );
}

#[test]
fn rejects_intentless_cancellation_of_nonexistent_order() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (_encoded, pda, _bump) = encode_and_derive(&intent, &program_id);
    assert!(
        svm.get_account(&pda).is_none(),
        "the order must not exist yet"
    );

    // With neither intent bytes nor an existing PDA there's nothing to recover
    // and nothing to create from, so the cancellation must be rejected rather
    // than leaving an order behind.
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: unique_pubkey(),
        order_pda: pda,
        intent_bytes: None,
    };
    let err = svm
        .send_transaction(signed_tx(&svm, &owner, &owner, cancel))
        .expect_err("an intent-less cancellation of a missing order must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::UninitializedAccount),
    );
    assert!(
        svm.get_account(&pda).is_none(),
        "a rejected cancellation must not leave a PDA behind"
    );
}

#[test]
fn creates_cancelled_pda_for_nonexistent_order() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, bump) = encode_and_derive(&intent, &program_id);
    assert!(
        svm.get_account(&pda).is_none(),
        "the order must not exist yet"
    );

    // A `created_by` distinct from the owner funds and signs for the new PDA.
    let created_by = unique_keypair();
    svm.airdrop(&created_by.pubkey(), 1_000_000_000)
        .expect("airdrop to created_by should succeed");
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: created_by.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };
    let tx = Transaction::new_signed_with_payer(
        &[cancel.into()],
        Some(&owner.pubkey()),
        &[&owner, &created_by],
        svm.latest_blockhash(),
    );
    send_transaction_metered(&mut svm, tx, BenchLabel::CancelOrder)
        .expect("cancel_order should create the order already cancelled");

    let account = svm
        .get_account(&pda)
        .expect("cancelling a missing order must create its PDA, not delete it");
    assert_eq!(account.owner, program_id, "PDA must be program-owned");
    assert_eq!(
        account.data,
        expected_cancelled_body(&intent, bump, created_by.pubkey()),
        "the created order must be cancelled and record created_by"
    );
    common::assert_rent_exempt(&svm, &account);
}

#[test]
fn a_cancelled_order_cannot_be_uncancelled_by_creating_it() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, bump) = encode_and_derive(&intent, &program_id);

    // Cancel an order that doesn't exist yet: this creates its PDA already
    // cancelled.
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };
    svm.send_transaction(signed_tx(&svm, &owner, &owner, cancel))
        .expect("cancel_order should create the order already cancelled");
    let cancelled = svm
        .get_account(&pda)
        .expect("order PDA must exist after cancellation");
    assert_eq!(
        cancelled.data,
        expected_cancelled_body(&intent, bump, owner.pubkey()),
        "the missing order must be created already cancelled",
    );

    // Creating the same order now must be rejected: its PDA already exists, and
    // the rejection must leave the cancelled flag set. Otherwise a signed intent
    // could be un-cancelled by racing a creation in after the cancellation.
    let create = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let err = svm
        .send_transaction(signed_tx(&svm, &owner, &owner, create))
        .expect_err("creating an order that's already been cancelled must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::AccountAlreadyInitialized),
    );

    let after = svm.get_account(&pda).expect("order PDA must still exist");
    assert_eq!(
        cancelled, after,
        "a rejected creation must leave the cancelled order byte-for-byte unchanged",
    );
}

#[test]
fn creating_cancelled_order_requires_created_by_signature() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    // The order doesn't exist, so this takes the create-cancelled path, which
    // funds the PDA's rent from `created_by`. Without `created_by`'s signature
    // that funding isn't authorized, so the instruction must fail.
    let created_by = unique_pubkey();
    let mut cancel = Instruction::from(CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by,
        order_pda: pda,
        intent_bytes: Some(encoded),
    });
    for meta in &mut cancel.accounts {
        if meta.pubkey == created_by {
            meta.is_signer = false;
        }
    }
    let err = svm
        .send_transaction(signed_tx(&svm, &owner, &owner, cancel))
        .expect_err("creating a cancelled order without created_by's signature must fail");
    // The rent-funding CPI to the System program rejects the unsigned funder.
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation),
    );
    assert!(
        svm.get_account(&pda).is_none(),
        "no order PDA may be left behind by a rejected creation"
    );
}

#[test]
fn cancelling_is_idempotent() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    let cancel = || CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };

    // First cancellation creates the order cancelled.
    svm.send_transaction(signed_tx(&svm, &owner, &owner, cancel()))
        .expect("first cancel_order should succeed");
    let after_first = svm.get_account(&pda).expect("order PDA must exist");

    // A fresh blockhash so the second, byte-identical cancellation isn't
    // rejected as a duplicate transaction before it reaches the program.
    svm.expire_blockhash();

    // Second cancellation is a no-op and leaves the order byte-for-byte the same.
    svm.send_transaction(signed_tx(&svm, &owner, &owner, cancel()))
        .expect("second cancel_order should succeed");
    let after_second = svm.get_account(&pda).expect("order PDA must still exist");

    assert_eq!(
        after_first, after_second,
        "cancelling an already-cancelled order must not change it"
    );
}

#[test]
fn rejects_cancellation_by_non_owner() {
    let (mut svm, program_id, attacker) = common::setup();
    let owner = unique_keypair();
    svm.airdrop(&owner.pubkey(), 1_000_000_000)
        .expect("airdrop to owner should succeed");

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    // The real owner creates the order.
    create_order_pda(&mut svm, &program_id, &owner, &intent);
    let before = svm.get_account(&pda).expect("order PDA must exist");

    // A different signer, presenting the same order, must not be able to cancel it.
    let cancel = CancelOrder {
        program_id,
        owner: attacker.pubkey(),
        created_by: attacker.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };
    let err = svm
        .send_transaction(signed_tx(&svm, &attacker, &attacker, cancel))
        .expect_err("cancellation by a non-owner must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, SettlementError::OwnerMismatch.into()),
    );

    let after = svm.get_account(&pda).expect("order PDA must still exist");
    assert_eq!(
        before, after,
        "a rejected cancellation must not change the order"
    );
}

#[test]
fn rejects_cancellation_when_owner_does_not_sign() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    create_order_pda(&mut svm, &program_id, &owner, &intent);
    let before = svm.get_account(&pda).expect("order PDA must exist");

    // The owner keeps its slot but its signature is stripped. Cancellation
    // authenticates the owner, so an unsigned request must be rejected.
    let mut cancel = Instruction::from(CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    });
    for meta in &mut cancel.accounts {
        if meta.pubkey == owner.pubkey() {
            assert!(
                meta.is_signer,
                "sanity check failed: owner is expected to be a signer"
            );
            meta.is_signer = false;
        }
    }

    // A separate fee payer submits, since the owner now signs nothing.
    let fee_payer = unique_keypair();
    svm.airdrop(&fee_payer.pubkey(), 1_000_000_000)
        .expect("airdrop to fee_payer should succeed");
    let err = svm
        .send_transaction(signed_tx(&svm, &fee_payer, &fee_payer, cancel))
        .expect_err("cancelling without the owner's signature must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, InstructionError::MissingRequiredSignature),
    );

    let after = svm.get_account(&pda).expect("order PDA must still exist");
    assert_eq!(
        before, after,
        "a rejected cancellation must not change the order"
    );
}

// This is a bit tricky: right now, there's no way to cancel off-chain (i.e.,
// signature-based) orders that haven't been created on-chain jet. On the other
// hand, there's no way to create off-chain orders at this point, so for now
// this problematic behavior is fine. We want to revisit this once we implement
// creating off-chain orders.
#[test]
fn rejects_nonexistent_off_chain_intent() {
    let (mut svm, program_id, owner) = common::setup();

    // An off-chain-authenticated intent has no on-chain owner-direct cancel
    // path; the create branch rejects it just as CreateOrder would.
    let intent = OrderIntent {
        flags: Flags {
            created_on_chain: false,
            ..Default::default()
        },
        ..sample_intent(owner.pubkey(), 0)
    };
    let (encoded, pda, _bump) = encode_and_derive(&intent, &program_id);

    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };
    let err = svm
        .send_transaction(signed_tx(&svm, &owner, &owner, cancel))
        .expect_err("cancelling a missing off-chain order must be rejected");
    assert_eq!(
        err.err,
        TransactionError::InstructionError(0, SettlementError::OrderCreatedOnChainMismatch.into()),
    );
    assert!(
        svm.get_account(&pda).is_none(),
        "a rejected cancellation must not leave a PDA behind"
    );
}

#[test]
fn rejects_arbitrary_wrong_pda() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = sample_intent(owner.pubkey(), 0);
    let (encoded, _canonical_pda, _bump) = encode_and_derive(&intent, &program_id);

    // A wrong, non-canonical order PDA for a missing order: the create branch
    // rejects it as CreateOrder does.
    let wrong_pda = unique_pubkey();
    let cancel = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: wrong_pda,
        intent_bytes: Some(encoded),
    };
    let tx = signed_tx(&svm, &owner, &owner, cancel);
    common::pda::assert_rejected_as_noncanonical(&mut svm, tx, &wrong_pda);
}
