use anyhow::Context as _;
use clap::Args as ClapArgs;
use cow_settlement_client::cow_settlement_interface::{
    data::order::OrderAccount, instruction::reclaim_order::ReclaimOrder, Pubkey, SettlementError,
};
use solana_instruction::Instruction;
use solana_sdk::{
    account::Account,
    instruction::InstructionError,
    signature::Signer,
    transaction::{Transaction, TransactionError},
};

use crate::cmd::settle::parse_order_input;
use crate::cmd::Context;
use crate::utils::transaction::fits;

/// Most addresses a single `getMultipleAccounts` call accepts.
const MAX_ACCOUNTS_PER_FETCH: usize = 100;

#[derive(ClapArgs)]
pub struct ReclaimArgs {
    /// The orders to close, each a base58 order PDA or a 64-char hex UID. Their
    /// rent goes back to each order's `created_by` account, whoever pays for
    /// the transactions.
    #[arg(required = true)]
    orders: Vec<String>,
}

/// Reclaims the orders packed into as few transactions as fit. When a
/// transaction fails on one of its orders, that order is dropped and the rest
/// are resent. Orders the program rejects as
/// [`SettlementError::OrderNotReclaimable`] are reported but not counted as
/// failures; any other failure makes the command fail once all orders are
/// processed.
pub fn run(ctx: Context, args: ReclaimArgs) -> anyhow::Result<()> {
    let payer = ctx.payer.pubkey();
    let mut failures = Vec::new();
    let mut fail = |input: &str, e: anyhow::Error| {
        println!("failed          {input}: {e:#}");
        failures.push(input.to_owned());
    };

    let mut orders = Vec::new();
    for input in &args.orders {
        match parse_order_input(&ctx.program_id, input) {
            Ok(pda) => orders.push((input.as_str(), pda)),
            Err(e) => fail(input, e),
        }
    }

    let mut pending: Vec<(&str, Instruction)> = Vec::new();
    for chunk in orders.chunks(MAX_ACCOUNTS_PER_FETCH) {
        let pdas: Vec<Pubkey> = chunk.iter().map(|(_, pda)| *pda).collect();
        let accounts = ctx
            .rpc
            .get_multiple_accounts(&pdas)
            .context("failed to fetch order accounts")?;
        for (&(input, order_pda), account) in chunk.iter().zip(accounts) {
            let recipient = account
                .context("order account not found")
                .and_then(|account| {
                    reclaim_recipient(&account, &ctx.program_id)
                        .with_context(|| format!("not an order of {}", ctx.program_id))
                });
            match recipient {
                Ok(reclaim_recipient) => pending.push((
                    input,
                    ReclaimOrder {
                        program_id: ctx.program_id,
                        order_pda,
                        reclaim_recipient,
                    }
                    .instruction(),
                )),
                Err(e) => fail(input, e),
            }
        }
    }

    while !pending.is_empty() {
        let ixs: Vec<Instruction> = pending.iter().map(|(_, ix)| ix.clone()).collect();
        let len = batch_len(&ixs, &payer);
        let blockhash = ctx
            .rpc
            .get_latest_blockhash()
            .context("failed to fetch blockhash")?;
        let tx =
            Transaction::new_signed_with_payer(&ixs[..len], Some(&payer), &[&ctx.payer], blockhash);
        match ctx.rpc.send_and_confirm_transaction(&tx) {
            Ok(sig) => {
                for (input, _) in pending.drain(..len) {
                    println!("reclaimed       {input} {sig}");
                }
            }
            Err(e) => match e.get_transaction_error() {
                // Only this order is to blame: drop it and resend the others.
                Some(TransactionError::InstructionError(index, err))
                    if usize::from(index) < len =>
                {
                    let (input, _) = pending.remove(usize::from(index));
                    match settlement_error(&err) {
                        Some(SettlementError::OrderNotReclaimable) => {
                            println!("not reclaimable {input}")
                        }
                        Some(err) => fail(input, anyhow::Error::new(e).context(format!("{err:?}"))),
                        None => fail(input, anyhow::Error::new(e).context("transaction failed")),
                    }
                }
                _ => {
                    let e = anyhow::Error::new(e).context("transaction failed");
                    for (input, _) in pending.drain(..len) {
                        fail(input, anyhow::anyhow!("{e:#}"));
                    }
                }
            },
        }
    }

    anyhow::ensure!(
        failures.is_empty(),
        "{} of {} orders failed",
        failures.len(),
        args.orders.len()
    );
    Ok(())
}

/// How many of the leading `ixs` fit into one transaction paid by `payer`.
/// At least one, so an instruction too large on its own still gets sent and
/// fails visibly.
fn batch_len(ixs: &[Instruction], payer: &Pubkey) -> usize {
    (2..=ixs.len())
        .take_while(|&len| fits(&ixs[..len], payer))
        .last()
        .unwrap_or(1)
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

    use super::*;
    use crate::utils::transaction::MAX_TRANSACTION_SIZE;

    const PROGRAM_ID: Pubkey = Pubkey::new_from_array([1; 32]);
    const CREATED_BY: Pubkey = Pubkey::new_from_array([2; 32]);

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

    fn tx_size(ixs: &[Instruction]) -> u64 {
        bincode::serialized_size(&Transaction::new_with_payer(ixs, Some(&CREATED_BY))).unwrap()
    }

    #[test]
    fn batch_len_packs_as_many_as_fit() {
        let ixs: Vec<_> = (0..100).map(|_| reclaim_ix(Pubkey::new_unique())).collect();
        let len = batch_len(&ixs, &CREATED_BY);
        assert!(tx_size(&ixs[..len]) <= MAX_TRANSACTION_SIZE);
        assert!(tx_size(&ixs[..=len]) > MAX_TRANSACTION_SIZE);
    }

    #[test]
    fn batch_len_packs_more_orders_sharing_a_creator() {
        let distinct: Vec<_> = (0..100).map(|_| reclaim_ix(Pubkey::new_unique())).collect();
        let shared: Vec<_> = (0..100).map(|_| reclaim_ix(CREATED_BY)).collect();
        assert!(batch_len(&shared, &CREATED_BY) > batch_len(&distinct, &CREATED_BY));
    }

    #[test]
    fn batch_len_takes_everything_that_fits() {
        let ixs = [reclaim_ix(CREATED_BY), reclaim_ix(CREATED_BY)];
        assert_eq!(batch_len(&ixs, &CREATED_BY), 2);
        assert_eq!(batch_len(&ixs[..1], &CREATED_BY), 1);
    }
}
