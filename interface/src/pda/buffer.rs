//! Buffer PDA seed and address derivation.
//!
//! Each buffer is a per-token SPL token account that holds funds controlled
//! by the settlement state PDA. It lives at a PDA keyed by the token mint, so
//! there is exactly one buffer address per token.
//!
//! The token account stored at this address is initialized by the
//! `CreateBuffer` instruction; its SPL `owner` (token authority) is the
//! settlement state PDA (see [`crate::pda::state`]), the single authority
//! controlling every buffer.
//!
//! The seeds start with [`SETTLEMENT_SEED`], which carries
//! the cargo package major and minor version, so a version bump moves every buffer.
//! Since only the state PDA can spend a buffer and that address moves too,
//! buffers must be drained under the old version before a bump ships, or their
//! contents are stranded.
//!
//! Native SOL has a buffer of its own (see [`find_native_sol_buffer_pda`]): an
//! empty account owned by the settlement program, whose lamports on top of rent
//! pay out orders buying native SOL. It isn't keyed by a mint, so its seeds
//! follow a pattern of their own.
//!
//! Unlike the order PDA, which stores its own bump (see
//! [`crate::data::order::OrderAccount`]), a buffer's layout belongs entirely to
//! the token program, leaving no room for one.

use solana_pubkey::Pubkey;

use crate::pda::SETTLEMENT_SEED;

mod known_mints;
pub use known_mints::KNOWN_MINTS;

/// Trailing seed identifying the buffer PDAs.
pub const BUFFER_SEED: &[u8] = b"buffer";

/// Canonical seed components for the buffer PDA holding the specified `mint`
/// token.
///
/// `mint` is the raw 32-byte token mint address, so the same helper serves
/// both the off-chain builder and the on-chain handler (which holds the mint
/// as an `Address`).
pub const fn buffer_pda_seeds(mint: &[u8; 32]) -> [&[u8]; 3] {
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

/// Trailing seed identifying the native SOL buffer PDA.
pub const NATIVE_SOL_BUFFER_SEED: &[u8] = b"buffer native sol";

/// Canonical seed components for the native SOL buffer PDA.
pub const NATIVE_SOL_BUFFER_PDA_SEEDS: [&[u8]; 2] = [SETTLEMENT_SEED, NATIVE_SOL_BUFFER_SEED];

/// Derive the canonical native SOL buffer PDA address (and bump).
pub fn find_native_sol_buffer_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&NATIVE_SOL_BUFFER_PDA_SEEDS, program_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::pda::tests::assert_distinct_versions_yield_distinct_pdas;

    #[test]
    fn find_buffer_pda_uses_canonical_seeds() {
        let token = Pubkey::new_unique();

        crate::pda::tests::assert_canonical_bump(
            |program_id| find_buffer_pda(program_id, &token),
            buffer_pda_seeds(token.as_array()),
        );
    }

    #[test]
    fn distinct_versions_yield_distinct_buffer_pdas() {
        let program_id = pubkey_from_seed("program id");
        let mint = Pubkey::new_unique();
        let (pda, _) = find_buffer_pda(&program_id, &mint);

        assert_distinct_versions_yield_distinct_pdas(
            &program_id,
            &pda,
            &[mint.as_array(), BUFFER_SEED],
        );
    }

    #[test]
    fn find_native_sol_buffer_pda_uses_canonical_seeds() {
        crate::pda::tests::assert_canonical_bump(
            find_native_sol_buffer_pda,
            NATIVE_SOL_BUFFER_PDA_SEEDS,
        );
    }

    #[test]
    fn distinct_versions_yield_distinct_native_sol_buffer_pdas() {
        let program_id = pubkey_from_seed("program id");
        let (pda, _) = find_native_sol_buffer_pda(&program_id);
        assert_distinct_versions_yield_distinct_pdas(&program_id, &pda, &[NATIVE_SOL_BUFFER_SEED]);
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
