use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::cow_settlement_interface::{
    data::order::OrderAccount, instruction::reclaim_order::ReclaimOrder, Pubkey, SettlementError,
};
use solana_sdk::{
    account::Account,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::cmd::Context;
use crate::utils::{
    output::{print_failures, print_summary},
    parsing::parse_order_input,
};

/// Most orders that always fit into one transaction, reached when every order
/// has its own creator, none of them the payer.
/// If the settlement program or the solana protocol changes, this constant may need
/// to be updated.
const MAX_ORDERS: usize = 15;

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The orders to close, each a base58 order PDA or a 64-char hex UID.
    #[arg(required = true, num_args = 1..=MAX_ORDERS)]
    orders: Vec<String>,
}

/// Reclaims the orders in a single transaction. Errors if any of the
/// input is not a order pda or uid, the order's reclaim recipients
/// couldn't be read, or the account is not owned by the settlement program.
pub fn run(ctx: Context, args: ReclaimArgs) -> anyhow::Result<()> {
    let mut failures: Vec<(&str, String)> = Vec::new();

    let mut orders = Vec::new();
    for input in &args.orders {
        match parse_order_input(&ctx.program_id, input) {
            Ok(pda) => orders.push((input.as_str(), pda)),
            Err(e) => failures.push((input, format!("{e:#}"))),
        }
    }

    let pdas: Vec<Pubkey> = orders.iter().map(|(_, pda)| *pda).collect();
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&pdas)
        .context("failed to fetch order accounts")?;
    let mut ixs = Vec::new();
    for (&(input, order_pda), account) in orders.iter().zip(accounts) {
        let recipient = account
            .context("order account not found")
            .and_then(|account| {
                reclaim_recipient(&account, &ctx.program_id)
                    .with_context(|| format!("not an order of {}", ctx.program_id))
            });
        match recipient {
            Ok(reclaim_recipient) => ixs.push(
                ReclaimOrder {
                    program_id: ctx.program_id,
                    order_pda,
                    reclaim_recipient,
                    owner: Some(ctx.payer.pubkey()),
                }
                .instruction(),
            ),
            Err(e) => failures.push((input, format!("{e:#}"))),
        }
    }
    if !failures.is_empty() {
        print_failures(&failures);
        anyhow::bail!(
            "{} of {} orders are bad, nothing was reclaimed",
            failures.len(),
            args.orders.len()
        );
    }

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let sig = ctx.rpc.send_and_confirm_transaction(&tx).map_err(|e| {
        // Every input made it into `orders`, so instruction `index` reclaims
        // order input `index`.
        if let Some(TransactionError::InstructionError(index, err)) = e.get_transaction_error() {
            if let Some(&(input, _)) = orders.get(usize::from(index)) {
                let reason = match SettlementError::try_from(err) {
                    Ok(err) => format!("{err:?}"),
                    Err(err) => err.to_string(),
                };
                print_failures(&[(input, reason)]);
            }
        }
        anyhow::Error::new(e).context("transaction failed, nothing was reclaimed")
    })?;

    let mut summary: Vec<(&str, &dyn ToString)> = vec![("signature", &sig)];
    summary.extend(pdas.iter().map(|pda| ("orderPda", pda as &dyn ToString)));
    print_summary(&summary);

    Ok(())
}

/// The `created_by` account recorded in an order, which the program requires
/// as the reclaim recipient. Fails if `account` isn't an order owned by
/// `program_id`.
fn reclaim_recipient(account: &Account, program_id: &Pubkey) -> anyhow::Result<Pubkey> {
    anyhow::ensure!(
        account.owner == *program_id,
        "account is owned by {}",
        account.owner
    );
    let order = OrderAccount::attach(&account.data[..])
        .map_err(|e| anyhow::anyhow!("account data is not an order: {e}"))?;
    Ok(order.created_by())
}
