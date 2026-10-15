//! Settlement state PDA seed and address derivation.
//!
//! There is a single state PDA per settlement program, derived from
//! [`SETTLEMENT_SEED`] alone. It is the program's central account: it manages
//! solver authentication and holds the SPL token authority over every buffer
//! account (see [`crate::pda::buffer`]).
//!
//! Because that lone seed carries the cargo crate version, a minor
//! version bump moves the state PDA. Users delegate their token accounts to
//! this address, so every delegation has to be renewed after a
//! bump.

use solana_pubkey::Pubkey;

use crate::pda::SETTLEMENT_SEED;

/// Canonical seed components for the settlement state PDA.
pub const STATE_PDA_SEEDS: [&[u8]; 1] = [SETTLEMENT_SEED];

/// Derive the settlement state PDA address (and bump) under `program_id`.
pub fn find_state_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&STATE_PDA_SEEDS, program_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::pda::tests::{assert_canonical_bump, assert_distinct_versions_yield_distinct_pdas};

    #[test]
    fn find_state_pda_uses_canonical_seeds() {
        assert_canonical_bump(find_state_pda, STATE_PDA_SEEDS);
    }

    #[test]
    fn distinct_versions_yield_distinct_state_pdas() {
        let program_id = pubkey_from_seed("program id");
        let (pda, _) = find_state_pda(&program_id);
        assert_distinct_versions_yield_distinct_pdas(&program_id, &pda, &[]);
    }
}
