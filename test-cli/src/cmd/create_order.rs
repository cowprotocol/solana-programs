use anyhow::Context as _;
use clap::{Args as ClapArgs, Parser};
use settlement_client::{
    instructions::CreateOrder,
    settlement_interface::{
        data::intent::{OrderIntent, OrderKind},
        pda::order::find_order_pda,
    },
};
use solana_sdk::{signature::Signer, transaction::Transaction};
use std::time::{SystemTime, UNIX_EPOCH};

use super::Context;
use crate::token::ResolvedToken;

#[derive(ClapArgs)]
struct CommonArgs {
    /// Unix timestamp after which the order expires (defaults to 5 minutes from now)
    #[arg(long, default_value_t = valid_to_in(300))]
    valid_to: u32,

    /// Allow partial fills across multiple settlements
    #[arg(long)]
    partially_fillable: bool,
}

#[derive(Parser)]
#[command(long_about = "\
Create an order to sell or buy exactly N token, per the subcommand used \
(`cow sell` / `cow buy`). This only creates the order on-chain; it does not \
itself execute the swap. Supported forms:

  cow sell 1.0 SOL USDC             sell exactly 1.0 SOL, receive any USDC
  cow sell 1.0 SOL 50.0 USDC        sell exactly 1.0 SOL, receive ≥ 50.0 USDC
  cow sell 1.0 SOL for 50.0 USDC    same as before, but more english
  cow sell 1.0 USDC                 sell 1.0 USDC into SOL (SOL implied as buy token)
  cow buy 1.0 SOL 100.0 USDC        buy exactly 1.0 SOL, spend at most 100.0 USDC
  cow buy 1.0 SOL for 100.0 USDC    same as before, but more english
  cow buy 1.0 SOL USDC              buy exactly 1.0 SOL, spending any USDC
  cow buy 1.0 USDC                  buy 1.0 USDC, selling any amount of SOL (implied)

Tokens can be a builtin symbol (SOL, WSOL, USDC), a mint address, or a token-account address.")]
pub struct BuyOrSellArgs {
    /// Remaining terms (tokens and/or an amount) — run with --help for supported forms
    #[arg(num_args = 2..=5)]
    terms: Vec<String>,

    #[command(flatten)]
    common: CommonArgs,
}

pub fn run_sell(ctx: Context, args: BuyOrSellArgs) -> anyhow::Result<()> {
    let BuyOrSellArgs { terms, common } = args;
    let parsed = parse(&ctx, OrderKind::Sell, &terms)?;
    execute(ctx, parsed, common)
}

pub fn run_buy(ctx: Context, args: BuyOrSellArgs) -> anyhow::Result<()> {
    let BuyOrSellArgs { terms, common } = args;
    let parsed = parse(&ctx, OrderKind::Buy, &terms)?;
    execute(ctx, parsed, common)
}

/// Fully resolved and in sell/buy order — `parse` applies the buy-side flip
/// and does token/amount resolution, so `execute` just builds instructions.
struct ParsedOrder {
    kind: OrderKind,
    sell: ResolvedToken,
    sell_amount: u64,
    sell_is_sol: bool,
    buy: ResolvedToken,
    buy_amount: u64,
}

fn parse(ctx: &Context, kind: OrderKind, terms: &[String]) -> anyhow::Result<ParsedOrder> {
    let term_refs: Vec<&str> = terms.iter().map(String::as_str).collect();
    let terms = &term_refs[..];

    // first two terms are always the same
    let [a_amount, a_tok, ..] = terms else {
        anyhow::bail!(
            "must provide at least an amount and a token; run `cow sell --help` for usage"
        );
    };

    // next token could be "for" or "with" (ignored), a second token, or a numeric amount

    // a/b are in the order the user typed them, not sell/buy order.
    let (b_tok, b_amount) = match terms[2..] {
        [] => ("SOL", "0"),
        ["for" | "with", b] => (b, "0"),
        ["for" | "with", buy_amount, b] => (b, buy_amount),
        [b] => (b, "0"),
        [buy_amount, b] => (b, buy_amount),
        _ => anyhow::bail!(
            "cannot interpret {:?}; run `cow sell --help` for usage",
            terms
        ),
    };

    let (sell_tok, sell_amount_str, buy_tok, buy_amount_str) = match kind {
        OrderKind::Sell => (*a_tok, *a_amount, b_tok, b_amount),
        OrderKind::Buy => (b_tok, b_amount, *a_tok, *a_amount),
    };

    let sell = crate::token::resolve(&ctx.rpc, &ctx.payer.pubkey(), sell_tok)?;
    let buy = crate::token::resolve(&ctx.rpc, &ctx.payer.pubkey(), buy_tok)?;

    let sell_amount =
        spl_token::try_ui_amount_into_amount(sell_amount_str.to_string(), sell.mint_data.decimals)
            .map_err(|_| anyhow::anyhow!("invalid sell amount: {sell_amount_str}"))?;
    let buy_amount = spl_token::try_ui_amount_into_amount(buy_amount_str.to_string(), buy.mint_data.decimals)
        .map_err(|_| anyhow::anyhow!("invalid buy amount: {buy_amount_str}"))?;

    Ok(ParsedOrder {
        kind,
        sell_is_sol: sell_tok.eq_ignore_ascii_case("sol"),
        sell,
        sell_amount,
        buy,
        buy_amount,
    })
}

fn execute(ctx: Context, parsed: ParsedOrder, common: CommonArgs) -> anyhow::Result<()> {
    let ParsedOrder {
        kind,
        sell,
        sell_amount,
        sell_is_sol,
        buy,
        buy_amount,
    } = parsed;

    // If the sell token is SOL, wrap it into the payer's WSOL ATA first.
    // NOTE: later this will be swapped for the solflow program.
    let mut ixs = Vec::new();

    if sell_is_sol {
        let (wsol_ata, wrap_ixs) = crate::instructions::wrap_sol(&ctx.payer.pubkey(), sell_amount)?;
        assert_eq!(wsol_ata, sell.ta, "resolved WSOL ATA mismatch");
        ixs.extend(wrap_ixs);
    }

    // Approve the settlement program to pull sell tokens on our behalf.
    ixs.push(crate::instructions::approve(
        &ctx.program_id,
        &sell.ta,
        &ctx.payer.pubkey(),
        sell_amount,
    )?);

    let intent = OrderIntent {
        owner: ctx.payer.pubkey(),
        sell_token_account: sell.ta,
        buy_token_account: buy.ta,
        sell_amount,
        buy_amount,
        valid_to: common.valid_to,
        kind,
        partially_fillable: common.partially_fillable,
        app_data: [0u8; 32],
    };

    let uid = intent.uid();
    let (order_pda, _) = find_order_pda(&ctx.program_id, &uid);

    // owner == created_by: the payer both owns the order and funds the rent.
    let create_order_ix = CreateOrder {
        program_id: ctx.program_id,
        owner: ctx.payer.pubkey(),
        created_by: ctx.payer.pubkey(),
        intent: &intent,
    };

    ixs.push(create_order_ix.into());

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
    let sig = ctx
        .rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let uid_hex: String = uid.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    println!("signature: {sig}");
    println!("order PDA: {order_pda}");
    println!("order UID: {uid_hex}");

    Ok(())
}

/// Returns the current unix timestamp plus `secs_from_now`, saturating at `u32::MAX`.
fn valid_to_in(secs_from_now: u64) -> u32 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    u32::try_from(now.saturating_add(secs_from_now)).unwrap_or(u32::MAX)
}
