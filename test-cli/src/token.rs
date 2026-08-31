//! Token resolution helpers: converts a user-supplied token string (alias, mint address,
//! or token-account address) into an SPL token account address, the token program that
//! owns it, and the decoded mint.
//!
//! Entry point: [`resolve`].

use anyhow::Context as _;
use cow_settlement_client::cow_settlement_interface::{token_program, Pubkey};
use solana_instruction::Instruction;
use solana_pubkey::pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::account::{Account, ReadableAccount};
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_associated_token_account_interface::instruction::create_associated_token_account_idempotent;
use spl_token_2022_interface::extension::StateWithExtensions;
use spl_token_2022_interface::native_mint;
use spl_token_2022_interface::state::{Account as TokenAccount, Mint};

/// Inline registry of recognised token symbols.
/// Avoids an RPC round-trip for well-known mints whose decimals are fixed.
/// Replace with a proper on-chain registry or quote-API lookup when available.
struct KnownToken {
    mint: Pubkey,
}

const DEVNET_GENESIS_HASH: &str = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";

// Temporary registry mapping solana networks (isolated by "genesis" hash) and token symbols to mint addresess. Intended to be replaced in the
// future with something more robust.
static REGISTRY: &[(&str, &str, KnownToken)] = &[(
    DEVNET_GENESIS_HASH,
    "USDC",
    KnownToken {
        mint: pubkey!("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU"),
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
    /// Mint address for the token.
    pub mint: Pubkey,
    /// The actual mint data
    pub mint_data: Mint,
    /// The token program owning both `mint` and `ta` — one of
    /// [`token_program::SUPPORTED_TOKEN_PROGRAMS`]. Any instruction touching
    /// `ta` has to be built against it, so it travels with the resolved token
    /// rather than being assumed.
    pub token_program: Pubkey,
    /// `Some(owner)` when `ta` does not yet exist on-chain. Call with the
    /// transaction fee payer to build the instruction that creates it.
    create_ata: Option<Pubkey>,
}

impl ResolvedToken {
    /// The idempotent instruction that creates `ta` (paid for by `payer`), or
    /// `None` if the account already exists on-chain.
    pub fn create_ata_ix(&self, payer: &Pubkey) -> Option<Instruction> {
        let owner = self.create_ata?;
        Some(create_associated_token_account_idempotent(
            payer,
            &owner,
            &self.mint,
            &self.token_program,
        ))
    }
}

/// Resolve a user-supplied token string to a token account and decimal count.
pub fn resolve(rpc: &RpcClient, owner: &Pubkey, token_str: &str) -> anyhow::Result<ResolvedToken> {
    let upper = token_str.to_uppercase();

    // 1. `"SOL"` / `"WSOL"` — payer's ATA for the native mint.
    if matches!(upper.as_str(), "SOL" | "WSOL") {
        return resolve_from_mint(rpc, owner, &native_mint::ID);
    }

    // 2. Base58 mint or token-account address — fetches decimals from the mint, and possibly the token account owner.
    if let Ok(pubkey) = token_str.parse::<Pubkey>() {
        return interpret_token_from_user_input(rpc, owner, &pubkey);
    }

    // 3. Known symbol (e.g. `"USDC"`) — payer's ATA for the registered mint, RPC call required to get genesis hash (detecting the network).
    let genesis_hash = rpc
        .get_genesis_hash()
        .with_context(|| "failed to fetch genesis hash (is the RPC URL correct?)")?
        .to_string();
    if let Some(known) = known_token(&genesis_hash, &upper) {
        return resolve_from_mint(rpc, owner, &known.mint);
    }

    anyhow::bail!(
        "unknown token '{token_str}'; supported symbols: SOL, WSOL, USDC — \
         or provide a mint / token-account address"
    )
}

pub fn resolve_from_token_account(
    rpc: &RpcClient,
    token_account: &Pubkey,
) -> anyhow::Result<ResolvedToken> {
    let account = rpc.get_account(token_account).with_context(|| {
        format!(
            "token account {token_account} not found on-chain
    HELP: you can create this token account yourself:
    $ spl-token create-account $MINT --owner $OWNER
        "
        )
    })?;

    let token_program = token_program_of(&account, token_account)?;
    let decoded_account = unpack_token_account(account.data())
        .with_context(|| format!("account {token_account} is not a token account"))?;

    Ok(ResolvedToken {
        ta: *token_account,
        mint: decoded_account.mint,
        mint_data: fetch_mint(rpc, &decoded_account.mint)?.1,
        token_program,
        // The account was just fetched and unpacked above, so it already exists.
        create_ata: None,
    })
}

/// Resolve token information via a base58 address that may be either a token account or a mint.
/// If a token account is supplied, an additional call is required to retrieve the mint address.
/// Then, the mint account data is decoded to retrieve important token information, such as the
/// decimals.
pub fn interpret_token_from_user_input(
    rpc: &RpcClient,
    owner: &Pubkey,
    token_account_or_mint: &Pubkey,
) -> anyhow::Result<ResolvedToken> {
    let account = rpc
        .get_account(token_account_or_mint)
        .with_context(|| format!("account {token_account_or_mint} not found on-chain"))?;

    let token_program = token_program_of(&account, token_account_or_mint)?;

    // Token accounts are tried first: a mint carrying enough extension data to
    // reach the token account length is only told apart from an account by the
    // account-type byte, which `unpack_token_account` checks.
    if let Some(token_account) = unpack_token_account(account.data()) {
        Ok(ResolvedToken {
            ta: *token_account_or_mint,
            mint: token_account.mint,
            mint_data: fetch_mint(rpc, &token_account.mint)?.1,
            token_program,
            // The account was just fetched and unpacked above, so it already exists.
            create_ata: None,
        })
    } else if let Some(mint) = unpack_mint(account.data()) {
        let ta = get_associated_token_address_with_program_id(
            owner,
            token_account_or_mint,
            &token_program,
        );
        Ok(ResolvedToken {
            ta,
            mint_data: mint,
            mint: *token_account_or_mint,
            token_program,
            create_ata: determine_create_ata(rpc, &ta, owner)?,
        })
    } else {
        anyhow::bail!(
            "{token_account_or_mint} could not be unpacked as a token account or mint \
             (data length: {})",
            account.data.len()
        )
    }
}

/// Resolve `mint` to `owner`'s associated token account, derived under whichever
/// token program owns the mint.
fn resolve_from_mint(
    rpc: &RpcClient,
    owner: &Pubkey,
    mint: &Pubkey,
) -> anyhow::Result<ResolvedToken> {
    let (token_program, mint_data) = fetch_mint(rpc, mint)?;
    let ta = get_associated_token_address_with_program_id(owner, mint, &token_program);

    Ok(ResolvedToken {
        ta,
        mint: *mint,
        mint_data,
        token_program,
        create_ata: determine_create_ata(rpc, &ta, owner)?,
    })
}

/// The token program owning `account`, rejecting anything the settlement
/// program cannot move tokens with.
fn token_program_of(account: &Account, address: &Pubkey) -> anyhow::Result<Pubkey> {
    let owner = *account.owner();
    anyhow::ensure!(
        token_program::is_supported(&owner),
        "{address} is not owned by a supported token program (owner: {owner})",
    );
    Ok(owner)
}

/// Used to set `create_ata` on `ResolvedToken`. Returns the ATA `owner` when the
/// account still needs to be created.
fn determine_create_ata(
    rpc: &RpcClient,
    token_account_address: &Pubkey,
    owner: &Pubkey,
) -> anyhow::Result<Option<Pubkey>> {
    let Ok(data) = rpc.get_account_data(token_account_address) else {
        return Ok(Some(*owner));
    };

    anyhow::ensure!(
        unpack_token_account(&data).is_some(),
        "account {token_account_address} is not a token account"
    );
    Ok(None)
}

/// Fetch `mint` and return the token program owning it alongside its decoded state.
fn fetch_mint(rpc: &RpcClient, mint: &Pubkey) -> anyhow::Result<(Pubkey, Mint)> {
    let account = rpc
        .get_account(mint)
        .with_context(|| format!("mint account {mint} not found"))?;

    let token_program = token_program_of(&account, mint)?;
    let mint_data =
        unpack_mint(account.data()).with_context(|| format!("account {mint} is not a mint"))?;

    Ok((token_program, mint_data))
}

/// Decode the base token-account state, skipping over any Token-2022 extensions.
/// The legacy layout is the same data without the extension suffix, so this
/// covers both token programs.
fn unpack_token_account(data: &[u8]) -> Option<TokenAccount> {
    StateWithExtensions::<TokenAccount>::unpack(data)
        .ok()
        .map(|state| state.base)
}

/// Decode the base mint state, skipping over any Token-2022 extensions. See
/// [`unpack_token_account`].
fn unpack_mint(data: &[u8]) -> Option<Mint> {
    StateWithExtensions::<Mint>::unpack(data)
        .ok()
        .map(|state| state.base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_program_pack::Pack as _;
    use spl_token_2022_interface::extension::mint_close_authority::MintCloseAuthority;
    use spl_token_2022_interface::extension::{
        BaseStateWithExtensionsMut as _, ExtensionType, StateWithExtensionsMut,
    };
    use spl_token_2022_interface::state::AccountState;

    /// A mint as the legacy token program stores it: exactly `Mint::LEN` bytes.
    fn legacy_mint(decimals: u8) -> Vec<u8> {
        let mint = Mint {
            decimals,
            is_initialized: true,
            ..Default::default()
        };
        let mut data = vec![0u8; Mint::LEN];
        mint.pack_into_slice(&mut data);
        data
    }

    /// A token account as the legacy token program stores it.
    fn legacy_token_account(mint: Pubkey) -> Vec<u8> {
        let account = TokenAccount {
            mint,
            owner: Pubkey::new_unique(),
            state: AccountState::Initialized,
            ..Default::default()
        };
        let mut data = vec![0u8; TokenAccount::LEN];
        account.pack_into_slice(&mut data);
        data
    }

    /// A Token-2022 mint carrying one extension, which pads it past
    /// `TokenAccount::LEN` and appends the account-type byte.
    fn extended_mint(decimals: u8) -> Vec<u8> {
        let len =
            ExtensionType::try_calculate_account_len::<Mint>(&[ExtensionType::MintCloseAuthority])
                .expect("mint length with a close authority");
        let mut data = vec![0u8; len];

        let mut state =
            StateWithExtensionsMut::<Mint>::unpack_uninitialized(&mut data).expect("empty mint");
        state
            .init_extension::<MintCloseAuthority>(true)
            .expect("close authority extension");
        state.base = Mint {
            decimals,
            is_initialized: true,
            ..Default::default()
        };
        state.pack_base();
        state.init_account_type().expect("account type");

        data
    }

    #[test]
    fn unpacks_legacy_mint_and_token_account() {
        assert_eq!(unpack_mint(&legacy_mint(6)).expect("mint").decimals, 6);

        let mint = Pubkey::new_unique();
        assert_eq!(
            unpack_token_account(&legacy_token_account(mint))
                .expect("token account")
                .mint,
            mint,
        );
    }

    #[test]
    fn unpacks_token_2022_mint_with_extensions() {
        // `Mint::unpack` rejects this outright: it insists on exactly `Mint::LEN`.
        assert_eq!(unpack_mint(&extended_mint(2)).expect("mint").decimals, 2);
    }

    #[test]
    fn extended_mint_is_not_mistaken_for_a_token_account() {
        // It is longer than `TokenAccount::LEN`, so only the account-type byte
        // tells the two apart — which is why `interpret_token_from_user_input`
        // may try the token account first.
        let data = extended_mint(2);
        assert!(data.len() > TokenAccount::LEN);
        assert!(unpack_token_account(&data).is_none());
    }

    #[test]
    fn legacy_mint_is_not_mistaken_for_a_token_account() {
        assert!(unpack_token_account(&legacy_mint(9)).is_none());
    }

    #[test]
    fn token_program_of_accepts_every_supported_program() {
        let address = Pubkey::new_unique();
        for program in token_program::SUPPORTED_TOKEN_PROGRAMS {
            let account = Account {
                owner: program,
                ..Default::default()
            };
            assert_eq!(token_program_of(&account, &address).unwrap(), program);
        }
    }

    #[test]
    fn token_program_of_rejects_other_owners() {
        let address = Pubkey::new_unique();
        let account = Account {
            owner: Pubkey::new_unique(),
            ..Default::default()
        };
        let err = token_program_of(&account, &address)
            .expect_err("a non-token program is not a token program")
            .to_string();
        assert!(
            err.contains("not owned by a supported token program"),
            "{err}"
        );
    }
}
