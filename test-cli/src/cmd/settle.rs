use anyhow::Context as _;
use clap::Args;
use settlement_client::{
    instructions::{
        BeginSettle, CreateBuffers, FinalizeSettle, FinalizedIntent, InitializedIntent, Pull
    }, settlement_interface::{
        Pubkey, data::{intent::OrderIntent, order::EncodedOrderAccount}, pda::buffer::find_buffer_pda,
    },
};
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Signature, Signer},
    transaction::Transaction,
};
use spl_associated_token_account_interface::{
    address::get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};
use std::{collections::{HashMap, HashSet}, ops::Add};

use crate::token::{ResolvedToken, resolve_token_from_account};

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

    /// Weight for determining how much of the output an order should get
    contribution_score: u64,

    /// All the information about the sell account's TA and Mint
    sell: ResolvedToken,

    /// All the information about the buy account's TA and Mint
    buy: ResolvedToken,
}

pub fn run(ctx: Context, args: SettleArgs) -> anyhow::Result<()> {
    let intents = resolve_intents(&ctx, &args)?;

    let mut all_ixs: Vec<Instruction> = vec![];
    let (sell_amount_pulled, buy_amount_pushed) =
        prepare_setup_ixs(&ctx, &args, &intents, &mut all_ixs)?;

    let mut sinks = compute_sinks(&ctx, &sell_amount_pulled, &buy_amount_pushed)?;

    // TODO: later this will be computed by a function that takes into account swap outputs
    // but for now the source amount that can be sent as output is just the same as the buffer output
    let sources = sinks.iter().map(|(a, s)| (*a, s[0].amount)).collect();

    let pulls = compute_pulls(&intents, &mut sinks);

    let initialized_intents: Vec<_> = intents
        .iter()
        .zip(pulls.iter())
        .map(|(intent, pulls)| InitializedIntent { intent: &intent.data, pulls })
        .collect();

    let begin_ix_index = all_ixs.len() as u16;
    let finalize_ix_index = begin_ix_index
        .saturating_add(1)
        // TODO: later add mid txs
        .saturating_add(0u16);

    let begin_ix = BeginSettle {
        program_id: ctx.program_id,
        finalize_ix_index,
        orders: &initialized_intents,
    };

    let push_amounts = compute_push_amounts(&intents, &sources);

    let settled: Vec<FinalizedIntent> = intents
        .iter()
        .zip(push_amounts.iter())
        .map(|(intent, &amount)| FinalizedIntent {
            intent: &intent.data,
            mint: intent.buy.mint,
            amount,
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
    print_settlement_summary(sig.as_ref(), &intents, &push_amounts);

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
                sell: resolve_token_from_account(&ctx.rpc, &ctx.payer.pubkey(), &intent.sell_token_account)?,
                buy: resolve_token_from_account(&ctx.rpc, &ctx.payer.pubkey(), &intent.buy_token_account)?,

                // for now: the contribution score weighting is based on the buy amount the user requests
                // clamped to at least 1 in case the user requests 0 in their order to prevent edge cases where all orders have buy_amount 0)
                // in a real circumstance, this should be based on the native price of the sell_amount
                contribution_score: intent.buy_amount.max(1),

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
) -> anyhow::Result<(HashMap<Pubkey, u64>, HashMap<Pubkey, u64>)> {
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

    Ok((sell_amount_pulled, buy_amount_pushed))
}

/// Compute aggregate pull destinations that will be needed for a settlement. Includes stuff
/// like buffer PDAs and, once swap routing exists, exchange routes.
fn compute_sinks(
    ctx: &Context,
    sell_amount_pulled: &HashMap<Pubkey, u64>,
    buy_amount_pushed: &HashMap<Pubkey, u64>,
) -> anyhow::Result<HashMap<Pubkey, Vec<Pull>>> {
    let mut sinks: HashMap<Pubkey, Vec<Pull>> = HashMap::new();

    // start with CoWs. Create a sink that pulls in tokens that don't need to be 
    // traded (aka, min(pulled, pushed)) because they are already going to be consumed by another order
    // and it is assumed that trading is always less efficient than direct matching.
    // this could be 0: if so, it will be populated in the loop later.
    for key in sell_amount_pulled.keys().chain(buy_amount_pushed.keys()) {
        // entry() avoids computing the difference twice for keys in both maps
        sinks.entry(*key).or_insert_with(|| {
            let (buffer_pda, _) = find_buffer_pda(&ctx.program_id, key);
            vec![Pull {
                destination: buffer_pda,
                amount: *sell_amount_pulled.get(key).unwrap_or(&0).min(buy_amount_pushed.get(key).unwrap_or(&0)),
            }]
        });
    }

    // TODO: later we would add exchanges needed for any tokens that still need a swap

    // distribute surplus: 
    for (sell_token, total_sell_amount) in sell_amount_pulled.iter() {
        let d = sinks
            .get_mut(sell_token)
            .expect("sink seeded for every sell mint in the CoW loop above");

        let sunk_total: u64 = d.iter().map(|p| p.amount).sum();
        let surplus = total_sell_amount.saturating_sub(sunk_total);

        if surplus > 0 {
            // edge case: if no sinks were needed (this could happen if the destination side order
            // requested a buy_amount of 0 tokens), send all the surplus to the buffer for distribution
            if sunk_total == 0 {
                d[0].amount = surplus;
            } else {
                // we want to send tokens proportionally to each sink
                let weights: Vec<u64> = d.iter().map(|p| p.amount).collect();
                for (pull, increment) in d.iter_mut().zip(distribute_proportionally(&weights, surplus)) {
                    pull.amount = pull.amount.saturating_add(increment);
                }
            }
        }

        // finally, sort for more efficient pull matching later
        d.sort_unstable_by_key(|p| std::cmp::Reverse(p.amount));
    }

    Ok(sinks)
}

/// Distribute `extra` proportionally across `weights` (each entry's current
/// amount), using largest-remainder rounding so the increments sum to
/// exactly `extra`.
fn distribute_proportionally(weights: &[u64], extra: u64) -> Vec<u64> {
    let total_weight: u128 = weights.iter().map(|&w| w as u128).sum();
    let extra128 = extra as u128;

    let mut increments: Vec<u64> = weights
        .iter()
        .map(|&w| extra128.saturating_mul(w as u128).checked_div(total_weight).unwrap_or(0) as u64)
        .collect();

    let sum: u64 = increments.iter().fold(0u64, |acc, &s| acc.saturating_add(s));
    let leftover = extra.saturating_sub(sum) as usize;
    if leftover > 0 {
        let mut fracs: Vec<(usize, u128)> = weights
            .iter()
            .enumerate()
            .map(|(j, &w)| (j, extra128.saturating_mul(w as u128).checked_rem(total_weight).unwrap_or(0)))
            .collect();
        fracs.sort_by_key(|a| std::cmp::Reverse(a.1));
        for k in 0..leftover {
            increments[fracs[k].0] = increments[fracs[k].0].saturating_add(1);
        }
    }

    increments
}

/// Carve each order's required pull amount out of the shared per-mint sink
/// pool, depleting `sinks` as we go. Whatever remains per mint afterward
/// feeds `compute_push_amounts`.
fn compute_pulls(intents: &[ResolvedIntent], sinks: &mut HashMap<Pubkey, Vec<Pull>>) -> Vec<Vec<Pull>> {
    let mut pulls = Vec::with_capacity(intents.len());
    for intent in intents {
        let mut p = Vec::with_capacity(1);

        let mut to_pull = intent.data.sell_amount;
        sinks.entry(intent.sell.mint).and_modify(|d| {
            while to_pull > 0 {
                let last = d.len() - 1;
                if d[last].amount <= to_pull {
                    to_pull -= d[last].amount;
                    p.push(d.pop().unwrap());
                } else {
                    p.push(Pull { destination: d[last].destination, amount: to_pull });
                    d[last].amount = d[last].amount.saturating_sub(to_pull);
                    to_pull = 0;
                }
            }
        });

        pulls.push(p);
    }

    pulls
}

/// Computed buy amounts: each order receives from the total available output tokens weighted proportional
/// to its contribution_score
fn compute_push_amounts(
    intents: &[ResolvedIntent],
    sources: &HashMap<Pubkey, u64>
) -> Vec<u64> {
    let mut orders_by_mint: HashMap<Pubkey, Vec<usize>> = HashMap::new();
    for (i, intent) in intents.iter().enumerate() {
        orders_by_mint.entry(intent.buy.mint).or_default().push(i);
    }

    let mut result = vec![0u64; intents.len()];

    for (mint, indices) in &orders_by_mint {
        let avail = *sources.get(mint).unwrap_or(&0);
        if avail == 0 {
            continue;
        }

        // we distribute based on contribution score
        // NOTE: technically its possible if contribution_score is not scaled to the expected buy_amount,
        // the output token could be insufficient for a user. In this case, it can be decided that that particular
        // order would be unsolvable, or it has to truncate. but since right now it *is* based on buy_amount, we are fine with
        // this simple calculation
        let weights: Vec<u64> = indices.iter().map(|&i| intents[i].contribution_score).collect();

        for (&order_idx, share) in indices.iter().zip(distribute_proportionally(&weights, avail)) {
            result[order_idx] = share;
        }
    }

    result
}

fn send_settle_transaction(ctx: &Context, all_ixs: &[Instruction]) -> anyhow::Result<Signature> {
    let blockhash = ctx.rpc.get_latest_blockhash().context("fetch blockhash")?;
    let tx =
        Transaction::new_signed_with_payer(all_ixs, Some(&ctx.payer.pubkey()), &[&ctx.payer], blockhash);
    ctx.rpc
        .send_and_confirm_transaction(&tx)
        .context("settle transaction failed")
}

fn print_settlement_summary(sig: Option<&Signature>, intents: &[ResolvedIntent], push_amounts: &[u64]) {
    match sig {
        Some(sig) => println!("settle: {sig}"),
        None => println!("settle: dry run (transaction not sent)"),
    }
    for (i, (intent, &pushed)) in intents.iter().zip(push_amounts.iter()).enumerate() {
        println!(
            "  order {i}: pulled {} (sell {}), pushed {} (buy {}){}",
            intent.data.sell_amount,
            intent.sell.mint,
            pushed,
            intent.buy.mint,
            if pushed > intent.data.buy_amount {
                format!(" [+{} surplus]", pushed.saturating_sub(intent.data.buy_amount))
            } else {
                String::new()
            },
        );
    }
}

fn fetch_order_intent(
    rpc: &RpcClient,
    ctx: &Context,
    s: &str,
) -> anyhow::Result<settlement_client::settlement_interface::data::intent::OrderIntent> {
    let pda = parse_order_input(ctx, s)?;
    let data = rpc
        .get_account_data(&pda)
        .with_context(|| format!("order account {pda} not found on-chain"))?;
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
        bytes[i] = u8::from_str_radix(std::str::from_utf8(piece).expect("Should return to utf8 string"), 16)
            .with_context(|| format!("invalid hex in UID '{s}' at byte {i}"))?;
    }
    let uid = Hash::new_from_array(bytes);
    let (pda, _) =
        settlement_client::settlement_interface::pda::order::find_order_pda(&ctx.program_id, &uid);
    Ok(pda)
}

/// # TODO
/// Wire up the Orca Whirlpools SDK (e.g. `orca-whirlpools-client`) to build
/// the real `swap` instruction. The function signature is ready; only the body
/// needs filling in once the dependency is added. For CoW settlements
/// (opposite-direction orders), no swap is needed — call this only when
/// the signer lacks the buy tokens and cannot cover them via CoW matching.
#[allow(dead_code)]
fn orca_swap(
    sell_mint: &Pubkey,
    buy_mint: &Pubkey,
    _input_ata: &Pubkey,
    _output_ata: &Pubkey,
    _authority: &Pubkey,
) -> anyhow::Result<Instruction> {
    anyhow::bail!(
        "swap from {} to {} required; Orca Whirlpool integration is not yet implemented — \
         add the orca-whirlpools-client crate and fill in `orca_swap` in settle.rs",
        sell_mint,
        buy_mint,
    )
}
