//! Token resolution helpers: converts a user-supplied token string (alias, mint address,
//! or token-account address) into an SPL token account address and decimal count.
//!
//! Entry point: [`resolve`].

use anyhow::Context as _;
use settlement_client::settlement_interface::Pubkey;
use solana_program_pack::Pack;
use solana_rpc_client::rpc_client::RpcClient;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_token_interface::native_mint;
use spl_token_interface::state::{Account as TokenAccount, Mint};

/// Inline registry of recognised token symbols.
/// Avoids an RPC round-trip for well-known mints whose decimals are fixed.
/// Replace with a proper on-chain registry or quote-API lookup when available.
struct KnownToken {
    mint: &'static str,
    decimals: u8,
}

const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

// Temporary registry mapping solana networks (isolated by "genesis" hash) and token symbols to mint addresess. Intended to be replaced in the
// future with something more robust.
static REGISTRY: &[(&str, &str, KnownToken)] = &[(
    DEVNET_GENESIS_HASH,
    "USDC",
    KnownToken {
        mint: "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
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
    pub account: Pubkey,
    /// On-chain `decimals` value for the token's mint.
    pub decimals: u8,
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
            account: wsol_ata,
            decimals: native_mint::DECIMALS,
        });
    }

    // 2. Base58 mint or token-account address — fetches decimals from the mint, and possibly the token account owner.
    if let Ok(pubkey) = token_str.parse::<Pubkey>() {
        return resolve_from_account(rpc, owner, &pubkey);
    }

    // 3. Known symbol (e.g. `"USDC"`) — payer's ATA for the registered mint, RPC call required to get genesis hash (detecting the network).
    let genesis_hash = rpc
        .get_genesis_hash()
        .with_context(|| "failed to fetch genesis hash (is the RPC URL correct?)")?
        .to_string();
    if let Some(known) = known_token(&genesis_hash, &upper) {
        let mint: Pubkey = known.mint.parse().expect("registry mint constant");
        let ata =
            get_associated_token_address_with_program_id(owner, &mint, &spl_token_interface::id());
        return Ok(ResolvedToken {
            account: ata,
            decimals: known.decimals,
        });
    }

    anyhow::bail!(
        "unknown token '{token_str}'; supported symbols: SOL, WSOL, USDC — \
         or provide a mint / token-account address"
    )
}

fn resolve_from_account(
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
            account: *token_account_or_mint,
            decimals: fetch_mint_decimals(rpc, &token_account.mint)?,
        })
    } else if let Ok(mint) = Mint::unpack(&account.data) {
        Ok(ResolvedToken {
            account: get_associated_token_address_with_program_id(
                owner,
                token_account_or_mint,
                &account.owner,
            ),
            decimals: mint.decimals,
        })
    } else {
        anyhow::bail!(
            "{token_account_or_mint} could not be unpacked as a token account or mint \
             (data length: {})",
            account.data.len()
        )
    }
}

fn fetch_mint_decimals(rpc: &RpcClient, mint: &Pubkey) -> anyhow::Result<u8> {
    let data = rpc
        .get_account_data(mint)
        .with_context(|| format!("mint account {mint} not found"))?;
    Ok(Mint::unpack(&data)
        .with_context(|| format!("failed to unpack mint {mint}"))?
        .decimals)
}
