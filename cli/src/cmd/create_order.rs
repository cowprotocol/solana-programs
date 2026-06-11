use anyhow::Context as _;
use clap::{Args as ClapArgs, Parser};
use settlement_client::{
    instructions::create_order,
    settlement_interface::{
        data::intent::{EncodedOrderIntent, OrderIntent, OrderKind},
        pda::order::find_order_pda,
        Pubkey,
    },
};
use solana_program_pack::Pack;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{signature::Signer, transaction::Transaction};
use spl_token_interface::state::Account as SplTokenAccount;

use super::Context;
use crate::token::verify_ata_ownership;

#[derive(ClapArgs)]
struct CommonArgs {
    /// Override the resolved sell-side SPL token account (default: payer's WSOL ATA for SOL)
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
Sell exactly N token for another. Supported forms:

  cow sell 1.0 SOL USDC        sell exactly 1.0 SOL, receive any USDC
  cow sell 1.0 SOL 50.0 USDC   sell exactly 1.0 SOL, receive ≥ 50.0 USDC
  cow sell 1.0 USDC            sell 1.0 SOL into USDC (SOL implied as sell token)

Tokens can be a builtin symbol (SOL, WSOL, USDC), a mint address, or a token-account address.")]
#[command(long_about = "\
Buy exactly N token using another. Supported forms:

  cow buy 1.0 SOL 100.0 USDC       buy exactly 1.0 SOL, spend at most 100.0 USDC
  cow buy 1.0 SOL USDC             buy exactly 1.0 SOL, spending any USDC
  cow buy 1.0 USDC                 buy 1.0 USDC, selling any amount of SOL (implied)

Tokens can be a builtin symbol (SOL, WSOL, USDC), a mint address, or a token-account address.")]
pub struct BuyOrSellArgs {
    /// Amount to sell (e.g. 1.0)
    amount: String,

    /// Remaining tokens — see above for all supported forms
    #[arg(num_args = 1..=4)]
    tokens: Vec<String>,

    #[command(flatten)]
    common: CommonArgs,
}
pub fn run_sell(ctx: Context, args: BuyOrSellArgs) -> anyhow::Result<()> {
    let BuyOrSellArgs {
        amount,
        tokens,
        common,
    } = args;
    let parsed = parse(OrderKind::Sell, &amount, &tokens)?;
    execute(ctx, parsed, common)
}

pub fn run_buy(ctx: Context, args: BuyOrSellArgs) -> anyhow::Result<()> {
    let BuyOrSellArgs {
        amount,
        tokens,
        common,
    } = args;
    let parsed = parse(OrderKind::Buy, &amount, &tokens)?;
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
    tokens: &'a [String],
) -> anyhow::Result<ParsedSyntax<'a>> {
    let t: Vec<&str> = tokens
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
            tokens
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

    let payer = ctx.load_payer()?;
    let rpc = ctx.rpc();

    let sell_resolved = crate::token::resolve(&rpc, &payer.pubkey(), sell_tok)?;
    let buy_resolved = crate::token::resolve(&rpc, &payer.pubkey(), buy_tok)?;

    let sell_amount = parse_amount(sell_amount_str.unwrap_or("0"), sell_resolved.decimals)?;
    let buy_amount = parse_amount(buy_amount_str.unwrap_or("0"), buy_resolved.decimals)?;

    // If the sell token is SOL, wrap it into the payer's WSOL ATA first.
    let (sell_token_account, mut prep_ixs) = if sell_tok.eq_ignore_ascii_case("sol") {
        let (wsol_ata, wrap_ixs) = crate::instructions::wrap_sol(&payer.pubkey(), sell_amount)?;
        (wsol_ata, wrap_ixs)
    } else {
        // We are selling from an ATA--either we have to resolve it from the mint, or the user gave
        // it to us and we should validate
        let account = if let Some(explicit) = common.sell_token_account {
            verify_ata_ownership(&rpc, &explicit, &payer.pubkey())?;
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
        &payer.pubkey(),
        sell_amount,
    )?);

    // Grant the settlement program close authority so it can reclaim rent after settlement.
    // Skip if the owner no longer controls close authority (i.e. already delegated).
    // (This isnt actually a feature in the settlement contract yet, but it might become)
    if owner_controls_close_authority(&rpc, &sell_token_account) {
        prep_ixs.push(crate::instructions::set_close_authority(
            &ctx.program_id,
            &sell_token_account,
            &payer.pubkey(),
        )?);
    }

    let intent = OrderIntent {
        owner: payer.pubkey(),
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
    let create_order_ix = create_order(&ctx.program_id, &payer.pubkey(), &payer.pubkey(), &intent);

    // Bundle preparation and order creation into a single transaction.
    let all_ixs: Vec<_> = prep_ixs.into_iter().chain([create_order_ix]).collect();

    let blockhash = rpc
        .get_latest_blockhash()
        .context("failed to fetch blockhash")?;
    let tx =
        Transaction::new_signed_with_payer(&all_ixs, Some(&payer.pubkey()), &[&payer], blockhash);
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .context("transaction failed")?;

    let uid_hex: String = uid.iter().map(|b| format!("{b:02x}")).collect();
    println!("signature: {sig}");
    println!("order PDA: {order_pda}");
    println!("order UID: {uid_hex}");

    Ok(())
}

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

/// Returns `true` if the token account's close authority is still controlled by its owner
/// (either unset, meaning the owner implicitly controls it, or explicitly equal to the owner).
///
/// Returns `true` for non-existent accounts (they will be created with owner control).
fn owner_controls_close_authority(rpc: &RpcClient, token_account: &Pubkey) -> bool {
    rpc.get_account(token_account)
        .ok()
        .and_then(|acc| SplTokenAccount::unpack(&acc.data).ok())
        .map(|ta| {
            // COption::None means the owner implicitly controls close authority.
            // Use bytes comparison to avoid cross-version PartialEq ambiguity.
            ta.close_authority
                .map(|ca| ca.to_bytes() == ta.owner.to_bytes())
                .unwrap_or(true)
        })
        .unwrap_or(true)
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
