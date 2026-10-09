use cow_settlement_client::cow_settlement_interface::{
    data::intent::{fixtures::sample_intent, EncodedOrderIntent, OrderIntent},
    instruction::{
        cancel_order::CancelOrder, create_order::CreateOrder, reclaim_order::ReclaimOrder,
    },
    pda::order::find_order_pda,
    SettlementError,
};
use cow_settlement_client::pda::order::DecodedOrderAccount;
use cow_settlement_interface::data::order::SIZE;
use litesvm::LiteSVM;
use solana_sdk::{
    clock::Clock,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    buffer,
    order::{buy_account, buy_mint, read_order, OrderBuilder},
    send,
    settlement::{build_staged_settlement, stage_order, StagedOrder},
    signed_tx, token, unique_keypair, unique_pubkey,
};

mod common;

const VALID_TO: u32 = 1_000;

fn reclaim_sample_intent(owner: Pubkey) -> OrderIntent {
    OrderIntent {
        owner,
        valid_to: VALID_TO,
        ..sample_intent(Default::default())
    }
}

fn encode_and_derive(
    intent: &OrderIntent,
    program_id: &Pubkey,
) -> ([u8; EncodedOrderIntent::SIZE], Pubkey) {
    let encoded = EncodedOrderIntent::from(intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, _) = find_order_pda(program_id, &encoded.hash());
    (bytes, pda)
}

/// Directly overwrite the body stored in an order PDA.
fn patch_order(
    svm: &mut LiteSVM,
    pda: &Pubkey,
    patch: impl FnOnce(DecodedOrderAccount) -> DecodedOrderAccount,
) {
    let mut account = svm.get_account(pda).expect("order PDA must exist");
    account.data = patch(read_order(svm, pda)).encode().to_vec();
    svm.set_account(*pda, account)
        .expect("set_account should succeed");
}

/// Create an order PDA owned by `owner` (who also pays rent), return the PDA.
fn create_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    owner: &Keypair,
    intent: &OrderIntent,
) -> Pubkey {
    let (encoded, pda) = encode_and_derive(intent, program_id);
    let ix = CreateOrder {
        program_id: *program_id,
        owner: owner.pubkey(),
        created_by: owner.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let tx = signed_tx(svm, owner, owner, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");
    pda
}

#[test]
fn happy_path_expired_returns_lamports_and_closes_pda() {
    let (mut svm, program_id, fee_payer) = common::setup();

    // `reclaim_recipient` is the `created_by` funder; it's separate from the fee
    // payer so its balance change reflects only the returned rent, not tx fees.
    let reclaim_recipient = unique_keypair();
    svm.airdrop(&reclaim_recipient.pubkey(), 1_000_000_000)
        .expect("airdrop should succeed");

    let intent = OrderIntent {
        owner: fee_payer.pubkey(),
        ..reclaim_sample_intent(fee_payer.pubkey())
    };
    let encoded = EncodedOrderIntent::from(&intent);
    let encoded_bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, _bump) = find_order_pda(&program_id, &encoded.hash());

    let pda_rent = svm.minimum_balance_for_rent_exemption(SIZE);

    // Create the order; `reclaim_recipient` funds the rent (`created_by`).
    let ix = CreateOrder {
        program_id,
        owner: fee_payer.pubkey(),
        created_by: reclaim_recipient.pubkey(),
        order_pda: pda,
        intent_bytes: encoded_bytes,
    };
    let tx = signed_tx(&svm, &fee_payer, &reclaim_recipient, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");

    // Since ReclaimOrder should return any funds in the order pda (even if beyond the rent limit), we airdrop some extra lamports
    let extra_lamports = 10;
    svm.airdrop(&pda, extra_lamports)
        .expect("airdrop should succeed");

    assert!(svm.get_account(&pda).is_some(), "order PDA must exist");

    let reclaim_recipient_before = common::lamports(&svm, &reclaim_recipient.pubkey());

    common::set_unix_timestamp(&mut svm, (VALID_TO + 1).into());

    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: reclaim_recipient.pubkey(),
        owner: None,
    }
    .instruction();
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    send_transaction_metered(&mut svm, tx, BenchLabel::ReclaimOrder)
        .expect("reclaim_order should succeed after expiry");

    // PDA is gone.
    assert!(
        svm.get_account(&pda).is_none(),
        "order PDA must be closed after reclaim"
    );

    // Reclaim recipient account received all lamports that were in the order pda; it paid no tx fees.
    let reclaim_recipient_after = common::lamports(&svm, &reclaim_recipient.pubkey());
    assert_eq!(
        reclaim_recipient_after - reclaim_recipient_before,
        pda_rent + extra_lamports,
        "reclaim recipient account must receive exactly the order PDA's rent lamports"
    );
}

/// Reclaim `pda` before its `valid_to`, crediting `owner`, and return the
/// transaction result.
fn perform_reclaim_while_unexpired(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    owner: &Keypair,
    pda: &Pubkey,
    include_owner_signature: bool,
) -> Result<(), solana_sdk::transaction::TransactionError> {
    // Taken from the order itself rather than from `VALID_TO`, so the clock the
    // transaction runs at can't drift from the order it's reclaiming.
    let valid_to = i64::from(read_order(svm, pda).intent.valid_to);
    common::set_unix_timestamp(svm, valid_to);

    let ix = ReclaimOrder {
        program_id: *program_id,
        order_pda: *pda,
        reclaim_recipient: owner.pubkey(),
        owner: include_owner_signature.then(|| owner.pubkey()),
    }
    .instruction();
    let tx = signed_tx(svm, owner, owner, ix);

    let result = send_transaction_metered(svm, tx, BenchLabel::ReclaimOrder);

    let executed_at = svm.get_sysvar::<Clock>().unix_timestamp;
    assert!(
        executed_at <= valid_to,
        "reclaim must run while the order is unexpired, ran at {executed_at} with valid_to {valid_to}"
    );

    result.map_err(|e| e.err)?;

    assert!(
        svm.get_account(pda).is_none(),
        "order PDA must be closed after reclaim"
    );

    Ok(())
}

#[test]
fn happy_path_order_fully_filled_is_reclaimable_before_expiry() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    // A sell order is full once its whole sell amount has been withdrawn.
    patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
        amount_withdrawn: order.intent.sell_amount.get(),
        ..order
    });

    perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda, false)
        .expect("a filled on-chain order should be reclaimable before it expires");
}

#[test]
fn happy_path_order_cancelled_is_reclaimable_by_owner_before_expiry() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
        cancelled: true,
        ..order
    });

    perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda, true)
        .expect("a cancelled on-chain order should be reclaimable before it expires");
}

#[test]
fn rejects_when_order_not_yet_expired() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);

    common::set_unix_timestamp(&mut svm, VALID_TO as i64); // technically this is the last valid timestamp

    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: owner.pubkey(),
        owner: None,
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::OrderNotReclaimable,
    );
}

#[test]
fn on_chain_order_partially_filled_is_not_reclaimable_before_expiry() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    // One token short of a full fill: the order can still be settled, so its
    // PDA has to stay.
    patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
        amount_withdrawn: order.intent.sell_amount.get() - 1,
        ..order
    });

    assert_instruction_error(
        perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda, true),
        SettlementError::OrderNotReclaimable,
    );
}

#[test]
fn recreating_a_reclaimed_order_creates_it_fresh() {
    let (mut svm, program_id, owner) = common::setup();

    let other_creator = unique_keypair();
    svm.airdrop(&other_creator.pubkey(), 1_000_000_000)
        .expect("airdrop to other_creator should succeed");

    let intent = reclaim_sample_intent(owner.pubkey());
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    // First creation records `owner` as `created_by`.
    create_order(&mut svm, &program_id, &owner, &intent);
    let before = svm.get_account(&pda).expect("order PDA should exist");

    // Closing the PDA clears all on-chain account data, so the same order can be
    // recreated afterwards.
    common::set_unix_timestamp(&mut svm, (VALID_TO + 1).into());
    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: owner.pubkey(),
        owner: None,
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    svm.send_transaction(tx)
        .expect("reclaim_order should succeed after expiry");
    assert!(
        svm.get_account(&pda).is_none(),
        "order PDA must be closed after reclaim"
    );

    // Recreate the same order (same uid, so same PDA) but with a different
    // `created_by`. Because the PDA was closed this is a genuine fresh creation,
    // not a no-op: the freshly written body records the new `created_by`, so the
    // account data differs from the original.
    let ix = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: other_creator.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let tx = signed_tx(&svm, &other_creator, &owner, ix);
    svm.send_transaction(tx)
        .expect("recreating a reclaimed order should succeed");
    let after = svm
        .get_account(&pda)
        .expect("order PDA must exist again after being recreated");
    assert_eq!(
        after.owner, program_id,
        "recreated order must be program-owned"
    );
    assert_ne!(
        before.data, after.data,
        "recreating a reclaimed order must write fresh data (the new created_by)"
    );
}

/// The sponsored model: `owner` authenticates an order with its signature while
/// a `sponsor` pays the fee and the rent. An adversary who records this
/// creation transaction might be able to recreate the order after the owner
/// decides to cancel it. It can't: replaying that original transaction is
/// rejected as already-processed, before it ever reaches the program.
#[test]
fn sponsored_order_cannot_be_recreated_by_replaying_the_original_transaction() {
    let (mut svm, program_id, sponsor) = common::setup();
    let owner = unique_keypair();
    let attacker = unique_keypair();
    svm.airdrop(&attacker.pubkey(), 1_000_000_000)
        .expect("airdrop to attacker should succeed");

    let intent = reclaim_sample_intent(owner.pubkey());
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    // Step 1: creation. `owner` signs, `sponsor` pays the fee and funds the
    // rent. The adversary records the fully-signed transaction verbatim.
    let create_ix = CreateOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: sponsor.pubkey(),
        order_pda: pda,
        intent_bytes: encoded,
    };
    let create_tx = Transaction::new_signed_with_payer(
        &[create_ix.into()],
        Some(&sponsor.pubkey()),
        &[&sponsor, &owner],
        svm.latest_blockhash(),
    );
    let replayed_tx = create_tx.clone();
    svm.send_transaction(create_tx)
        .expect("sponsored create_order should succeed");
    assert!(
        !read_order(&svm, &pda).cancelled,
        "the order must start active"
    );

    // Step 2: cancellation. `owner` regrets the order and cancels it.
    let cancel_ix = CancelOrder {
        program_id,
        owner: owner.pubkey(),
        created_by: sponsor.pubkey(),
        order_pda: pda,
        intent_bytes: Some(encoded),
    };
    let cancel_tx = Transaction::new_signed_with_payer(
        &[cancel_ix.into()],
        Some(&sponsor.pubkey()),
        &[&sponsor, &owner],
        svm.latest_blockhash(),
    );
    svm.send_transaction(cancel_tx)
        .expect("the owner should be able to cancel its order");
    assert!(
        read_order(&svm, &pda).cancelled,
        "the order must be cancelled"
    );

    // Step 3: the owner authorizes reclaim before expiry. The sponsor
    // receives the rent, independently of the owner's authorization.
    common::set_unix_timestamp(&mut svm, (VALID_TO - 1).into());
    let reclaim_ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: sponsor.pubkey(),
        owner: Some(owner.pubkey()),
    }
    .instruction();
    let reclaim_tx = signed_tx(&svm, &attacker, &owner, reclaim_ix);
    svm.send_transaction(reclaim_tx)
        .expect("owner-authorized reclaim should succeed");
    assert!(svm.get_account(&pda).is_none());

    // Step 4: attempted recreation. The adversary tries to recreate this order
    // from the original transaction, whose signature the runtime already
    // recorded. Replaying it verbatim is rejected before it reaches the
    // program.
    let err = svm
        .send_transaction(replayed_tx)
        .expect_err("replaying the original create transaction must be rejected");
    assert_eq!(
        err.err,
        TransactionError::AlreadyProcessed,
        "the replay must be rejected as an already-processed transaction"
    );
    assert!(
        svm.get_account(&pda).is_none(),
        "the reclaimed order must not reappear from a replayed transaction"
    );
}

#[test]
fn rejects_when_reclaim_recipient_mismatch() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);

    common::set_unix_timestamp(&mut svm, (VALID_TO + 1).into());

    let wrong_recipient = unique_pubkey();
    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: wrong_recipient,
        owner: None,
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::ReclaimRecipientMismatch,
    );
}

const SETTLED_SELL_AMOUNT: u64 = 1_000_000;
const SETTLED_BUY_AMOUNT: u64 = 2_000_000;

/// Mint a partially fillable order selling [`SETTLED_SELL_AMOUNT`] for
/// [`SETTLED_BUY_AMOUNT`], and stage a settlement selling `sell_amount` of it at
/// exactly the order's limit price (so any fraction of it settles). Passing
/// [`SETTLED_SELL_AMOUNT`] stages a full fill.
fn settleable_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
    sell_amount: u64,
) -> (StagedOrder, Pubkey) {
    let intent = OrderBuilder::new(svm, program_id, payer)
        .sell_amount(SETTLED_SELL_AMOUNT)
        .buy_amount(SETTLED_BUY_AMOUNT)
        .partially_fillable(true)
        .build();
    let (order_pda, _bump) = find_order_pda(program_id, &intent.uid());
    let staged = stage_order(
        svm,
        program_id,
        payer,
        &intent,
        &[sell_amount],
        sell_amount
            .checked_mul(SETTLED_BUY_AMOUNT)
            .expect("order math should work")
            .checked_div(SETTLED_SELL_AMOUNT)
            .expect("order math should work"),
    );
    (staged, order_pda)
}

/// A settlement that fills only part of an order leaves it fillable again, so
/// the order PDA has to stay until the order expires.
#[test]
fn rejects_reclaim_of_a_partially_filled_order() {
    let (mut svm, program_id, payer, solver) = common::setup_settle_ready();
    const PARTIAL_FILL: u64 = SETTLED_SELL_AMOUNT / 3;
    let (staged, order_pda) = settleable_order(&mut svm, &program_id, &payer, PARTIAL_FILL);

    let instructions =
        build_staged_settlement(&program_id, &solver.pubkey(), &[staged], vec![], &[]);
    send(&mut svm, &solver, &instructions).expect("a partial settlement should succeed");
    assert_eq!(
        read_order(&svm, &order_pda).amount_withdrawn,
        PARTIAL_FILL,
        "the settlement must have recorded a partial fill"
    );

    let ix = ReclaimOrder {
        program_id,
        order_pda,
        reclaim_recipient: payer.pubkey(),
        owner: None,
    }
    .instruction();
    let tx = signed_tx(&svm, &payer, &payer, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        SettlementError::OrderNotReclaimable,
    );
    assert!(
        svm.get_account(&order_pda).is_some(),
        "order PDA must survive a rejected reclaim"
    );
}

/// A reclaim placed between `BeginSettle` and `FinalizeSettle` closes the order
/// PDA without breaking the settlement around it.
///
/// `BeginSettle` does all of the settlement's order validation and records the
/// fill, and it's the only instruction of the pair that takes the order PDA as
/// an account. So reclaim is free to happen after that point.
#[test]
fn reclaim_mid_settlement_succeeds() {
    let (mut svm, program_id, payer, solver) = common::setup_settle_ready();
    let (staged, order_pda) = settleable_order(&mut svm, &program_id, &payer, SETTLED_SELL_AMOUNT);
    let pull_destination = staged.pulls[0].destination;
    let buy_token_account = buy_account(&staged.intent);
    let buffer_pda = buffer::buffer_pda(&program_id, &buy_mint(&staged.intent));
    let pda_rent = svm.minimum_balance_for_rent_exemption(SIZE);

    let reclaim = ReclaimOrder {
        program_id,
        order_pda,
        reclaim_recipient: payer.pubkey(),
        owner: None,
    }
    .instruction();
    let instructions =
        build_staged_settlement(&program_id, &solver.pubkey(), &[staged], vec![reclaim], &[]);

    // The `payer` that created the order signs nothing here and pays no fee (the
    // solver does), so its balance moves by the returned rent alone.
    let payer_before = common::lamports(&svm, &payer.pubkey());
    send(&mut svm, &solver, &instructions)
        .expect("reclaiming a just-filled order mid-settlement should succeed");

    assert!(
        svm.get_account(&order_pda).is_none(),
        "order PDA must be closed by the mid-settlement reclaim"
    );
    assert_eq!(
        common::lamports(&svm, &payer.pubkey()) - payer_before,
        pda_rent,
        "the order's creator must receive the closed PDA's rent"
    );

    // Both legs of the settlement went through around the reclaim: the pull in
    // `BeginSettle`, before the order PDA was closed, and the push in
    // `FinalizeSettle`, after.
    assert_eq!(token::balance(&svm, &pull_destination), SETTLED_SELL_AMOUNT);
    assert_eq!(token::balance(&svm, &buy_token_account), SETTLED_BUY_AMOUNT);
    assert_eq!(token::balance(&svm, &buffer_pda), 0);
}

#[test]
fn rejects_unexpired_cancelled_order_reclaim_by_non_owner() {
    let (mut svm, program_id, owner) = common::setup();
    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
        cancelled: true,
        ..order
    });
    let attacker = unique_keypair();
    svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    common::set_unix_timestamp(&mut svm, i64::from(VALID_TO));

    let reclaim = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: owner.pubkey(),
        owner: Some(attacker.pubkey()),
    }
    .instruction();
    let result = svm.send_transaction(signed_tx(&svm, &attacker, &attacker, reclaim));

    assert_instruction_error(result.map_err(|e| e.err), SettlementError::OwnerMismatch);
    assert!(svm.get_account(&pda).is_some());
}

#[test]
fn rejects_unexpired_cancelled_order_reclaim_without_owner_signature() {
    let (mut svm, program_id, owner) = common::setup();
    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
        cancelled: true,
        ..order
    });
    let attacker = unique_keypair();
    svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    common::set_unix_timestamp(&mut svm, i64::from(VALID_TO));

    let mut reclaim = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: owner.pubkey(),
        owner: Some(owner.pubkey()),
    }
    .instruction();

    for account in &mut reclaim.accounts {
        account.is_signer = false;
    }

    let result = svm.send_transaction(signed_tx(&svm, &attacker, &attacker, reclaim));

    assert_instruction_error(
        result.map_err(|e| e.err),
        solana_sdk::instruction::InstructionError::MissingRequiredSignature,
    );
    assert!(svm.get_account(&pda).is_some());
}

#[test]
fn rejects_unexpired_cancelled_order_reclaim_without_owner_account() {
    let (mut svm, program_id, owner) = common::setup();
    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
        cancelled: true,
        ..order
    });
    let attacker = unique_keypair();
    svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    common::set_unix_timestamp(&mut svm, i64::from(VALID_TO));

    let reclaim = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: owner.pubkey(),
        owner: None,
    }
    .instruction();
    let result = svm.send_transaction(signed_tx(&svm, &attacker, &attacker, reclaim));

    assert_instruction_error(
        result.map_err(|e| e.err),
        solana_sdk::instruction::InstructionError::MissingRequiredSignature,
    );
    assert!(svm.get_account(&pda).is_some());
}

#[test]
fn expired_cancelled_order_is_permissionlessly_reclaimable() {
    for fully_filled in [false, true] {
        let (mut svm, program_id, owner) = common::setup();
        let intent = reclaim_sample_intent(owner.pubkey());
        let pda = create_order(&mut svm, &program_id, &owner, &intent);
        let reclaimer = unique_keypair();
        patch_order(&mut svm, &pda, |order| DecodedOrderAccount {
            cancelled: true,
            amount_withdrawn: if fully_filled {
                order.intent.sell_amount.get()
            } else {
                0
            },
            ..order
        });
        svm.airdrop(&reclaimer.pubkey(), 1_000_000_000).unwrap();
        common::set_unix_timestamp(&mut svm, i64::from(VALID_TO + 1));
        let reclaim = ReclaimOrder {
            program_id,
            order_pda: pda,
            reclaim_recipient: owner.pubkey(),
            owner: None,
        }
        .instruction();
        svm.send_transaction(signed_tx(&svm, &reclaimer, &reclaimer, reclaim))
            .expect("anyone may reclaim an expired cancellation tombstone");
        assert!(svm.get_account(&pda).is_none());
    }
}

/// A user may want to cancel an order while another party holds a user's signed creation authorization unsubmitted.
/// If the order was able to be permissionlessly reclaimed while cancelled, the unsubmitted creation authorization
/// could still be played and be unexpectedly settled despite being cancelled.
///
/// Here we confirm that the order cannot be reclaimed while cancelled by this third party, and the withheld creation
/// authorization unable to be submitted.
#[test]
fn cancellation_blocks_withheld_sponsored_creation() {
    let (mut svm, program_id, sponsor) = common::setup();
    let owner = unique_keypair();
    let intent = reclaim_sample_intent(owner.pubkey());
    let (encoded, pda) = encode_and_derive(&intent, &program_id);
    // Fully signed but never submitted: AlreadyProcessed cannot protect it.
    let pending_creation = signed_tx(
        &svm,
        &sponsor,
        &owner,
        CreateOrder {
            program_id,
            owner: owner.pubkey(),
            created_by: sponsor.pubkey(),
            order_pda: pda,
            intent_bytes: encoded,
        },
    );
    svm.send_transaction(signed_tx(
        &svm,
        &sponsor,
        &owner,
        CancelOrder {
            program_id,
            owner: owner.pubkey(),
            created_by: sponsor.pubkey(),
            order_pda: pda,
            intent_bytes: Some(encoded),
        },
    ))
    .expect("cancellation must create a tombstone before creation lands");
    common::set_unix_timestamp(&mut svm, i64::from(VALID_TO - 1));
    let tombstone = svm.get_account(&pda).unwrap();
    let reclaim = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: sponsor.pubkey(),
        owner: None,
    }
    .instruction();

    // The 3rd party shouldn't be able to reclaim the cancelled order.
    assert_instruction_error(
        svm.send_transaction(signed_tx(&svm, &sponsor, &sponsor, reclaim))
            .map_err(|e| e.err),
        solana_sdk::instruction::InstructionError::MissingRequiredSignature,
    );
    assert_eq!(svm.get_account(&pda).unwrap(), tombstone);

    // The 3rd party shouldn't be able to submit the previously signed order creation with
    // still valid authorization while it remains cancelled.
    assert_instruction_error(
        svm.send_transaction(pending_creation).map_err(|e| e.err),
        solana_sdk::instruction::InstructionError::AccountAlreadyInitialized,
    );
    assert_eq!(svm.get_account(&pda).unwrap(), tombstone);
    assert!(read_order(&svm, &pda).cancelled);
}
