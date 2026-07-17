//! Buffer PDA seed and address derivation.
//!
//! Each buffer is a per-token SPL token account that holds funds on behalf
//! of the settlement program. It lives at a PDA keyed by the token mint, so
//! there is exactly one buffer address per token.
//!
//! The token account stored at this address is initialized by the
//! `CreateBuffer` instruction; its SPL `owner` (token authority) is the
//! settlement state PDA (see [`crate::pda::state`]), the single authority
//! controlling every buffer.

use solana_account_view::AccountView;
use solana_address::Address;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use spl_token_interface::state::Account as TokenAccount;

use crate::pda::SETTLEMENT_SEED;
use crate::SettlementError;

/// Trailing seed identifying the buffer PDAs.
pub const BUFFER_SEED: &[u8] = b"buffer";

/// Canonical seed components for the buffer PDA holding the specified `mint`
/// token.
///
/// `mint` is the raw 32-byte token mint address, so the same helper serves
/// both the off-chain builder and the on-chain handler (which holds the mint
/// as an `Address`).
pub fn buffer_pda_seeds(mint: &[u8; 32]) -> [&[u8]; 3] {
    [SETTLEMENT_SEED, mint, BUFFER_SEED]
}

/// Canonical seeds for re-deriving the buffer PDA for `mint` with `bump`.
pub fn buffer_pda_signer_seeds<'a>(mint: &'a [u8; 32], bump: &'a [u8; 1]) -> [&'a [u8]; 4] {
    let [s0, s1, s2] = buffer_pda_seeds(mint);
    [s0, s1, s2, bump]
}

/// Derive the canonical buffer PDA address (and bump) for the token `mint`.
pub fn find_buffer_pda(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&buffer_pda_seeds(mint.as_array()), program_id)
}

/// A settlement buffer's SPL token account, verified to sit at the canonical
/// buffer PDA for the mint it holds.
pub struct BufferTokenAccount(TokenAccount);

impl BufferTokenAccount {
    /// Unpack `buffer` as an SPL token account and confirm it sits at the
    /// canonical buffer PDA for the mint recorded in its own data, re-deriving
    /// the address from that mint and `bump`.
    ///
    /// The buffer is self-describing: it carries the mint it holds, so it
    /// proves its own identity without a separate mint input. Unpacking copies
    /// the account data out, so no borrow is held across a later transfer that
    /// writes the same account.
    pub fn load_verified_from_pda(
        program_id: &Address,
        buffer: &AccountView,
        bump: u8,
    ) -> Result<Self, SettlementError> {
        let token_account = TokenAccount::unpack(
            &buffer
                .try_borrow()
                .map_err(|_| SettlementError::PushSourceNotBuffer)?,
        )
        .map_err(|_| SettlementError::PushSourceNotBuffer)?;

        let derived = Address::create_program_address(
            &buffer_pda_signer_seeds(&token_account.mint.to_bytes(), &[bump]),
            program_id,
        )
        .map_err(|_| SettlementError::PushSourceNotBuffer)?;
        if buffer.address() != &derived {
            return Err(SettlementError::PushSourceNotBuffer);
        }

        Ok(Self(token_account))
    }

    /// The verified SPL token account backing the buffer.
    pub fn token_account(&self) -> &TokenAccount {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_buffer_pda_uses_canonical_seeds() {
        let token = Pubkey::new_unique();

        crate::pda::tests::assert_canonical_bump(
            |program_id| find_buffer_pda(program_id, &token),
            buffer_pda_seeds(token.as_array()),
        );
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn distinct_tokens_yield_distinct_pdas(
                program_id in any::<[u8; 32]>(),
                token1 in any::<[u8; 32]>(),
                token2 in any::<[u8; 32]>(),
            ) {
                prop_assume!(token1 != token2);
                let program_id = Pubkey::new_from_array(program_id);
                let (pda1, _) = find_buffer_pda(&program_id, &Pubkey::new_from_array(token1));
                let (pda2, _) = find_buffer_pda(&program_id, &Pubkey::new_from_array(token2));
                prop_assert_ne!(pda1, pda2);
            }
        }
    }
}
