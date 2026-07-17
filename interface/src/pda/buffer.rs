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

#[allow(dead_code)]
pub struct BufferTokenAccount(TokenAccount);

impl BufferTokenAccount {
    pub fn load_verified_from_pda(
        program_id: &Address,
        buffer: &AccountView,
        bump: u8,
    ) -> Result<BufferTokenAccount, SettlementError> {
        // Read the destination's mint; the borrow ends with this block, before
        // the transfer reuses the account.
        let ta = TokenAccount::unpack_from_slice(
            &buffer
                .try_borrow()
                .map_err(|_| SettlementError::PushSourceNotBuffer)?,
        )
        .map_err(|_| SettlementError::PushSourceNotBuffer)?;

        let derived = Address::create_program_address(
            &buffer_pda_signer_seeds(&ta.mint.to_bytes(), &[bump]),
            program_id,
        )
        .map_err(|_| SettlementError::PushSourceNotBuffer)?;
        if buffer.address() != &derived {
            return Err(SettlementError::PushSourceNotBuffer.into());
        }

        Ok(BufferTokenAccount(ta))
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

    #[test]
    fn accepts_a_valid_pda() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (pda, bump) = find_buffer_pda(&program_id, &mint);

        let buffer = crate::instruction::fixtures::fake_account(pda);
        validate_buffer_pda(&program_id, &buffer, &mint, bump)
            .expect("the canonical buffer PDA must be accepted");
    }

    #[test]
    fn rejects_an_invalid_address() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (_, bump) = find_buffer_pda(&program_id, &mint);

        // An account sitting at some other address is not the buffer.
        let buffer = crate::instruction::fixtures::fake_account(Pubkey::new_unique());
        let err = validate_buffer_pda(&program_id, &buffer, &mint, bump)
            .expect_err("a non-canonical address must be rejected");
        assert_eq!(err, SettlementError::PushSourceNotBuffer.into());
    }

    #[test]
    fn rejects_a_wrong_bump() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (pda, bump) = find_buffer_pda(&program_id, &mint);

        // The address is canonical but the carried bump doesn't derive it.
        let buffer = crate::instruction::fixtures::fake_account(pda);
        let err = validate_buffer_pda(&program_id, &buffer, &mint, bump ^ 1)
            .expect_err("a wrong bump must be rejected");
        assert_eq!(err, SettlementError::PushSourceNotBuffer.into());
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
