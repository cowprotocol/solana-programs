use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::cow_settlement_interface::{
    data::order::OrderAccount, instruction::reclaim_order::ReclaimOrder, parse_settlement_error,
    Pubkey,
};
use solana_sdk::{
    account::Account,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::cmd::Context;
use crate::utils::{
    output::{print_failures, print_summary},
    parsing::OrderArg,
};

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The orders to close, each a base58 order PDA or a 64-char hex UID.
    #[arg(required = true, value_parser = OrderArg::parse)]
    orders: Vec<OrderArg>,
}

/// Reclaims the orders in a single transaction. Errors if the order's reclaim
/// recipients couldn't be read, or the account is not owned by the settlement
/// program.
pub fn run(ctx: Context, args: ReclaimArgs) -> anyhow::Result<()> {
    let orders = &args.orders;
    let pdas: Vec<Pubkey> = orders
        .iter()
        .map(|order| order.pda(&ctx.program_id))
        .collect();
    let accounts = ctx
        .rpc
        .get_multiple_accounts(&pdas)
        .context("failed to fetch order accounts")?;
    let mut ixs = Vec::new();
    for ((order, &order_pda), account) in orders.iter().zip(&pdas).zip(accounts) {
        let reclaim_recipient = account
            .context("order account not found")
            .and_then(|account| {
                reclaim_recipient(&account, &ctx.program_id)
                    .with_context(|| format!("not an order of {}", ctx.program_id))
            })
            .with_context(|| format!("bad order {order}, nothing was reclaimed"))?;
        ixs.push(
            ReclaimOrder {
                program_id: ctx.program_id,
                order_pda,
                reclaim_recipient,
                owner: Some(ctx.payer.pubkey()),
            }
            .instruction(),
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
        // If the transaction failed, try to introspect and neatly print out the failing error
        if let Some(TransactionError::InstructionError(index, err)) = e.get_transaction_error() {
            if let Some(order) = orders.get(usize::from(index)) {
                let reason = match parse_settlement_error(err) {
                    Ok(err) => format!("{err:?}"),
                    Err(err) => err.to_string(),
                };
                print_failures(&[(order.to_string(), reason)]);
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
