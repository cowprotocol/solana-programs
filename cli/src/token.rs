//! Token resolution helpers: converts a user-supplied token string (alias, mint address,
//! or token-account address) into an SPL token account address and decimal count.
//!
//! Entry point: [`resolve`].

use anyhow::Context as _;
use settlement_client::settlement_interface::Pubkey;
use solana_program_pack::Pack;
use solana_rpc_client::rpc_client::RpcClient;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_token_interface::native_mint;
use spl_token_interface::state::{Account as TokenAccount, Mint};

// TOKEN_2022_PROGRAM_ID is stable; replace with spl_token_2022::id() once that crate is added.
const TOKEN_2022_PROGRAM_ID: &str = "TokenzQdBNbEquZMSWx5Qvq4AEJb5JMmjfLE5eTnFdyv7E";

/// Inline registry of recognised token symbols.
/// Avoids an RPC round-trip for well-known mints whose decimals are fixed.
/// Replace with a proper on-chain registry or quote-API lookup when available.
struct KnownToken {
    mint: &'static str,
    decimals: u8,
}

static REGISTRY: &[(&str, KnownToken)] = &[(
    "USDC",
    KnownToken {
        mint: "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU",
        decimals: 6,
    },
)];

pub struct ResolvedToken {
    /// SPL token account to use in the order (ATA if input was a mint).
    pub account: Pubkey,
    /// On-chain `decimals` value for the token's mint.
    pub decimals: u8,
}

/// Resolve a user-supplied token string to a token account and decimal count.
///
/// Resolution order:
/// 1. `"SOL"` / `"WSOL"` — payer's WSOL ATA, 9 decimals, no RPC call.
/// 2. Known symbol (e.g. `"USDC"`) — payer's ATA for the registered mint, no RPC call.
/// 3. Base58 mint address — derives payer's ATA; fetches decimals from the mint.
/// 4. Base58 token-account address — used directly; fetches decimals from its mint.
pub fn resolve(rpc: &RpcClient, owner: &Pubkey, token_str: &str) -> anyhow::Result<ResolvedToken> {
    let upper = token_str.to_uppercase();

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

    if let Some((_, known)) = REGISTRY.iter().find(|(sym, _)| *sym == upper.as_str()) {
        let mint: Pubkey = known.mint.parse().expect("registry mint constant");
        let ata =
            get_associated_token_address_with_program_id(owner, &mint, &spl_token_interface::id());
        return Ok(ResolvedToken {
            account: ata,
            decimals: known.decimals,
        });
    }

    if let Ok(pubkey) = token_str.parse::<Pubkey>() {
        return resolve_pubkey(rpc, owner, &pubkey);
    }

    anyhow::bail!(
        "unknown token '{token_str}'; supported symbols: SOL, WSOL, USDC — \
         or provide a mint / token-account address"
    )
}

fn resolve_pubkey(
    rpc: &RpcClient,
    owner: &Pubkey,
    pubkey: &Pubkey,
) -> anyhow::Result<ResolvedToken> {
    let account = rpc
        .get_account(pubkey)
        .with_context(|| format!("account {pubkey} not found on-chain"))?;

    // String comparison keeps us safe across any Pubkey version in the dep tree.
    let owner_str = account.owner.to_string();
    let is_token_2022 = owner_str == TOKEN_2022_PROGRAM_ID;
    anyhow::ensure!(
        owner_str == spl_token_interface::id().to_string() || is_token_2022,
        "{pubkey} is not owned by a token program (owner: {owner_str})"
    );

    let token_program: Pubkey = if is_token_2022 {
        TOKEN_2022_PROGRAM_ID.parse().expect("constant")
    } else {
        spl_token_interface::id()
    };

    // Try unpacking as a token account first, then as a mint.
    if let Ok(token_account) = TokenAccount::unpack(&account.data) {
        Ok(ResolvedToken {
            account: *pubkey,
            decimals: fetch_mint_decimals(rpc, &token_account.mint)?,
        })
    } else if let Ok(mint) = Mint::unpack(&account.data) {
        Ok(ResolvedToken {
            account: get_associated_token_address_with_program_id(owner, pubkey, &token_program),
            decimals: mint.decimals,
        })
    } else {
        anyhow::bail!(
            "{pubkey} could not be unpacked as a token account or mint \
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
