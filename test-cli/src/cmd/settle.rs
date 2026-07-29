use anyhow::Context as _;
use clap::Args;
use settlement_client::{
    instructions::{
        BeginSettle, CreateBuffers, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull,
    },
    settlement_interface::{
        data::{intent::OrderIntent, order::EncodedOrderAccount},
        pda::buffer::find_buffer_pda,
        Pubkey,
    },
};
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Signature, Signer},
    transaction::Transaction,
};
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use std::collections::HashMap;

use crate::token::{resolve_token_from_account, ResolvedToken};

use super::Context;

#[derive(Args)]
pub struct SettleArgs {
    /// Order UIDs (64-char hex) or PDA addresses (base58), one or more
    #[arg(required = true, num_args = 1..)]
    orders: Vec<String>,

    /// Build and print the settlement without sending the transaction on-chain
    #[arg(long)]
    dry_run: bool,
}

struct ResolvedIntent {
    /// The original order from the user
    data: OrderIntent,

    /// All the information about the sell account's TA and Mint
    sell: ResolvedToken,

    /// All the information about the buy account's TA and Mint
    buy: ResolvedToken,
}

pub fn run(ctx: Context, args: SettleArgs) -> anyhow::Result<()> {
    let intents = resolve_intents(&ctx, &args)?;

    let mut all_ixs: Vec<Instruction> = vec![];
    let sell_amount_pulled = prepare_setup_ixs(&ctx, &args, &intents, &mut all_ixs)?;

    let mut sinks = compute_sinks(&ctx, &sell_amount_pulled);

    let pulls = compute_pulls(&intents, &mut sinks);

    let initialized_intents: Vec<_> = intents
        .iter()
        .zip(pulls.iter())
        .map(|(intent, pulls)| InitializedIntent {
            intent: &intent.data,
            pulls,
        })
        .collect();

    let begin_ix_index = all_ixs.len() as u16;
    let finalize_ix_index = begin_ix_index.saturating_add(1);

    let begin_ix = BeginSettle {
        program_id: ctx.program_id,
        finalize_ix_index,
        orders: &initialized_intents,
        auction_id: 0,
    };

    // Send exactly each order's buy amount; any surplus tokens stay in the buffers.
    let settled: Vec<FinalizedIntent> = intents
        .iter()
        .map(|intent| FinalizedIntent {
            intent: &intent.data,
            mint: intent.buy.mint,
            amount: intent.data.buy_amount,
        })
        .collect();

    let finalize_ix = FinalizeSettle {
        program_id: ctx.program_id,
        begin_ix_index,
        orders: &settled,
    };

    all_ixs.push(begin_ix.into());
    all_ixs.push(finalize_ix.into());

    let sig = if args.dry_run {
        None
    } else {
        Some(send_settle_transaction(&ctx, &all_ixs)?)
    };
    print_settlement_summary(sig.as_ref(), &intents);

    Ok(())
}

/// Resolve each order input to its on-chain intent, then resolve the sell/buy
/// token accounts for every order. Sorted largest-sell-first so that later
/// the packing can be a bit more optimal for matching pull destinations with
/// orders that can fill them.
fn resolve_intents(ctx: &Context, args: &SettleArgs) -> anyhow::Result<Vec<ResolvedIntent>> {
    let intents = args
        .orders
        .iter()
        .map(|s| fetch_order_intent(&ctx.rpc, ctx, s))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let mut intents = intents
        .into_iter()
        .map(|intent| {
            Ok(ResolvedIntent {
                sell: resolve_token_from_account(
                    &ctx.rpc,
                    &ctx.payer.pubkey(),
                    &intent.sell_token_account,
                )?,
                buy: resolve_token_from_account(
                    &ctx.rpc,
                    &ctx.payer.pubkey(),
                    &intent.buy_token_account,
                )?,

                data: intent,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    intents.sort_unstable_by_key(|i| std::cmp::Reverse(i.data.sell_amount));

    Ok(intents)
}

/// Create any missing signer ATAs and buffer PDAs before the settle tx, and
/// tally up the total sell/buy amount per mint across all orders.
fn prepare_setup_ixs(
    ctx: &Context,
    args: &SettleArgs,
    intents: &[ResolvedIntent],
    all_ixs: &mut Vec<Instruction>,
) -> anyhow::Result<HashMap<Pubkey, u64>> {
    let mut sell_amount_pulled: HashMap<Pubkey, u64> = HashMap::new();
    let mut buy_amount_pushed: HashMap<Pubkey, u64> = HashMap::new();
    let mut mint_buffers_to_create: Vec<Pubkey> = Vec::new();

    for (i, intent) in intents.iter().enumerate() {
        if !buy_amount_pushed.contains_key(&intent.buy.mint) {
            let (buffer_pda, _) = find_buffer_pda(&ctx.program_id, &intent.buy.mint);
            if ctx.rpc.get_account(&buffer_pda).is_err() {
                mint_buffers_to_create.push(intent.buy.mint);
            }
        }

        // accumulates the sell and buy amounts (inserts if the key doesn't exist)
        let sell_tally_entry = sell_amount_pulled.entry(intent.sell.mint).or_default();
        *sell_tally_entry = sell_tally_entry.saturating_add(intent.data.sell_amount);
        let buy_tally_entry = buy_amount_pushed.entry(intent.buy.mint).or_default();
        *buy_tally_entry = buy_tally_entry.saturating_add(intent.data.buy_amount);

        if intent.sell.ta_data.owner == Pubkey::default() {
            let named_intent = &args.orders[i];
            let ta = intent.sell.ta;
            anyhow::bail!("Order {named_intent}: sell account {ta} does not exist")
        }

        if intent.buy.ta_data.owner == Pubkey::default() {
            // as of right now, it may be necessary to create the buy token account if it doesn't exist yet
            // here we assume it is an associated token account
            all_ixs.push(create_associated_token_account_idempotent(
                &ctx.payer.pubkey(),
                &intent.data.owner,
                &intent.buy.mint,
                &spl_token_interface::id(),
            ));
        }
    }

    if !mint_buffers_to_create.is_empty() {
        all_ixs.push(
            CreateBuffers {
                program_id: ctx.program_id,
                payer: ctx.payer.pubkey(),
                mints: &mint_buffers_to_create,
            }
            .into(),
        );
    }

    Ok(sell_amount_pulled)
}

/// Compute the pull destinations for a settlement. Every sold token is pulled
/// into its mint's buffer PDA; tokens beyond what's needed to satisfy the
/// orders simply stay in the buffer. Once swap routing exists this will also
/// include exchange routes.
fn compute_sinks(
    ctx: &Context,
    sell_amount_pulled: &HashMap<Pubkey, u64>,
) -> HashMap<Pubkey, Vec<Pull>> {
    sell_amount_pulled
        .iter()
        .map(|(mint, &amount)| {
            let (buffer_pda, _) = find_buffer_pda(&ctx.program_id, mint);
            (
                *mint,
                vec![Pull {
                    destination: buffer_pda,
                    amount,
                }],
            )
        })
        .collect()
}

/// Carve each order's required pull amount out of the shared per-mint sink
/// pool, depleting `sinks` as we go. Whatever remains per mint afterward
/// feeds `compute_push_amounts`.
fn compute_pulls(
    intents: &[ResolvedIntent],
    sinks: &mut HashMap<Pubkey, Vec<Pull>>,
) -> Vec<Vec<Pull>> {
    let mut pulls = Vec::with_capacity(intents.len());
    for intent in intents {
        let mut p = Vec::with_capacity(1);

        let mut to_pull = intent.data.sell_amount;
        sinks.entry(intent.sell.mint).and_modify(|d| {
            while to_pull > 0 {
                let last = d.len().saturating_sub(1);
                if d[last].amount <= to_pull {
                    to_pull = to_pull.saturating_sub(d[last].amount);
                    p.push(d.pop().unwrap());
                } else {
                    p.push(Pull {
                        destination: d[last].destination,
                        amount: to_pull,
                    });
                    d[last].amount = d[last].amount.saturating_sub(to_pull);
                    to_pull = 0;
                }
            }
        });

        pulls.push(p);
    }

    pulls
}

fn send_settle_transaction(ctx: &Context, all_ixs: &[Instruction]) -> anyhow::Result<Signature> {
    let blockhash = ctx.rpc.get_latest_blockhash().context("fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        all_ixs,
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.rpc
        .send_and_confirm_transaction(&tx)
        .context("settle transaction failed")
}

fn print_settlement_summary(sig: Option<&Signature>, intents: &[ResolvedIntent]) {
    match sig {
        Some(sig) => println!("settle: {sig}"),
        None => println!("settle: dry run (transaction not sent)"),
    }
    for (i, intent) in intents.iter().enumerate() {
        println!(
            "  order {i}: pulled {} (sell {}), pushed {} (buy {})",
            intent.data.sell_amount, intent.sell.mint, intent.data.buy_amount, intent.buy.mint,
        );
    }
}

fn fetch_order_intent(
    rpc: &RpcClient,
    ctx: &Context,
    s: &str,
) -> anyhow::Result<OrderIntent> {
    let pda = parse_order_input(ctx, s)?;
    let data = rpc
        .get_account_data(&pda)
        .with_context(|| format!("failed to get order account data for {pda}"))?;
    let bytes: [u8; EncodedOrderAccount::SIZE] = data.as_slice().try_into().map_err(|_| {
        anyhow::anyhow!(
            "unexpected account data length {} for order at {pda}",
            data.len()
        )
    })?;
    let (order_account, _uid) = EncodedOrderAccount::decode_and_hash(&bytes)
        .map_err(|e| anyhow::anyhow!("failed to decode order at {pda}: {e:?}"))?;
    Ok(order_account.intent)
}

/// Accept either a 64-char hex UID or a base58 pubkey (the PDA directly).
fn parse_order_input(ctx: &Context, s: &str) -> anyhow::Result<Pubkey> {
    if let Ok(pubkey) = s.parse::<Pubkey>() {
        return Ok(pubkey);
    }
    anyhow::ensure!(
        s.len() == 64,
        "expected a base58 order PDA or a 64-char hex UID, got '{s}'"
    );

    // TODO: after a bit of research, this appears to be the most recommended way in std + solana_hash to
    // convert a string into a hash. We might want to move this into a proper function later.
    let mut bytes = [0u8; 32];
    for (i, piece) in s.as_bytes().chunks(2).enumerate() {
        bytes[i] = u8::from_str_radix(
            std::str::from_utf8(piece).expect("Should return to utf8 string"),
            16,
        )
        .with_context(|| format!("invalid hex in UID '{s}' at byte {i}"))?;
    }
    let uid = Hash::new_from_array(bytes);
    let (pda, _) =
        settlement_client::settlement_interface::pda::order::find_order_pda(&ctx.program_id, &uid);
    Ok(pda)
}
