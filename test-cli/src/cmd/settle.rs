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
use solana_rpc_client::{api::config::UiTransactionEncoding, rpc_client::RpcClient};
use solana_sdk::{
    signature::{Signature, Signer},
    transaction::Transaction,
};
use std::collections::{HashMap, HashSet};

use crate::token::{interpret_token_from_user_input, ResolvedToken};

use super::Context;

#[derive(Args)]
pub struct SettleArgs {
    /// Order UIDs (64-char hex) or PDA addresses (base58), one or more
    /// The order accounts are expected to have already been created on-chain (i.e. the CLI will not create the orders for you)
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

    let pulls = compute_pulls(&intents, &mut sinks)?;

    let initialized_intents: Vec<_> = intents
        .iter()
        .zip(pulls.iter())
        .map(|(intent, pulls)| InitializedIntent {
            intent: &intent.data,
            pulls,
        })
        .collect();

    let (begin_ix_index, finalize_ix_index) = u16::try_from(all_ixs.len())
        .ok()
        .and_then(|begin| Some((begin, begin.checked_add(1)?)))
        .context("too many instructions: begin/finalize index overflow")?;

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

    let blockhash = ctx.rpc.get_latest_blockhash().context("fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &all_ixs,
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let (units_consumed, result) = if args.dry_run {
        let simulate_result = ctx
            .rpc
            .simulate_transaction(&tx)
            .with_context(|| "dry run simulation of settlement transaction failed")?;
        (
            simulate_result
                .value
                .units_consumed
                .expect("simulation result doesn't include units units_consumed"),
            None,
        )
    } else {
        let sig = ctx
            .rpc
            .send_and_confirm_transaction(&tx)
            .context("settle transaction failed")?;

        let tx_info = ctx
            .rpc
            .get_transaction(&sig, UiTransactionEncoding::Json)
            .expect("could not pull data of finalized transaction");

        (
            tx_info
                .transaction
                .meta
                .with_context(|| format!("transaction {sig} has no context"))?
                .compute_units_consumed
                .expect("transaction meta doesn't include compute_units_consumed"),
            Some(sig),
        )
    };
    print_settlement_summary(result.as_ref(), units_consumed, &intents);

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
                sell: interpret_token_from_user_input(
                    &ctx.rpc,
                    &ctx.payer.pubkey(),
                    &intent.sell_token_account,
                )?,
                buy: interpret_token_from_user_input(
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

/// Tally `amount` for `mint` in `tally`. The first time a mint is seen, also
/// check whether its buffer PDA already exists on-chain and register it for
/// creation if not.
fn tally_and_register_buffer(
    ctx: &Context,
    tally: &mut HashMap<Pubkey, u64>,
    mint_buffers_to_create: &mut HashSet<Pubkey>,
    mint: Pubkey,
    amount: u64,
) -> anyhow::Result<()> {
    match tally.get(&mint) {
        Some(cur_amount) => {
            let new_amount = cur_amount
                .checked_add(amount)
                .with_context(|| format!("trade amount tally overflow for mint {mint}"))?;
            tally.insert(mint, new_amount);
        }
        None => {
            let (buffer_pda, _) = find_buffer_pda(&ctx.program_id, &mint);
            if ctx.rpc.get_account(&buffer_pda).is_err() {
                mint_buffers_to_create.insert(mint);
            }
            tally.insert(mint, amount);
        }
    }

    Ok(())
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
    let mut mint_buffers_to_create: HashSet<Pubkey> = HashSet::new();

    for (i, intent) in intents.iter().enumerate() {
        // for both the buy and sell token: we need to tally the total transfer amounts
        // if this is the first time we are seeing the token, we should also check the buffer account, and create it if necessary.
        tally_and_register_buffer(
            ctx,
            &mut sell_amount_pulled,
            &mut mint_buffers_to_create,
            intent.sell.mint,
            intent.data.sell_amount,
        )?;
        tally_and_register_buffer(
            ctx,
            &mut buy_amount_pushed,
            &mut mint_buffers_to_create,
            intent.buy.mint,
            intent.data.buy_amount,
        )?;

        if intent.sell.create_ata_ix.is_some() {
            let named_intent = &args.orders[i];
            let ta = intent.sell.ta;
            anyhow::bail!("Order {named_intent}: sell account {ta} does not exist")
        }

        // as of right now, it may be necessary to create the buy token account if it doesn't exist yet
        if let Some(create_ata_ix) = &intent.buy.create_ata_ix {
            all_ixs.push(create_ata_ix(&ctx.payer.pubkey()));
        }
    }

    if !mint_buffers_to_create.is_empty() {
        all_ixs.push(
            CreateBuffers {
                program_id: ctx.program_id,
                payer: ctx.payer.pubkey(),
                mints: &mint_buffers_to_create.into_iter().collect::<Vec<_>>(),
            }
            .into(),
        );
    }

    Ok(sell_amount_pulled)
}

/// Compute the pull destinations for a settlement. Every sold token is pulled
/// into its mint's buffer PDA; tokens beyond what's needed to satisfy the
/// orders simply stay in the buffer.
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
) -> anyhow::Result<Vec<Vec<Pull>>> {
    let mut pulls = vec![];
    for intent in intents {
        let mut pulls_for_intent = vec![];

        let mut to_pull = intent.data.sell_amount;
        if let Some(sinks_for_mint) = sinks.get_mut(&intent.sell.mint) {
            while to_pull > 0 {
                let sink_to_fill = sinks_for_mint
                    .last_mut()
                    .context("sink exhausted while computing pulls")?;
                if sink_to_fill.amount <= to_pull {
                    to_pull = to_pull
                        .checked_sub(sink_to_fill.amount)
                        .context("pull amount underflow")?;
                    pulls_for_intent
                        .push(sinks_for_mint.pop().expect("just accessed via last_mut"));
                } else {
                    pulls_for_intent.push(Pull {
                        destination: sink_to_fill.destination,
                        amount: to_pull,
                    });
                    sink_to_fill.amount = sink_to_fill
                        .amount
                        .checked_sub(to_pull)
                        .context("pull amount underflow")?;
                    to_pull = 0;
                }
            }
        }

        pulls.push(pulls_for_intent);
    }

    Ok(pulls)
}

fn print_settlement_summary(
    sig: Option<&Signature>,
    units_consumed: u64,
    intents: &[ResolvedIntent],
) {
    match sig {
        Some(sig) => println!("settle: {sig} ({units_consumed} CU)"),
        None => println!("settle: dry run (simulated success, {units_consumed} CU)"),
    }
    for (i, intent) in intents.iter().enumerate() {
        println!(
            "  order {i}: pulled {} (sell {}), pushed {} (buy {})",
            intent.data.sell_amount, intent.sell.mint, intent.data.buy_amount, intent.buy.mint,
        );
    }
}

fn fetch_order_intent(rpc: &RpcClient, ctx: &Context, s: &str) -> anyhow::Result<OrderIntent> {
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
        s.len() == 64 && s.is_ascii(),
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
