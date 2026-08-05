//! Settlement state PDA seed and address derivation.
//!
//! There is a single state PDA per settlement program, derived from
//! [`SETTLEMENT_SEED`] alone. It is the program's central account: it manages
//! solver authentication and holds the SPL token authority over every buffer
//! account (see [`crate::pda::buffer`]).
//!
//! Because that lone seed carries the cargo crate version, a minor
//! version bump moves the state PDA. Users delegate their token accounts to
//! this address (see `DESIGN.md`), so every delegation has to be renewed after a
//! bump.

use solana_pubkey::Pubkey;

use crate::pda::SETTLEMENT_SEED;

/// Canonical seed components for the settlement state PDA.
pub fn state_pda_seeds<'a>() -> [&'a [u8]; 1] {
    [SETTLEMENT_SEED]
}

/// Canonical seeds for signing as the settlement state PDA with `bump`. The
/// on-chain settlement handlers use this to construct the CPI signer.
pub fn state_pda_signer_seeds(bump: &[u8; 1]) -> [&[u8]; 2] {
    let [seed] = state_pda_seeds();
    [seed, bump]
}

/// Derive the canonical settlement state PDA address (and bump).
pub fn find_state_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&state_pda_seeds(), program_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_state_pda_uses_canonical_seeds() {
        crate::pda::tests::assert_canonical_bump(find_state_pda, state_pda_seeds());
    }

    #[test]
    fn distinct_versions_yield_distinct_state_pdas() {
        use crate::pda::tests::{settlement_seed_for, SAMPLE_VERSIONS};

        let program_id = Pubkey::new_unique();
        let (pda, _) = find_state_pda(&program_id);

        for other in SAMPLE_VERSIONS {
            let (other_pda, _) =
                Pubkey::find_program_address(&[&settlement_seed_for(other)], &program_id);
            assert_ne!(
                pda, other_pda,
                "version {other} must not share the current version's state PDA",
            );
        }
    }
}
