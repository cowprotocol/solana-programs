use anyhow::Context as _;
use clap::{Args as ClapArgs, Parser};
use settlement_client::{
    instructions::CreateOrder,
    settlement_interface::{
        data::intent::{EncodedOrderIntent, OrderIntent, OrderKind},
        pda::order::find_order_pda,
        Pubkey,
    },
};
use solana_sdk::{signature::Signer, transaction::Transaction};

use super::Context;
use crate::token::verify_ata_ownership;

#[derive(ClapArgs)]
struct CommonArgs {
    /// Override the resolved sell-side SPL token account (default: payer's ATA for the sell token)
    #[arg(long)]
    sell_token_account: Option<Pubkey>,

    /// Override the resolved buy-side SPL token account (default: payer's ATA for the buy token)
    #[arg(long)]
    buy_token_account: Option<Pubkey>,

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

  cow sell 1.0 SOL USDC        sell exactly 1.0 SOL, receive any USDC
  cow sell 1.0 SOL 50.0 USDC   sell exactly 1.0 SOL, receive ≥ 50.0 USDC
  cow sell 1.0 USDC            sell 1.0 SOL into USDC (SOL implied as sell token)
  cow buy 1.0 SOL 100.0 USDC   buy exactly 1.0 SOL, spend at most 100.0 USDC
  cow buy 1.0 SOL USDC         buy exactly 1.0 SOL, spending any USDC
  cow buy 1.0 USDC             buy 1.0 USDC, selling any amount of SOL (implied)

Tokens can be a builtin symbol (SOL, WSOL, USDC), a mint address, or a token-account address.")]
pub struct BuyOrSellArgs {
    /// Amount to sell (e.g. 1.0)
    amount: String,

    /// Remaining terms (tokens and/or an amount) — run with --help for supported forms
    #[arg(num_args = 1..=4)]
    terms: Vec<String>,

    #[command(flatten)]
    common: CommonArgs,
}

pub fn run_sell(ctx: Context, args: BuyOrSellArgs) -> anyhow::Result<()> {
    let BuyOrSellArgs {
        amount,
        terms,
        common,
    } = args;
    let parsed = parse(OrderKind::Sell, &amount, &terms)?;
    execute(ctx, parsed, common)
}

pub fn run_buy(ctx: Context, args: BuyOrSellArgs) -> anyhow::Result<()> {
    let BuyOrSellArgs {
        amount,
        terms,
        common,
    } = args;
    let parsed = parse(OrderKind::Buy, &amount, &terms)?;
    execute(ctx, parsed, common)
}

/// `(kind, a_tok, a_amount, b_tok, b_amount)` — amounts are `None` when unspecified.
/// If kind = OrderKind::Sell, then `a_tok` and `a_amount` is the sold token
/// If kind = OrderKind::Buy, then `b_tok` and `b_amount` is the sold token
type ParsedSyntax<'a> = (
    OrderKind,
    &'a str,
    Option<&'a str>,
    &'a str,
    Option<&'a str>,
);

fn parse<'a>(
    kind: OrderKind,
    amount: &'a str,
    terms: &'a [String],
) -> anyhow::Result<ParsedSyntax<'a>> {
    let t: Vec<&str> = terms
        .iter()
        .filter(|s| !s.eq_ignore_ascii_case("for"))
        .map(String::as_str)
        .collect();
    match t.as_slice() {
        [tok] => Ok((kind, "SOL", Some(amount), tok, None)),
        [a_tok, b_tok] => Ok((kind, a_tok, Some(amount), b_tok, None)),
        [a_tok, buy_amount, b_tok] if is_amount(buy_amount) => {
            Ok((kind, a_tok, Some(amount), b_tok, Some(buy_amount)))
        }
        _ => anyhow::bail!(
            "cannot interpret {:?}; run `cow sell --help` for usage",
            terms
        ),
    }
}

fn execute(ctx: Context, parsed: ParsedSyntax<'_>, common: CommonArgs) -> anyhow::Result<()> {
    let (kind, mut sell_tok, mut sell_amount_str, mut buy_tok, mut buy_amount_str) = parsed;

    // if buying the token, the parsing comes reversed
    if kind == OrderKind::Buy {
        (sell_tok, buy_tok) = (buy_tok, sell_tok);
        (sell_amount_str, buy_amount_str) = (buy_amount_str, sell_amount_str);
    }

    let sell_resolved = crate::token::resolve(&ctx.rpc, &ctx.payer.pubkey(), sell_tok)?;
    let buy_resolved = crate::token::resolve(&ctx.rpc, &ctx.payer.pubkey(), buy_tok)?;

    let sell_amount_str = sell_amount_str.unwrap_or("0");
    let buy_amount_str = buy_amount_str.unwrap_or("0");
    let sell_amount =
        spl_token::try_ui_amount_into_amount(sell_amount_str.to_string(), sell_resolved.decimals)
            .map_err(|_| anyhow::anyhow!("invalid sell amount: {sell_amount_str}"))?;
    let buy_amount =
        spl_token::try_ui_amount_into_amount(buy_amount_str.to_string(), buy_resolved.decimals)
            .map_err(|_| anyhow::anyhow!("invalid buy amount: {buy_amount_str}"))?;

    // If the sell token is SOL, wrap it into the payer's WSOL ATA first.
    let (sell_token_account, mut prep_ixs) = if sell_tok.eq_ignore_ascii_case("sol") {
        let (wsol_ata, wrap_ixs) = crate::instructions::wrap_sol(&ctx.payer.pubkey(), sell_amount)?;
        (wsol_ata, wrap_ixs)
    } else {
        // We are selling from an ATA--either we have to resolve it from the mint, or the user gave
        // it to us and we should validate
        let account = if let Some(explicit) = common.sell_token_account {
            verify_ata_ownership(&ctx.rpc, &explicit, &ctx.payer.pubkey())?;
            explicit
        } else {
            sell_resolved.account
        };
        (account, vec![])
    };

    let buy_token_account = common.buy_token_account.unwrap_or(buy_resolved.account);

    // Approve the settlement program to pull sell tokens on our behalf.
    prep_ixs.push(crate::instructions::approve(
        &ctx.program_id,
        &sell_token_account,
        &ctx.payer.pubkey(),
        sell_amount,
    )?);

    let intent = OrderIntent {
        owner: ctx.payer.pubkey(),
        sell_token_account,
        buy_token_account,
        sell_amount,
        buy_amount,
        valid_to: common.valid_to,
        kind,
        partially_fillable: common.partially_fillable,
        app_data: [0u8; 32],
    };

    let encoded = EncodedOrderIntent::from(&intent);
    let uid = encoded.hash();
    let (order_pda, _) = find_order_pda(&ctx.program_id, &uid);

    // owner == created_by: the payer both owns the order and funds the rent.
    let create_order_ix = CreateOrder {
        program_id: ctx.program_id,
        owner: ctx.payer.pubkey(),
        created_by: ctx.payer.pubkey(),
        intent: &intent,
    };

    // Bundle preparation and order creation into a single transaction.
    let all_ixs: Vec<_> = prep_ixs
        .into_iter()
        .chain([create_order_ix.into()])
        .collect();

    let blockhash = ctx
        .rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &all_ixs,
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

/// Returns `true` if `s` looks like a numeric amount (as opposed to a token
/// symbol/mint), so the 3-token form can be disambiguated before decimals are
/// known. Real validation happens later via `spl_token::try_ui_amount_into_amount`.
fn is_amount(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Returns the current unix timestamp plus `secs_from_now`, saturating at `u32::MAX`.
fn valid_to_in(secs_from_now: u64) -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_add(secs_from_now).min(u32::MAX as u64) as u32
}
