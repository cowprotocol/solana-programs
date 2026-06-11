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
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token_interface::state::Account as SplTokenAccount;

use super::Context;

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
#[command(
    long_about = "\
Sell a token for another. Supported forms:

  cow sell 1.0 SOL for USDC        sell exactly 1.0 SOL, receive any USDC
  cow sell 1.0 SOL for 50.0 USDC   sell exactly 1.0 SOL, receive ≥ 50.0 USDC
  cow sell 1.0 USDC                sell 1.0 SOL into USDC (SOL implied as sell token)

Tokens can be a builtin symbol (SOL, WSOL, USDC), a mint address, or a token-account address."
)]
pub struct SellArgs {
    /// Amount to sell (e.g. 1.0)
    amount: String,

    /// Remaining tokens — see above for all supported forms
    #[arg(num_args = 1..=4)]
    tokens: Vec<String>,

    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Parser)]
#[command(
    long_about = "\
Buy a token using another. Supported forms:

  cow buy 1.0 SOL with USDC        buy exactly 1.0 SOL, spending any USDC
  cow buy 1.0 USDC                 buy 1.0 USDC, selling SOL (implied)

Tokens can be a builtin symbol (SOL, WSOL, USDC), a mint address, or a token-account address."
)]
pub struct BuyArgs {
    /// Amount to buy (e.g. 1.0)
    amount: String,

    /// Remaining tokens — see above for all supported forms
    #[arg(num_args = 1..=3)]
    tokens: Vec<String>,

    #[command(flatten)]
    common: CommonArgs,
}

pub fn run_sell(ctx: Context, args: SellArgs) -> anyhow::Result<()> {
    let SellArgs { amount, tokens, common } = args;
    let parsed = parse_sell(&amount, &tokens)?;
    execute(ctx, parsed, common)
}

pub fn run_buy(ctx: Context, args: BuyArgs) -> anyhow::Result<()> {
    let BuyArgs { amount, tokens, common } = args;
    let parsed = parse_buy(&amount, &tokens)?;
    execute(ctx, parsed, common)
}

/// `(kind, sell_tok, sell_amount, buy_tok, buy_amount)` — amounts are `None` when unspecified.
type ParsedSyntax<'a> = (
    OrderKind,
    &'a str,
    Option<&'a str>,
    &'a str,
    Option<&'a str>,
);

/// Parse sell syntax, stripping the optional `for` keyword.
///
///   sell 1.0 USDC                → sell 1.0 SOL (implied), get USDC
///   sell 1.0 SOL [for] USDC      → sell 1.0 SOL, get USDC
///   sell 1.0 SOL [for] 50.0 USDC → sell 1.0 SOL, get ≥ 50.0 USDC
fn parse_sell<'a>(amount: &'a str, tokens: &'a [String]) -> anyhow::Result<ParsedSyntax<'a>> {
    let t: Vec<&str> = tokens
        .iter()
        .filter(|s| !s.eq_ignore_ascii_case("for"))
        .map(String::as_str)
        .collect();
    match t.as_slice() {
        // sell 1.0 USDC — sell 1.0 SOL into USDC (SOL implied)
        [buy_tok] => Ok((OrderKind::Sell, "SOL", Some(amount), buy_tok, None)),
        // sell 1.0 SOL [for] USDC — sell 1.0 SOL, get USDC
        [sell_tok, buy_tok] => Ok((OrderKind::Sell, sell_tok, Some(amount), buy_tok, None)),
        // sell 1.0 SOL [for] 50.0 USDC — sell with minimum buy amount
        [sell_tok, buy_amount, buy_tok] if is_amount(buy_amount) => Ok((
            OrderKind::Sell,
            sell_tok,
            Some(amount),
            buy_tok,
            Some(buy_amount),
        )),
        _ => anyhow::bail!(
            "cannot interpret {:?}; run `cow sell --help` for usage",
            tokens
        ),
    }
}

/// Parse buy syntax, stripping the optional `with` keyword.
///
///   buy 1.0 USDC            → buy 1.0 USDC, sell SOL (implied)
///   buy 1.0 SOL [with] USDC → buy 1.0 SOL, sell USDC
fn parse_buy<'a>(amount: &'a str, tokens: &'a [String]) -> anyhow::Result<ParsedSyntax<'a>> {
    let t: Vec<&str> = tokens
        .iter()
        .filter(|s| !s.eq_ignore_ascii_case("with"))
        .map(String::as_str)
        .collect();
    match t.as_slice() {
        // buy 1.0 USDC — buy 1.0 USDC, sell SOL implied
        [buy_tok] => Ok((OrderKind::Buy, "SOL", None, buy_tok, Some(amount))),
        // buy 1.0 SOL [with] USDC — buy 1.0 SOL, sell USDC
        [buy_tok, sell_tok] => Ok((OrderKind::Buy, sell_tok, None, buy_tok, Some(amount))),
        _ => anyhow::bail!(
            "cannot interpret {:?}; run `cow buy --help` for usage",
            tokens
        ),
    }
}

fn execute(ctx: Context, parsed: ParsedSyntax<'_>, common: CommonArgs) -> anyhow::Result<()> {
    let (kind, sell_tok, sell_amount_str, buy_tok, buy_amount_str) = parsed;

    let payer = ctx.load_payer()?;
    let rpc = ctx.rpc();

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

/// Errors if `token_account` is an ATA whose owner is not `expected_owner`.
///
/// Non-ATA accounts and accounts that don't exist on-chain are silently skipped — the
/// on-chain program will reject a misowned account at settlement time anyway.
fn verify_ata_ownership(
    rpc: &RpcClient,
    token_account: &Pubkey,
    expected_owner: &Pubkey,
) -> anyhow::Result<()> {
    let Some(raw) = rpc.get_account(token_account).ok() else {
        return Ok(());
    };
    let Ok(ta) = SplTokenAccount::unpack(&raw.data) else {
        return Ok(());
    };
    let expected_ata = get_associated_token_address_with_program_id(
        &ta.owner,
        &ta.mint,
        &spl_token_interface::id(),
    );
    if expected_ata == *token_account && ta.owner.to_bytes() != expected_owner.to_bytes() {
        anyhow::bail!(
            "sell_token_account {token_account} is an ATA belonging to {}, not the signer {}",
            ta.owner,
            expected_owner,
        );
    }
    Ok(())
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
