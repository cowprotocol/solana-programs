use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::cow_settlement_interface::{
    data::order::OrderAccount, instruction::reclaim_order::ReclaimOrder, Pubkey, SettlementError,
};
use solana_sdk::{
    account::Account,
    instruction::InstructionError,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::cmd::settle::parse_order_input;
use crate::cmd::Context;
use crate::utils::output::{print_failures, print_summary};

/// Most orders that always fit into one transaction, reached when every order
/// has its own creator, none of them the payer.
const MAX_ORDERS: usize = 15;

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The orders to close, each a base58 order PDA or a 64-char hex UID, at
    /// most 15 so they fit into one transaction. Their rent goes back to each
    /// order's `created_by` account, whoever pays for the transaction.
    #[arg(required = true, num_args = 1..=MAX_ORDERS)]
    orders: Vec<String>,
}

/// Reclaims the orders in a single transaction. If any order is bad, nothing
/// is reclaimed and the offending orders are summarized: every order that
/// can't be read as one is caught before sending, but once the transaction
/// fails on-chain only the first order it failed on is known.
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
                let reason = match settlement_error(&err) {
                    Some(err) => format!("{err:?}"),
                    None => err.to_string(),
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

/// The settlement error a failed instruction carries, if any.
fn settlement_error(err: &InstructionError) -> Option<SettlementError> {
    match err {
        InstructionError::Custom(code) => SettlementError::try_from(*code).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use cow_settlement_client::cow_settlement_interface::data::order::{DISCRIMINATOR, SIZE};
    use solana_instruction::Instruction;

    use super::*;

    const PROGRAM_ID: Pubkey = Pubkey::new_from_array([1; 32]);
    const CREATED_BY: Pubkey = Pubkey::new_from_array([2; 32]);

    /// Largest serialized transaction the network accepts
    /// (`solana_packet::PACKET_DATA_SIZE`).
    const MAX_TRANSACTION_SIZE: u64 = 1232;

    /// An order account owned by `owner`: only the discriminator and
    /// `created_by` (bytes 19..51 of the layout) need to be meaningful.
    fn order_account(owner: Pubkey) -> Account {
        let mut data = vec![0; SIZE];
        data[0] = DISCRIMINATOR;
        data[19..51].copy_from_slice(CREATED_BY.as_ref());
        Account {
            lamports: 1,
            data,
            owner,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn reclaim_recipient_is_created_by() {
        let account = order_account(PROGRAM_ID);
        assert_eq!(
            reclaim_recipient(&account, &PROGRAM_ID).unwrap(),
            CREATED_BY
        );
    }

    #[test]
    fn reclaim_recipient_rejects_foreign_owner() {
        let account = order_account(Pubkey::new_from_array([3; 32]));
        assert!(reclaim_recipient(&account, &PROGRAM_ID).is_err());
    }

    #[test]
    fn reclaim_recipient_rejects_non_order_data() {
        let mut account = order_account(PROGRAM_ID);
        account.data[0] = DISCRIMINATOR.wrapping_add(1);
        assert!(reclaim_recipient(&account, &PROGRAM_ID).is_err());
        account.data.truncate(SIZE - 1);
        assert!(reclaim_recipient(&account, &PROGRAM_ID).is_err());
    }

    #[test]
    fn settlement_error_decodes_custom_codes() {
        assert_eq!(
            settlement_error(&InstructionError::Custom(
                SettlementError::OrderNotReclaimable.into()
            )),
            Some(SettlementError::OrderNotReclaimable)
        );
        assert_eq!(settlement_error(&InstructionError::Custom(u32::MAX)), None);
        assert_eq!(
            settlement_error(&InstructionError::InvalidAccountData),
            None
        );
    }

    fn reclaim_ix(reclaim_recipient: Pubkey) -> Instruction {
        ReclaimOrder {
            program_id: PROGRAM_ID,
            order_pda: Pubkey::new_unique(),
            reclaim_recipient,
        }
        .instruction()
    }

    fn fits(ixs: &[Instruction]) -> bool {
        let tx = Transaction::new_with_payer(ixs, Some(&Pubkey::new_unique()));
        bincode::serialized_size(&tx).unwrap() <= MAX_TRANSACTION_SIZE
    }

    #[test]
    fn max_orders_is_the_most_that_always_fit() {
        let ixs: Vec<_> = (0..=MAX_ORDERS)
            .map(|_| reclaim_ix(Pubkey::new_unique()))
            .collect();
        assert!(fits(&ixs[..MAX_ORDERS]));
        assert!(!fits(&ixs));
    }
}
