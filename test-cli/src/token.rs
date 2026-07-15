//! Token resolution helpers: converts a user-supplied token string (alias, mint address,
//! or token-account address) into an SPL token account address and decimal count.
//!
//! Entry point: [`resolve`].

use anyhow::Context as _;
use settlement_client::settlement_interface::Pubkey;
use solana_program_pack::Pack;
use solana_pubkey::pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_token_interface::native_mint;
use spl_token_interface::state::{Account as TokenAccount, Mint};

/// Inline registry of recognised token symbols.
/// Avoids an RPC round-trip for well-known mints whose decimals are fixed.
/// Replace with a proper on-chain registry or quote-API lookup when available.
struct KnownToken {
    mint: Pubkey,
    decimals: u8,
}

const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

// Temporary registry mapping solana networks (isolated by "genesis" hash) and token symbols to mint addresess. Intended to be replaced in the
// future with something more robust.
static REGISTRY: &[(&str, &str, KnownToken)] = &[(
    DEVNET_GENESIS_HASH,
    "USDC",
    KnownToken {
        mint: pubkey!("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"),
        decimals: 6,
    },
)];

fn known_token(genesis_hash: &str, symbol: &str) -> Option<&'static KnownToken> {
    REGISTRY
        .iter()
        .find(|(g, s, _)| *g == genesis_hash && *s == symbol)
        .map(|(_, _, known)| known)
}

pub struct ResolvedToken {
    /// SPL token account to use in the order (ATA if supplied program argument was a mint).
    pub ta: Pubkey,
    /// The actual TokenAccount data
    pub ta_data: TokenAccount,
    /// Mint address for the token.
    pub mint: Pubkey,
    /// The actual mint data
    pub mint_data: Mint,
}

/// Resolve a user-supplied token string to a token account and decimal count.
pub fn resolve(rpc: &RpcClient, owner: &Pubkey, token_str: &str) -> anyhow::Result<ResolvedToken> {
    let upper = token_str.to_uppercase();

    // 1. `"SOL"` / `"WSOL"` — payer's WSOL ATA, 9 decimals, no RPC call needed.
    if matches!(upper.as_str(), "SOL" | "WSOL") {
        let wsol_mint: Pubkey = native_mint::id();
        let wsol_ata = get_associated_token_address_with_program_id(
            owner,
            &wsol_mint,
            &spl_token_interface::id(),
        );
        return Ok(ResolvedToken {
            ta: wsol_ata,
            mint: wsol_mint,
            ta_data: fetch_ta_data(rpc, &wsol_ata)?,
            mint_data: fetch_mint_data(rpc, &wsol_mint)?,
        });
    }

    // 2. Base58 mint or token-account address — fetches decimals from the mint, and possibly the token account owner.
    if let Ok(pubkey) = token_str.parse::<Pubkey>() {
        return resolve_token_from_account(rpc, owner, &pubkey);
    }

    // 3. Known symbol (e.g. `"USDC"`) — payer's ATA for the registered mint, RPC call required to get genesis hash (detecting the network).
    let genesis_hash = rpc
        .get_genesis_hash()
        .with_context(|| "failed to fetch genesis hash (is the RPC URL correct?)")?
        .to_string();
    if let Some(known) = known_token(&genesis_hash, &upper) {
        let ata = get_associated_token_address_with_program_id(
            owner,
            &known.mint,
            &spl_token_interface::id(),
        );
        return Ok(ResolvedToken {
            ta: ata,
            ta_data: fetch_ta_data(rpc, &ata)?,
            mint: known.mint,
            mint_data: fetch_mint_data(rpc, &known.mint)?,
        });
    }

    anyhow::bail!(
        "unknown token '{token_str}'; supported symbols: SOL, WSOL, USDC — \
         or provide a mint / token-account address"
    )
}

/// Resolve token information via a base58 address that may be either a token account or a mint.
/// If a token account is supplied, an additional call is required to retrieve the mint address.
/// Then, the mint account data is decoded to retrieve important token information, such as the
/// decimals.
pub fn resolve_token_from_account(
    rpc: &RpcClient,
    owner: &Pubkey,
    token_account_or_mint: &Pubkey,
) -> anyhow::Result<ResolvedToken> {
    let account = rpc
        .get_account(token_account_or_mint)
        .with_context(|| format!("account {token_account_or_mint} not found on-chain"))?;

    anyhow::ensure!(
        account.owner == spl_token_interface::id(),
        "{token_account_or_mint} is not owned by the token program (owner: {})",
        account.owner
    );

    if let Ok(token_account) = TokenAccount::unpack(&account.data) {
        Ok(ResolvedToken {
            ta: *token_account_or_mint,
            mint: token_account.mint,
            ta_data: token_account,
            mint_data: fetch_mint_data(rpc, &token_account.mint)?,
        })
    } else if let Ok(mint) = Mint::unpack(&account.data) {
        let ata = get_associated_token_address_with_program_id(owner, token_account_or_mint, &spl_token_interface::id());
        Ok(ResolvedToken {
            ta: ata,
            ta_data: fetch_ta_data(rpc, &ata)?,
            mint_data: mint,
            mint: *token_account_or_mint,
        })
    } else {
        anyhow::bail!(
            "{token_account_or_mint} could not be unpacked as a token account or mint \
             (data length: {})",
            account.data.len()
        )
    }
}

fn fetch_ta_data(rpc: &RpcClient, token_account: &Pubkey) -> anyhow::Result<TokenAccount> {
    let data = rpc
        .get_account_data(token_account);

    if let Err(_) = data {
        return Ok(TokenAccount::default());
    }

    if let Ok(ta_data) = TokenAccount::unpack(&data.unwrap()) {
        Ok(ta_data)
    } else {
        Err(anyhow::anyhow!(
            "account {token_account} is not a token account"
        ))
    }
}

fn fetch_mint_data(rpc: &RpcClient, mint: &Pubkey) -> anyhow::Result<Mint> {

    let data = rpc
        .get_account_data(mint)
        .with_context(|| format!("mint account {mint} not found"))?;

    if let Ok(mint_data) = Mint::unpack(&data) {
        Ok(mint_data)
    } else {
        Err(anyhow::anyhow!(
            "account {mint} is not a mint"
        ))
    }
}
