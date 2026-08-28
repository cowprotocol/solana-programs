use cow_settlement_client::cow_settlement_interface::{
    data::intent::{fixtures::sample_intent, EncodedOrderIntent, OrderIntent},
    instruction::{create_order::CreateOrder, reclaim_order::ReclaimOrder},
    pda::order::find_order_pda,
    SettlementError,
};
use cow_settlement_interface::data::{
    intent::Flags,
    order::{EncodedOrderAccount, OrderAccount},
};
use litesvm::LiteSVM;
use solana_sdk::{
    clock::Clock,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
};

use crate::common::{
    assert_instruction_error,
    benchmark::{send_transaction_metered, BenchLabel},
    buffer, create_account_at,
    order::OrderBuilder,
    send,
    settlement::{build_staged_settlement, stage_order, StagedOrder},
    signed_tx, to_instruction_error, token, unique_keypair, unique_pubkey,
};

mod common;

const VALID_TO: u32 = 1_000;

fn reclaim_sample_intent(owner: Pubkey) -> OrderIntent {
    OrderIntent {
        owner,
        valid_to: VALID_TO,
        ..sample_intent(Flags {
            created_on_chain: true,
            ..Default::default()
        })
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

/// Decode the order stored in an order PDA.
fn read_order(svm: &LiteSVM, pda: &Pubkey) -> OrderAccount {
    let account = svm.get_account(pda).expect("order PDA must exist");
    OrderAccount::try_from(&account.data[..]).expect("order PDA must decode")
}

/// Directly overwrite the body stored in an order PDA.
fn patch_order(svm: &mut LiteSVM, pda: &Pubkey, patch: impl FnOnce(OrderAccount) -> OrderAccount) {
    let mut account = svm.get_account(pda).expect("order PDA must exist");
    account.data = EncodedOrderAccount::from(patch(read_order(svm, pda))).to_vec();
    svm.set_account(*pda, account)
        .expect("set_account should succeed");
}

/// Put an order PDA on-chain directly, bypassing `CreateOrder`, which only
/// accepts intents declaring on-chain authentication. This is how an order
/// authenticated by an off-chain signature is staged.
fn hack_write_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    intent: &OrderIntent,
    created_by: &Pubkey,
    patch: impl FnOnce(OrderAccount) -> OrderAccount,
) -> Pubkey {
    let (pda, bump) = find_order_pda(program_id, &intent.uid());
    let order = patch(OrderAccount {
        bump,
        created_by: *created_by,
        intent: intent.clone(),
        ..Default::default()
    });
    create_account_at(svm, pda, program_id, &EncodedOrderAccount::from(order)[..]);
    pda
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

    let pda_rent = svm.minimum_balance_for_rent_exemption(
        cow_settlement_client::cow_settlement_interface::data::order::EncodedOrderAccount::SIZE,
    );

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
) -> Result<(), solana_sdk::transaction::TransactionError> {
    // Taken from the order itself rather than from `VALID_TO`, so the clock the
    // transaction runs at can't drift from the order it's reclaiming.
    let valid_to = i64::from(read_order(svm, pda).intent.valid_to);
    common::set_unix_timestamp(svm, valid_to);

    let ix = ReclaimOrder {
        program_id: *program_id,
        order_pda: *pda,
        reclaim_recipient: owner.pubkey(),
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
fn happy_path_on_chain_order_fully_filled_is_reclaimable_before_expiry() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    // A sell order is full once its whole sell amount has been withdrawn.
    patch_order(&mut svm, &pda, |order| OrderAccount {
        amount_withdrawn: order.intent.sell_amount,
        ..order
    });

    perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda)
        .expect("a filled on-chain order should be reclaimable before it expires");
}

#[test]
fn happy_path_on_chain_order_cancelled_is_reclaimable_before_expiry() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    patch_order(&mut svm, &pda, |order| OrderAccount {
        cancelled: true,
        ..order
    });

    perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda)
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
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        to_instruction_error(SettlementError::OrderNotReclaimable),
    );
}

#[test]
fn on_chain_order_partially_filled_is_not_reclaimable_before_expiry() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = reclaim_sample_intent(owner.pubkey());
    let pda = create_order(&mut svm, &program_id, &owner, &intent);
    // One token short of a full fill: the order can still be settled, so its
    // PDA has to stay.
    patch_order(&mut svm, &pda, |order| OrderAccount {
        amount_withdrawn: order.intent.sell_amount - 1,
        ..order
    });

    assert_instruction_error(
        perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda),
        to_instruction_error(SettlementError::OrderNotReclaimable),
    );
}

/// An order authenticated by an off-chain signature can be recreated by anyone
/// holding that signature, which would reset its fills and its cancellation.
/// Being unfillable doesn't make it reclaimable, then: only expiry does, and
/// expiry does so regardless of how the order was authenticated.
#[test]
fn off_chain_order_is_reclaimable_only_once_expired() {
    let (mut svm, program_id, owner) = common::setup();

    let intent = OrderIntent {
        flags: Flags {
            created_on_chain: false,
            ..Default::default()
        },
        ..reclaim_sample_intent(owner.pubkey())
    };
    // Cancelled *and* completely filled: the strongest case for early reclaim,
    // and it still has to wait.
    let pda = hack_write_order(&mut svm, &program_id, &intent, &owner.pubkey(), |order| {
        OrderAccount {
            cancelled: true,
            amount_withdrawn: order.intent.sell_amount,
            ..order
        }
    });

    assert_instruction_error(
        perform_reclaim_while_unexpired(&mut svm, &program_id, &owner, &pda),
        to_instruction_error(SettlementError::OrderNotReclaimable),
    );
    assert!(
        svm.get_account(&pda).is_some(),
        "order PDA must survive a rejected reclaim"
    );

    common::set_unix_timestamp(&mut svm, (VALID_TO + 1).into());
    svm.expire_blockhash();
    let ix = ReclaimOrder {
        program_id,
        order_pda: pda,
        reclaim_recipient: owner.pubkey(),
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    svm.send_transaction(tx)
        .expect("an expired order should be reclaimable however it was authenticated");
    assert!(
        svm.get_account(&pda).is_none(),
        "order PDA must be closed after reclaim"
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
    svm.expire_blockhash();
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
    }
    .instruction();
    let tx = signed_tx(&svm, &owner, &owner, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        to_instruction_error(SettlementError::ReclaimRecipientMismatch),
    );
}

const SETTLED_SELL_AMOUNT: u64 = 1_000_000;
const SETTLED_BUY_AMOUNT: u64 = 2_000_000;

fn settleable_order(
    svm: &mut LiteSVM,
    program_id: &Pubkey,
    payer: &Keypair,
) -> (StagedOrder, Pubkey) {
    let intent = OrderBuilder::new(svm, program_id, payer)
        .sell_amount(SETTLED_SELL_AMOUNT)
        .buy_amount(SETTLED_BUY_AMOUNT)
        .partially_fillable(false)
        .build();
    let (order_pda, _bump) = find_order_pda(program_id, &intent.uid());
    let staged = stage_order(
        svm,
        program_id,
        payer,
        &intent,
        &[SETTLED_SELL_AMOUNT],
        SETTLED_BUY_AMOUNT,
    );
    (staged, order_pda)
}

#[test]
fn rejects_reclaim_of_a_partially_filled_order() {
    let (mut svm, program_id, payer, _solver) = common::setup_settle_ready();
    let (_staged, order_pda) = settleable_order(&mut svm, &program_id, &payer);

    let ix = ReclaimOrder {
        program_id,
        order_pda,
        reclaim_recipient: payer.pubkey(),
    }
    .instruction();
    let tx = signed_tx(&svm, &payer, &payer, ix);
    assert_instruction_error(
        svm.send_transaction(tx).map_err(|e| e.err),
        to_instruction_error(SettlementError::OrderNotReclaimable),
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
    let (staged, order_pda) = settleable_order(&mut svm, &program_id, &payer);
    let pull_destination = staged.pulls[0].destination;
    let buy_token_account = staged.intent.buy_token_account;
    let buffer_pda = buffer::buffer_pda(&program_id, &staged.intent.buy_mint);
    let pda_rent = svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE);

    let reclaim = ReclaimOrder {
        program_id,
        order_pda,
        reclaim_recipient: payer.pubkey(),
    }
    .instruction();
    let instructions =
        build_staged_settlement(&program_id, &solver.pubkey(), &[staged], vec![reclaim]);

    // The `payer` that created the order signs nothing here and pays no fee (the
    // solver does), so its balance moves by the returned rent alone.
    let payer_before = common::lamports(&svm, &payer.pubkey());
    send(&mut svm, &solver, instructions)
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
