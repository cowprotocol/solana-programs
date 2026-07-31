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
use solana_rpc_client::{
    api::config::{RpcTransactionConfig, UiTransactionEncoding},
    rpc_client::RpcClient,
};
use solana_sdk::{
    signature::{Signature, Signer},
    transaction::Transaction,
};
use std::collections::{HashMap, HashSet};

use crate::token::{resolve_from_token_account, ResolvedToken};

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

/// What happened to the settlement transaction, as reported by
/// [`print_settlement_summary`].
enum SettleOutcome {
    /// Sent on-chain and confirmed.
    Sent { sig: Signature, units_consumed: u64 },

    /// `--dry-run`: simulated without an error.
    Simulated { units_consumed: u64 },

    /// `--dry-run`: the simulation itself reverted. The RPC call succeeds in this case,
    /// so the error only shows up in the simulation result.
    SimulationFailed {
        err: String,
        units_consumed: Option<u64>,
        logs: Vec<String>,
    },
}

impl SettleOutcome {
    /// The headline status line of the settlement summary.
    fn status_line(&self) -> String {
        match self {
            Self::Sent {
                sig,
                units_consumed,
            } => format!("settle: {sig} ({units_consumed} CU)"),
            Self::Simulated { units_consumed } => {
                format!("settle: dry run (simulated success, {units_consumed} CU)")
            }
            Self::SimulationFailed {
                err,
                units_consumed: Some(units_consumed),
                ..
            } => format!("settle: dry run (SIMULATION FAILED: {err}, {units_consumed} CU)"),
            Self::SimulationFailed { err, .. } => {
                format!("settle: dry run (SIMULATION FAILED: {err})")
            }
        }
    }
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
    prepare_setup_ixs(&ctx, &intents, &mut all_ixs)?;

    let pulls = compute_pulls(&ctx, &intents);

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

    let outcome = if args.dry_run {
        // A reverting simulation is still a successful RPC call, with the program error in
        // `value.err` — without checking it a failed settlement reads as a successful dry run.
        let simulated = ctx
            .rpc
            .simulate_transaction(&tx)
            .with_context(|| "dry run simulation of settlement transaction failed")?
            .value;

        match simulated.err {
            Some(err) => SettleOutcome::SimulationFailed {
                err: err.to_string(),
                units_consumed: simulated.units_consumed,
                logs: simulated.logs.unwrap_or_default(),
            },
            None => SettleOutcome::Simulated {
                units_consumed: simulated
                    .units_consumed
                    .context("simulation result doesn't include units_consumed")?,
            },
        }
    } else {
        let sig = ctx
            .rpc
            .send_and_confirm_transaction(&tx)
            .context("settle transaction failed")?;

        // `get_transaction` sends no commitment, so the node defaults to `finalized` and
        // returns null for a tx that is merely confirmed. Ask at the client's own
        // commitment instead, which is what `send_and_confirm_transaction` waited for.
        let tx_info = ctx
            .rpc
            .get_transaction_with_config(
                &sig,
                RpcTransactionConfig {
                    commitment: Some(ctx.rpc.commitment()),
                    ..Default::default()
                },
            )
            .with_context(|| format!("could not pull data of confirmed transaction {sig}"))?;

        SettleOutcome::Sent {
            sig,
            units_consumed: tx_info
                .transaction
                .meta
                .with_context(|| format!("transaction {sig} has no context"))?
                .compute_units_consumed
                .ok_or_else(|| {
                    anyhow::anyhow!("transaction meta doesn't include compute_units_consumed")
                })?,
        }
    };
    print_settlement_summary(&outcome, &intents);

    // The summary already spells out what went wrong; keep the exit code non-zero so a
    // failed dry run isn't mistaken for a passing one by whatever is driving the CLI.
    anyhow::ensure!(
        !matches!(outcome, SettleOutcome::SimulationFailed { .. }),
        "dry run simulation of settlement transaction failed",
    );

    Ok(())
}

/// Resolve each order input to its on-chain intent, then resolve the sell/buy
/// token accounts for every order to determine mint and anything else pertinent.
/// NOTE: Both `sell_token_account` and `buy_token_account` must exist as
/// there is no on-chain mechanism to identify the mint account needed to create
/// the ATA for the account if it doesn't exist
fn resolve_intents(ctx: &Context, args: &SettleArgs) -> anyhow::Result<Vec<ResolvedIntent>> {
    let intents = args
        .orders
        .iter()
        .map(|s| fetch_order_intent(&ctx.rpc, ctx, s))
        .collect::<anyhow::Result<Vec<_>>>()?;

    intents
        .into_iter()
        .map(|intent| {
            Ok(ResolvedIntent {
                sell: resolve_from_token_account(&ctx.rpc, &intent.sell_token_account)?,
                buy: resolve_from_token_account(&ctx.rpc, &intent.buy_token_account)?,

                data: intent,
            })
        })
        .collect()
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

/// Create any missing buffer PDAs before the settle tx, and check that the
/// settlement is self-funding (see [`ensure_cow_balance`]).
fn prepare_setup_ixs(
    ctx: &Context,
    intents: &[ResolvedIntent],
    all_ixs: &mut Vec<Instruction>,
) -> anyhow::Result<()> {
    let mut sell_amount_pulled: HashMap<Pubkey, u64> = HashMap::new();
    let mut buy_amount_pushed: HashMap<Pubkey, u64> = HashMap::new();
    let mut mint_buffers_to_create: HashSet<Pubkey> = HashSet::new();

    for intent in intents {
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
    }

    ensure_cow_balance(&sell_amount_pulled, &buy_amount_pushed)?;

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

    Ok(())
}

/// Check that the orders can settle against each other: for every mint, the
/// amount the users sell into its buffer must cover the amount the settlement
/// pushes back out of it. Otherwise there is nothing to CoW the orders with and
/// the settlement would have to be funded from elsewhere.
///
/// This deliberately ignores whatever the buffers already hold, so a settlement
/// that would only work by spending pre-existing buffer balances is rejected
/// here rather than silently draining them.
fn ensure_cow_balance(
    sell_amount_pulled: &HashMap<Pubkey, u64>,
    buy_amount_pushed: &HashMap<Pubkey, u64>,
) -> anyhow::Result<()> {
    for (mint, &pushed) in buy_amount_pushed {
        let pulled = sell_amount_pulled.get(mint).copied().unwrap_or_default();
        anyhow::ensure!(
            pulled >= pushed,
            "orders can't be CoW'd: mint {mint} needs {pushed} pushed out of its buffer \
             but only {pulled} is sold into it",
        );
    }

    Ok(())
}

/// Compute the pull destinations for a settlement: every order's full sell
/// amount goes into the buffer PDA of its sell mint. The proceeds each user
/// receives are their limit price (the order's buy amount), so any surplus
/// simply stays in the buffers.
fn compute_pulls(ctx: &Context, intents: &[ResolvedIntent]) -> Vec<[Pull; 1]> {
    intents
        .iter()
        .map(|intent| {
            let (buffer_pda, _) = find_buffer_pda(&ctx.program_id, &intent.sell.mint);
            [Pull {
                destination: buffer_pda,
                amount: intent.data.sell_amount,
            }]
        })
        .collect()
}

fn print_settlement_summary(outcome: &SettleOutcome, intents: &[ResolvedIntent]) {
    println!("{}", outcome.status_line());
    for (i, intent) in intents.iter().enumerate() {
        println!(
            "  order {i}: pulled {} (sell {}), pushed {} (buy {})",
            intent.data.sell_amount, intent.sell.mint, intent.data.buy_amount, intent.buy.mint,
        );
    }

    // The amounts above are what the settlement *would* have moved, so the logs go last
    // to explain why it didn't.
    if let SettleOutcome::SimulationFailed { logs, .. } = outcome {
        if logs.is_empty() {
            println!("  (simulation returned no logs)");
        } else {
            println!("  simulation logs:");
            for line in logs {
                println!("    {line}");
            }
        }
    }
}

fn fetch_order_intent(rpc: &RpcClient, ctx: &Context, s: &str) -> anyhow::Result<OrderIntent> {
    let pda = parse_order_input(&ctx.program_id, s)?;
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
fn parse_order_input(program_id: &Pubkey, s: &str) -> anyhow::Result<Pubkey> {
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
            std::str::from_utf8(piece).context("Should return to utf8 string")?,
            16,
        )
        .with_context(|| format!("invalid hex in UID '{s}' at byte {i}"))?;
    }
    let uid = Hash::new_from_array(bytes);
    let (pda, _) =
        settlement_client::settlement_interface::pda::order::find_order_pda(program_id, &uid);
    Ok(pda)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_distinguishes_simulated_success_from_failure() {
        let ok = SettleOutcome::Simulated {
            units_consumed: 1234,
        }
        .status_line();
        assert_eq!(ok, "settle: dry run (simulated success, 1234 CU)");

        let failed = SettleOutcome::SimulationFailed {
            err: "Error processing Instruction 1: custom program error: 0x2".to_string(),
            units_consumed: Some(99),
            logs: vec![],
        }
        .status_line();
        assert!(
            failed.contains("SIMULATION FAILED") && failed.contains("custom program error: 0x2"),
            "failure should be reported in the status line: {failed}"
        );
        assert!(
            !failed.contains("success"),
            "a reverting simulation must not read as a success: {failed}"
        );
    }

    #[test]
    fn status_line_omits_compute_units_when_simulation_reports_none() {
        let line = SettleOutcome::SimulationFailed {
            err: "AccountNotFound".to_string(),
            units_consumed: None,
            logs: vec![],
        }
        .status_line();
        assert_eq!(line, "settle: dry run (SIMULATION FAILED: AccountNotFound)");
    }

    #[test]
    fn cow_balance_accepts_covered_and_exact_mints() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        // `a` is oversold (the surplus stays in its buffer), `b` matches exactly.
        let sold = HashMap::from([(a, 100), (b, 50)]);
        let bought = HashMap::from([(a, 90), (b, 50)]);
        assert!(ensure_cow_balance(&sold, &bought).is_ok());
    }

    #[test]
    fn cow_balance_rejects_undersold_mint() {
        let mint = Pubkey::new_unique();
        let sold = HashMap::from([(mint, 10)]);
        let bought = HashMap::from([(mint, 11)]);
        let err = ensure_cow_balance(&sold, &bought)
            .expect_err("pushing more than is sold can't be CoW'd")
            .to_string();
        assert!(err.contains("can't be CoW'd"), "unexpected error: {err}");
    }

    #[test]
    fn cow_balance_rejects_mint_that_is_only_bought() {
        // A mint nobody sells has nothing in its buffer to push from.
        let bought = HashMap::from([(Pubkey::new_unique(), 1)]);
        assert!(ensure_cow_balance(&HashMap::new(), &bought).is_err());
    }

    #[test]
    fn parse_order_input_passes_through_base58_pda() {
        let pda = Pubkey::new_unique();
        assert_eq!(
            parse_order_input(&Pubkey::new_unique(), &pda.to_string()).unwrap(),
            pda,
        );
    }

    #[test]
    fn parse_order_input_derives_pda_from_hex_uid() {
        let program_id = Pubkey::new_unique();
        let uid = Hash::new_from_array(*Pubkey::new_unique().as_array());
        let (expected, _) =
            settlement_client::settlement_interface::pda::order::find_order_pda(&program_id, &uid);

        // `Hash`'s `Display` is base58, so spell the hex out from the raw bytes.
        let hex: String = uid.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex.len(), 64);

        assert_eq!(parse_order_input(&program_id, &hex).unwrap(), expected);
    }

    #[test]
    fn parse_order_input_rejects_input_that_is_neither() {
        // Not base58 (so not a pubkey) and not 64 chars long.
        let err = parse_order_input(&Pubkey::new_unique(), "0x1234")
            .expect_err("short non-base58 input is not a valid order reference")
            .to_string();
        assert!(err.contains("expected a base58 order PDA"), "{err}");

        // Right length, but not hex.
        let err = parse_order_input(&Pubkey::new_unique(), &"z".repeat(64))
            .expect_err("64 non-hex chars are not a valid UID")
            .to_string();
        assert!(err.contains("invalid hex in UID"), "{err}");
    }
}
