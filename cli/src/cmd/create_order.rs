use anyhow::Context as _;
use clap::Parser;
use settlement_client::{
    instructions::create_order,
    settlement_interface::{
        data::intent::{EncodedOrderIntent, OrderIntent, OrderKind},
        pda::order::find_order_pda,
        Pubkey,
    },
};
use solana_sdk::{signature::Signer, transaction::Transaction};

use super::Context;

#[derive(Parser)]
#[command(
    visible_alias = "swap",
    long_about = "\
Create an on-chain order PDA. Supported positional forms:

  cow create-order 1.0 SOL USDC        sell exactly 1.0 SOL, receive any USDC
  cow create-order SOL 1.0 USDC        buy  exactly 1.0 USDC, spend any SOL
  cow create-order 1.0 USDC            buy  exactly 1.0 USDC (SOL implied as sell)
  cow create-order 1.0 SOL 50.0 USDC   sell exactly 1.0 SOL, receive ≥ 50.0 USDC

Tokens can be 'SOL'/'WSOL', a mint address, or a token-account address.
Decimals are fetched from the token's on-chain mint account.
The unspecified side defaults to 0 (no price protection) until a quote API is integrated.
Token accounts are derived from the mint automatically; override with the flags below."
)]
pub struct Args {
    /// 2–4 positional tokens describing the swap (see above)
    #[arg(num_args = 2..=4)]
    pub tokens: Vec<String>,

    /// Override the resolved sell-side SPL token account (default: payer's WSOL ATA for SOL)
    #[arg(long)]
    pub sell_token_account: Option<Pubkey>,

    /// Override the resolved buy-side SPL token account (default: payer's ATA for the buy token)
    #[arg(long)]
    pub buy_token_account: Option<Pubkey>,

    /// Unix timestamp after which the order expires (defaults to 5 minutes from now)
    #[arg(long, default_value_t = valid_to_in(300))]
    pub valid_to: u32,

    /// Allow partial fills across multiple settlements
    #[arg(long)]
    pub partially_fillable: bool,
}

pub fn run(ctx: Context, args: Args) -> anyhow::Result<()> {
    let payer = ctx.load_payer()?;
    let rpc = ctx.rpc();

    let (kind, sell_tok, sell_amount_str, buy_tok, buy_amount_str) =
        parse_syntax(&args.tokens)?;

    let sell_resolved = crate::token::resolve(&rpc, &payer.pubkey(), sell_tok)?;
    let buy_resolved = crate::token::resolve(&rpc, &payer.pubkey(), buy_tok)?;

    let sell_amount = sell_amount_str
        .map(|s| parse_amount(s, sell_resolved.decimals))
        .transpose()?
        .unwrap_or(0);
    let buy_amount = buy_amount_str
        .map(|s| parse_amount(s, buy_resolved.decimals))
        .transpose()?
        .unwrap_or(0);

    // If the sell token is SOL, wrap it into the payer's WSOL ATA first.
    let (sell_token_account, mut prep_ixs) = if sell_tok.eq_ignore_ascii_case("sol") {
        let (wsol_ata, wrap_ixs) = crate::instructions::wrap_sol(&payer.pubkey(), sell_amount)?;
        (wsol_ata, wrap_ixs)
    } else {
        (args.sell_token_account.unwrap_or(sell_resolved.account), vec![])
    };

    let buy_token_account = args.buy_token_account.unwrap_or(buy_resolved.account);

    // Approve the settlement program to pull sell tokens on our behalf.
    prep_ixs.push(crate::instructions::approve(
        &ctx.program_id,
        &sell_token_account,
        &payer.pubkey(),
        sell_amount,
    )?);

    // Grant the settlement program close authority so it can reclaim rent after settlement.
    prep_ixs.push(crate::instructions::set_close_authority(
        &ctx.program_id,
        &sell_token_account,
        &payer.pubkey(),
    )?);

    let intent = OrderIntent {
        owner: payer.pubkey(),
        sell_token_account,
        buy_token_account,
        sell_amount,
        buy_amount,
        valid_to: args.valid_to,
        kind,
        partially_fillable: args.partially_fillable,
        app_data: [0u8; 32],
    };

    let encoded = EncodedOrderIntent::from(&intent);
    let uid = encoded.hash();
    let (order_pda, _) = find_order_pda(&ctx.program_id, &uid);

    // owner == created_by: the payer both owns the order and funds the rent.
    let create_order_ix =
        create_order(&ctx.program_id, &payer.pubkey(), &payer.pubkey(), &intent);

    // Bundle preparation and order creation into a single transaction.
    let all_ixs: Vec<_> = prep_ixs.into_iter().chain([create_order_ix]).collect();

    let blockhash = rpc.get_latest_blockhash().context("failed to fetch blockhash")?;
    let tx = Transaction::new_signed_with_payer(
        &all_ixs,
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let uid_hex: String = uid.iter().map(|b| format!("{b:02x}")).collect();
    println!("signature: {sig}");
    println!("order PDA: {order_pda}");
    println!("order UID: {uid_hex}");

    Ok(())
}

/// Parse 2–4 positional tokens into `(kind, sell_tok, sell_amount, buy_tok, buy_amount)`.
///
/// Returns `Option<&str>` for the amounts: `None` means unspecified (caller uses 0).
fn parse_syntax(tokens: &[String]) -> anyhow::Result<(OrderKind, &str, Option<&str>, &str, Option<&str>)> {
    match tokens {
        // cow swap 1.0 USDC — buy 1.0 USDC (SOL implied as sell)
        [amount, buy_tok] if is_amount(amount) => {
            Ok((OrderKind::Buy, "SOL", None, buy_tok, Some(amount)))
        }
        // cow swap 1.0 SOL USDC — sell exactly 1.0 SOL
        [amount, sell_tok, buy_tok] if is_amount(amount) => {
            Ok((OrderKind::Sell, sell_tok, Some(amount), buy_tok, None))
        }
        // cow swap SOL 1.0 USDC — buy exactly 1.0 USDC
        [sell_tok, amount, buy_tok] if is_amount(amount) => {
            Ok((OrderKind::Buy, sell_tok, None, buy_tok, Some(amount)))
        }
        // cow swap 1.0 SOL 50.0 USDC — sell 1.0 SOL, receive ≥ 50.0 USDC
        [sell_amount, sell_tok, buy_amount, buy_tok]
            if is_amount(sell_amount) && is_amount(buy_amount) =>
        {
            Ok((OrderKind::Sell, sell_tok, Some(sell_amount), buy_tok, Some(buy_amount)))
        }
        _ => anyhow::bail!(
            "cannot interpret {:?}; run `cow create-order --help` for usage",
            tokens
        ),
    }
}

/// Returns `true` if `s` looks like a decimal number rather than a token identifier.
fn is_amount(s: &str) -> bool {
    s.parse::<f64>().is_ok()
}

/// Convert a human-readable amount string to the token's smallest unit using
/// `decimals` fetched from the on-chain mint.
fn parse_amount(amount_str: &str, decimals: u8) -> anyhow::Result<u64> {
    let amount: f64 = amount_str
        .parse()
        .with_context(|| format!("invalid amount: {amount_str}"))?;
    let multiplier = 10u64.pow(u32::from(decimals));
    Ok((amount * multiplier as f64).round() as u64)
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
