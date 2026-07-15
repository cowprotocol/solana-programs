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
use solana_sdk::{signature::Signer, transaction::Transaction};
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
}

struct ResolvedIntent {
    data: OrderIntent,
    sell: ResolvedToken,
    buy: ResolvedToken,
}

pub fn run(ctx: Context, args: SettleArgs) -> anyhow::Result<()> {
    // Resolve each input → fetch and decode the on-chain order account.
    let intents = args
        .orders
        .iter()
        .map(|s| fetch_order_intent(&ctx.rpc, &ctx, s))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Fetch the sell and buy mints for every order.
    let mut intents = intents
        .into_iter()
        .map(|intent| {
            Ok(ResolvedIntent {
                sell: resolve_token_from_account(&ctx.rpc, &ctx.payer.pubkey(), &intent.sell_token_account)?,
                buy: resolve_token_from_account(&ctx.rpc, &ctx.payer.pubkey(), &intent.buy_token_account)?,
                data: intent,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // here we sort so that later the packing can be a bit more optimal for matching the pull destinations with orders that can fill them.
    intents.sort_unstable_by_key(|i| std::cmp::Reverse(i.data.sell_amount));

    let mut all_ixs: Vec<Instruction> = vec![];

    let mut sell_amount_pulled: HashMap<Pubkey, u64> = HashMap::new();
    let mut buy_amount_pushed: HashMap<Pubkey, u64> = HashMap::new();

    let mut mint_buffers_to_create: Vec<Pubkey> = Vec::new();

    // Create any missing signer ATAs and buffer PDAs before the settle tx.
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
    // represents the balance change of the buffer with the current operational state:
    // positive = surplus generated in the contract
    // negative = buffers will be consumed
    //let mut balances: HashMap<Pubkey, i64> = HashMap::new();

    /*for key in sell_amount_pulled.keys().chain(buy_amount_pushed.keys()) {
        // entry() avoids computing the difference twice for keys in both maps
        balances.entry(key.clone()).or_insert_with(|| {
            let pulled = *sell_amount_pulled.get(key).unwrap_or(&0) as i64;
            let pushed = *buy_amount_pushed.get(key).unwrap_or(&0) as i64;
            pulled - pushed
        });
    }*/

    //let (surplus_tokens, shortfall_tokens): (Vec<_>, Vec<_>) = balances.iter().partition(|(t, a)| a.is_positive());

    let mut demands: HashMap<Pubkey, Vec<Pull>> = HashMap::new();

    // start with CoWs
    for key in sell_amount_pulled.keys().chain(buy_amount_pushed.keys()) {
        // entry() avoids computing the difference twice for keys in both maps
        demands.entry(key.clone()).or_insert_with(|| {
            let (buffer_pda, _) = find_buffer_pda(&ctx.program_id, key);
            vec![Pull { destination: buffer_pda, amount: *sell_amount_pulled.get(key).unwrap_or(&0).min(buy_amount_pushed.get(key).unwrap_or(&0)) }]
        });
    }

    // find shortfall tokens and find out how to trade between them (generates more "demands")
    /*for shortfall in shortfall_tokens {
        // hypothetically here we can take some of the surplus tokens and trade them into shortfall tokens
        anyhow::bail!("Not implemented: Trade cannot be completed as a perfect CoW")
    }*/

    // at this point, all buffers should be in surplus

    // end with surplus: anything not needed elsewhere should be added to the buffers (always the first pda added)
    for (sell_token, total_sell_amount) in sell_amount_pulled {
        demands.entry(sell_token).and_modify(|d| {
            let demands_sum = d.iter().map(|p| p.amount).sum();
            d[0].amount = total_sell_amount.saturating_sub(demands_sum);

            // finally, sort demands smallest to largest
            d.sort_unstable_by_key(|p| p.amount);
        });
    }
    
    let mut pulls = Vec::with_capacity(intents.len());
    for intent in &intents {
        let mut p = Vec::with_capacity(1);

        let mut to_pull = intent.data.sell_amount;
        demands.entry(intent.sell.mint).and_modify(|d| {
            while to_pull > 0 {
                if d[0].amount <= to_pull {
                    to_pull -= d[0].amount;
                    p.push(d.pop().unwrap());
                }
                else {
                    p.push(Pull { destination: d[0].destination, amount: to_pull });
                    d[0].amount = d[0].amount.saturating_sub(to_pull);
                    to_pull = 0;
                }
            }
        });

        pulls.push(p);
    }


    let initialized_intents: Vec<_> = intents.iter()
        .zip(pulls.iter())
        .map(|(intent, pulls)| InitializedIntent { intent: &intent.data, pulls: pulls })
        .collect();

    let begin_ix_index = all_ixs.len() as u16;
    let finalize_ix_index = begin_ix_index
        .saturating_add(1)
        // TODO: later add mid txs
        .saturating_add(0 as u16);

    let begin_ix = BeginSettle {
        program_id: ctx.program_id,
        finalize_ix_index,
        orders: &initialized_intents,
    };

    // Proportional push amounts: each order buying mint M receives
    //   available[M] * buy_amount / sum(buy_amounts for M)
    // using largest-remainder rounding so the total pushed equals available[M].
    let buy_amounts: Vec<u64> = intents.iter().map(|i| i.data.buy_amount).collect();
    let buy_amount_available = demands.iter().map(|(k, v)| (*k, v.iter().map(|p| p.amount).sum())).collect();
    let push_amounts =
        proportional_shares(&buy_amounts, &intents, &buy_amount_available, &buy_amount_pushed);

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

    let blockhash = ctx.rpc.get_latest_blockhash().context("fetch blockhash")?;
    let tx =
        Transaction::new_signed_with_payer(&all_ixs, Some(&ctx.payer.pubkey()), &[&ctx.payer], blockhash);
    let sig = ctx.rpc
        .send_and_confirm_transaction(&tx)
        .context("settle transaction failed")?;

    println!("settle: {sig}");
    for (i, ((intent, pair), &pushed)) in intents
        .iter()
        .zip(intents.iter())
        .zip(push_amounts.iter())
        .enumerate()
    {
        println!(
            "  order {i}: pulled {} (sell {}), pushed {} (buy {}){}",
            intent.data.sell_amount,
            pair.sell.mint,
            pushed,
            pair.buy.mint,
            if pushed > intent.data.buy_amount {
                format!(" [+{} surplus]", pushed.saturating_sub(intent.data.buy_amount))
            } else {
                String::new()
            },
        );
    }
    Ok(())
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

fn signer_ata(signer: &Pubkey, mint: &Pubkey) -> Pubkey {
    get_associated_token_address_with_program_id(signer, mint, &spl_token_interface::id())
}

fn token_transfer(
    source: &Pubkey,
    destination: &Pubkey,
    authority: &Pubkey,
    amount: u64,
) -> anyhow::Result<Instruction> {
    spl_token_interface::instruction::transfer(
        &spl_token_interface::id(),
        source,
        destination,
        authority,
        &[],
        amount,
    )
    .context("build SPL token transfer instruction")
}

/// Compute proportional push amounts for a set of orders sharing a buy mint.
///
/// Each order receives `available[M] * buy_amount / sum(buy_amounts for M)`.
/// If every order sharing mint `M` has `buy_amount == 0` (no minimum set,
/// e.g. `cow sell 1.0 SOL USDC`), the group is weighted evenly instead. The
/// largest-remainder method distributes any integer rounding leftover so
/// that `sum(push_amounts for M) == available[M]` exactly.
///
/// `buy_amounts[i]` is `intent.buy_amount` for order `i`.
/// `pairs[i].buy_mint` identifies which mint group order `i` belongs to.
fn proportional_shares(
    buy_amounts: &[u64],
    pairs: &[ResolvedIntent],
    available: &HashMap<Pubkey, u64>,
    needed: &HashMap<Pubkey, u64>,
) -> Vec<u64> {
    let mut orders_by_mint: HashMap<Pubkey, Vec<usize>> = HashMap::new();
    for (i, pair) in pairs.iter().enumerate() {
        orders_by_mint.entry(pair.buy.mint).or_default().push(i);
    }

    let mut result = vec![0u64; buy_amounts.len()];

    for (mint, indices) in &orders_by_mint {
        let avail = *available.get(mint).unwrap_or(&0);
        if avail == 0 {
            continue;
        }
        let total = *needed.get(mint).unwrap_or(&0);

        // Weight each order by its buy_amount. If every order in this mint
        // group left buy_amount at 0 ("receive any amount", the default for
        // e.g. `cow sell 1.0 SOL USDC`), total == 0 and there's no ratio to
        // weight by — split the available surplus evenly instead of
        // dropping it.
        let weights: Vec<u128> = if total == 0 {
            vec![1u128; indices.len()]
        } else {
            indices.iter().map(|&i| buy_amounts[i] as u128).collect()
        };
        let total_weight: u128 = weights.iter().sum();
        let avail128 = avail as u128;

        // Floor share for each order.
        let mut shares: Vec<u64> = weights
            .iter()
            .map(|&w| {
                avail128
                    .saturating_mul(w)
                    .checked_div(total_weight)
                    .unwrap_or(0) as u64
            })
            .collect();

        // Distribute rounding leftover one token at a time to orders with the
        // largest fractional parts (largest-remainder method).
        let sum_shares: u64 = shares.iter().fold(0u64, |acc, &s| acc.saturating_add(s));
        let leftover = avail.saturating_sub(sum_shares) as usize;
        if leftover > 0 {
            let mut fracs: Vec<(usize, u128)> = weights
                .iter()
                .enumerate()
                .map(|(j, &w)| {
                    (
                        j,
                        avail128
                            .saturating_mul(w)
                            .checked_rem(total_weight)
                            .unwrap_or(0),
                    )
                })
                .collect();
            fracs.sort_by_key(|a| std::cmp::Reverse(a.1));
            for k in 0..leftover {
                shares[fracs[k].0] = shares[fracs[k].0].saturating_add(1);
            }
        }

        for (&order_idx, share) in indices.iter().zip(shares) {
            result[order_idx] = share;
        }
    }

    result
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
