use litesvm::LiteSVM;
use settlement_client::settlement_interface::pda::order::ORDER_SEED;
use settlement_client::settlement_interface::{
    create_order::create_order,
    data::{
        intent::{EncodedOrderIntent, OrderIntent, OrderKind},
        order::{EncodedOrderAccount, OrderAccount},
    },
    pda::{order::find_order_pda, SETTLEMENT_SEED},
};
use solana_sdk::instruction::{Instruction, InstructionError};
use solana_sdk::{
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use solana_system_interface::error::SystemError;

mod common;

fn sample_intent() -> OrderIntent {
    OrderIntent {
        owner: Pubkey::new_from_array([0x11; 32]),
        buy_token_account: Pubkey::new_from_array([0x22; 32]),
        sell_token_account: Pubkey::new_from_array([0x33; 32]),
        sell_amount: 1_000_000,
        buy_amount: 2_000_000,
        valid_to: 0xdead_beef,
        kind: OrderKind::Sell,
        partially_fillable: true,
        app_data: [0x44; 32],
    }
}

fn encode_and_derive(
    intent: &OrderIntent,
    program_id: &Pubkey,
) -> ([u8; EncodedOrderIntent::SIZE], Pubkey) {
    let encoded = EncodedOrderIntent::from(intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let (pda, _bump) = find_order_pda(program_id, &encoded.hash());
    (bytes, pda)
}

/// Sign `ix` with `fee_payer` as the transaction fee payer and
/// `order_payer` as the keypair filling the `created_by` slot. Tests pass
/// two distinct keypairs to keep these roles independent.
fn signed_tx(
    svm: &LiteSVM,
    fee_payer: &Keypair,
    order_payer: &Keypair,
    ix: Instruction,
) -> Transaction {
    Transaction::new_signed_with_payer(
        &[ix],
        Some(&fee_payer.pubkey()),
        &[fee_payer, order_payer],
        svm.latest_blockhash(),
    )
}

#[test]
fn happy_path_creates_order_pda_with_expected_body() {
    let (mut svm, program_id, fee_payer) = common::setup();

    let intent = sample_intent();
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    // `fee_payer` pays for both rent and tx fees.
    let ix = create_order(&program_id, &fee_payer.pubkey(), &pda, &encoded);
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    svm.send_transaction(tx)
        .expect("create_order should succeed");

    let account = svm
        .get_account(&pda)
        .expect("order PDA should exist after create_order");
    assert_eq!(
        account.owner, program_id,
        "PDA must be owned by the program"
    );
    assert_eq!(
        account.data.len(),
        EncodedOrderAccount::SIZE,
        "PDA must be sized to the order body",
    );

    let expected_body: [u8; EncodedOrderAccount::SIZE] = EncodedOrderAccount::from(OrderAccount {
        cancelled: false,
        amount_withdrawn: 0,
        amount_received: 0,
        created_by: fee_payer.pubkey(),
        intent: intent.clone(),
    })
    .into();
    assert_eq!(
        account.data, expected_body,
        "PDA body must match expected layout"
    );

    // Rent-exempt sanity: the PDA must hold exactly the rent minimum for
    // its size.
    let rent = svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE);
    assert!(
        account.lamports == rent,
        "PDA must be rent-exempt: {} < {}",
        account.lamports,
        rent,
    );
}

#[test]
fn happy_path_creates_order_pda_with_separate_rent_and_fee_payers() {
    let (mut svm, program_id, fee_payer) = common::setup();
    let order_payer = Keypair::new_from_array([42; 32]);
    svm.airdrop(&order_payer.pubkey(), 1_000_000_000)
        .expect("airdrop to order payer should succeed");

    let intent = sample_intent();
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    fn lamports(account: &Keypair, svm: &LiteSVM) -> u64 {
        svm.get_account(&account.pubkey())
            .expect("account must exist")
            .lamports
    }

    let fee_payer_before = lamports(&fee_payer, &svm);
    let order_payer_before = lamports(&order_payer, &svm);

    let ix = create_order(&program_id, &order_payer.pubkey(), &pda, &encoded);
    let tx = signed_tx(&svm, &fee_payer, &order_payer, ix);
    let receipt = svm
        .send_transaction(tx)
        .expect("create_order should succeed");

    let fee_payer_after = lamports(&fee_payer, &svm);
    let order_payer_after = lamports(&order_payer, &svm);

    let rent = svm.minimum_balance_for_rent_exemption(EncodedOrderAccount::SIZE);

    // The rent for the new PDA was funded entirely by `order_payer`.
    assert_eq!(order_payer_before - order_payer_after, rent);
    // `fee_payer` was charged only the transaction fee, not the rent.
    assert_eq!(fee_payer_before - fee_payer_after, receipt.fee);
    // Sanity check: fee doesn't include rent.
    assert!(rent > receipt.fee);
}

#[test]
fn rejects_arbitrary_wrong_pda() {
    let (mut svm, program_id, fee_payer) = common::setup();

    let intent = sample_intent();
    let (encoded, _canonical_pda) = encode_and_derive(&intent, &program_id);

    let wrong_pda = Pubkey::new_unique();
    let ix = create_order(&program_id, &fee_payer.pubkey(), &wrong_pda, &encoded);
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);

    let err = svm
        .send_transaction(tx)
        .expect_err("wrong PDA must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation)
        ),
        "expected instruction 0 to fail, got {:?}",
        err.err,
    );
    assert!(
        svm.get_account(&wrong_pda).is_none(),
        "wrong PDA must not have been created"
    );
}

#[test]
fn rejects_non_canonical_bump_pda() {
    let (mut svm, program_id, fee_payer) = common::setup();

    let intent = sample_intent();
    let encoded = EncodedOrderIntent::from(&intent);
    let bytes: [u8; EncodedOrderIntent::SIZE] = (&encoded).into();
    let uid = encoded.hash();
    let (_canonical_pda, canonical_bump) = find_order_pda(&program_id, &uid);

    // Walk bumps below canonical until we find one that also yields an
    // off-curve point. That's a legitimate PDA for the program's seeds, but
    // not the canonical one.
    let (non_canonical_bump, non_canonical_pda) = (0..canonical_bump)
        .rev()
        .find_map(|bump| {
            Pubkey::create_program_address(
                &[SETTLEMENT_SEED, &uid, ORDER_SEED, &[bump]],
                &program_id,
            )
            .ok()
            .map(|addr| (bump, addr))
        })
        .expect("sample intent must have a non-canonical off-curve bump");

    assert_ne!(non_canonical_bump, canonical_bump);

    let ix = create_order(&program_id, &fee_payer.pubkey(), &non_canonical_pda, &bytes);
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    let err = svm
        .send_transaction(tx)
        .expect_err("non-canonical-bump PDA must be rejected");
    assert!(
        matches!(
            err.err,
            TransactionError::InstructionError(0, InstructionError::PrivilegeEscalation)
        ),
        "expected instruction 0 to fail, got {:?}",
        err.err,
    );
    assert!(
        svm.get_account(&non_canonical_pda).is_none(),
        "non-canonical-bump PDA must not have been created"
    );
}

#[test]
fn rejects_creating_same_pda_twice() {
    let (mut svm, program_id, fee_payer) = common::setup();

    let intent = sample_intent();
    let (encoded, pda) = encode_and_derive(&intent, &program_id);

    // First creation populates the PDA.
    let ix = create_order(&program_id, &fee_payer.pubkey(), &pda, &encoded);
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    svm.send_transaction(tx)
        .expect("first create_order should succeed");

    // The two transactions are otherwise identical; expire the blockhash so
    // the second isn't rejected as a duplicate signature instead of a
    // duplicate PDA.
    svm.expire_blockhash();

    let ix = create_order(&program_id, &fee_payer.pubkey(), &pda, &encoded);
    let tx = signed_tx(&svm, &fee_payer, &fee_payer, ix);
    // Keep the compiled message's `account_keys` around so we can resolve
    // the `program_id_index` of the failing inner instruction below.
    let account_keys = tx.message.account_keys.clone();
    let err = svm
        .send_transaction(tx)
        .expect_err("recreating an existing PDA must be rejected");

    let expected = TransactionError::InstructionError(
        0,
        InstructionError::Custom(SystemError::AccountAlreadyInUse as u32),
    );
    assert_eq!(err.err, expected);

    // The check above is a bit misleading: the u32 that's returned there
    // is `0`, so this could be by chance a custom error from the current
    // program (`Custom(0)`). Here we do an extra sanity check that the
    // system program has indeed been called.
    let instruction_index = 0;
    let last_cpi = err.meta.inner_instructions[instruction_index]
        .last()
        .expect("system-program CPI should be available");
    let failing_program = account_keys[last_cpi.instruction.program_id_index as usize];
    assert_eq!(
        failing_program,
        settlement_interface::create_order::SYSTEM_PROGRAM_ID
    );
}
