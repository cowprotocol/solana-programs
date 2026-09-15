//! Integration tests for settling a `CreateWithdrawalOrder`. The happy path
//! draws buffered fees out through the ordinary `[BeginSettle, FinalizeSettle]`
//! pair with no settlement-side code of its own. The confinement test shows the
//! flip side: because the program forces the order's owner to the state PDA, a
//! withdrawal order can only ever sell an account the state PDA owns, so it can
//! never reach user funds.

use crate::common::{
    assert_instruction_error, buffer,
    order::read_order,
    register_solver, send, send_with_signers,
    settlement::{build_staged_settlement, StagedOrder},
    setup_init, token, unique_keypair,
    withdrawal::{prepare_fee_withdrawal_accounts, sample_fee_order},
};
use cow_settlement_client::instruction::{CreateWithdrawalOrder, Pull};
use cow_settlement_interface::{
    data::intent::OrderIntent, pda::order::find_order_pda, SettlementError,
};
use solana_sdk::signer::Signer;

mod common;

#[test]
fn settling_a_withdrawal_order_withdraws_the_buffered_fees() {
    let (mut svm, params) = setup_init();

    // A registered, funded solver settles the order.
    let solver = unique_keypair();
    register_solver(&mut svm, &params, &solver.pubkey());
    svm.airdrop(&solver.pubkey(), 1_000_000_000)
        .expect("airdrop to solver should succeed");

    // The fee buffer holds 1_000_000 of the fee mint; the order sells all of it
    // for 500_000 of the buy mint, delivered to the treasury.
    const FEES: u64 = 1_000_000;
    const PROCEEDS: u64 = 500_000;
    let fee_withdrawal_accounts = prepare_fee_withdrawal_accounts(&mut svm, &params, FEES);
    let intent = sample_fee_order(params.state_pda, &fee_withdrawal_accounts, FEES, PROCEEDS);
    let ix = CreateWithdrawalOrder {
        program_id: params.program_id,
        authority: params.withdrawal.pubkey(),
        payer: params.payer.pubkey(),
        intent: &intent,
    };
    send_with_signers(&mut svm, &params.payer, &[&params.withdrawal], &[ix.into()])
        .expect("placing a withdrawal order should succeed");

    let fee_recipient = token::create_token_account(
        &mut svm,
        &params.payer,
        &fee_withdrawal_accounts.fee_mint,
        &solver.pubkey(),
    );
    buffer::ensure_funded(
        &mut svm,
        &params.program_id,
        &params.payer,
        &fee_withdrawal_accounts.buy_mint,
        PROCEEDS,
    );
    let staged = StagedOrder {
        intent: intent.clone(),
        pulls: vec![Pull {
            destination: fee_recipient,
            amount: FEES,
        }],
        amount_out: PROCEEDS,
    };
    let instructions =
        build_staged_settlement(&params.program_id, &solver.pubkey(), &[staged], vec![]);
    send(&mut svm, &solver, &instructions).expect("settling the withdrawal order should succeed");

    // The fees left the buffer for the solver, and the proceeds reached the
    // treasury out of the buy buffer.
    assert_eq!(
        token::balance(&svm, &fee_withdrawal_accounts.fee_buffer),
        0,
        "the fee buffer is drained"
    );
    assert_eq!(
        token::balance(&svm, &fee_recipient),
        FEES,
        "the solver received the fees"
    );
    assert_eq!(
        token::balance(&svm, &fee_withdrawal_accounts.treasury),
        PROCEEDS,
        "the treasury received the proceeds"
    );
    assert_eq!(
        token::balance(
            &svm,
            &buffer::buffer_pda(&params.program_id, &fee_withdrawal_accounts.buy_mint)
        ),
        0,
        "the buy buffer paid out the proceeds"
    );

    // The order records the fill.
    let (order_pda, _) = find_order_pda(&params.program_id, &intent.uid());
    let decoded = read_order(&svm, &order_pda);
    assert_eq!(decoded.amount_withdrawn, FEES);
    assert_eq!(decoded.amount_received, PROCEEDS);
}

/// Forcing `intent.owner` to be the state PDA means a withdrawal order can only
/// sell an account the state PDA owns.
/// However, we can still point the order at a user token account instead:
/// creating it still succeeds (the sell account isn't checked), but settling
/// reverts, so the withdrawal authority can never reach funds that aren't
/// controlled by the state PDA.
#[test]
fn a_withdrawal_order_cannot_sell_an_account_the_state_pda_doesnt_own() {
    let (mut svm, params) = setup_init();

    let solver = unique_keypair();
    register_solver(&mut svm, &params, &solver.pubkey());
    svm.airdrop(&solver.pubkey(), 1_000_000_000)
        .expect("airdrop to solver should succeed");

    const FUNDS: u64 = 1_000_000;
    const PROCEEDS: u64 = 500_000;
    let fee_withdrawal_accounts = prepare_fee_withdrawal_accounts(&mut svm, &params, FUNDS);

    let victim = unique_keypair();
    let victim_account = token::create_token_account(
        &mut svm,
        &params.payer,
        &fee_withdrawal_accounts.fee_mint,
        &victim.pubkey(),
    );
    token::mint_to(
        &mut svm,
        &params.payer,
        &fee_withdrawal_accounts.fee_mint,
        &victim_account,
        FUNDS,
    );

    // A withdrawal order owned by the state PDA (as the program forces), but
    // selling out of the victim's account rather than a buffer.
    let intent = OrderIntent {
        sell_token_account: victim_account,
        ..sample_fee_order(params.state_pda, &fee_withdrawal_accounts, FUNDS, PROCEEDS)
    };
    let ix = CreateWithdrawalOrder {
        program_id: params.program_id,
        authority: params.withdrawal.pubkey(),
        payer: params.payer.pubkey(),
        intent: &intent,
    };
    send_with_signers(&mut svm, &params.payer, &[&params.withdrawal], &[ix.into()])
        .expect("creation succeeds: the sell account isn't validated until settlement");

    // Stage a settlement exactly as a solver would, with a well-formed buy side
    // so the sell-side ownership check is what rejects it.
    let recipient = token::create_token_account(
        &mut svm,
        &params.payer,
        &fee_withdrawal_accounts.fee_mint,
        &solver.pubkey(),
    );
    buffer::ensure_funded(
        &mut svm,
        &params.program_id,
        &params.payer,
        &fee_withdrawal_accounts.buy_mint,
        PROCEEDS,
    );
    let staged = StagedOrder {
        intent: intent.clone(),
        pulls: vec![Pull {
            destination: recipient,
            amount: FUNDS,
        }],
        amount_out: PROCEEDS,
    };
    let instructions =
        build_staged_settlement(&params.program_id, &solver.pubkey(), &[staged], vec![]);

    let result = send(&mut svm, &solver, &instructions);
    assert_instruction_error(result, SettlementError::SellTokenOwnerMismatch);

    assert_eq!(
        token::balance(&svm, &victim_account),
        FUNDS,
        "the victim's account is left untouched"
    );
}
