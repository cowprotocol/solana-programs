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

use solana_address::Address;
use solana_pubkey::Pubkey;

use crate::pda::{is_pda_with_signer_seeds, SETTLEMENT_SEED};

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

/// Confirm `address` is the settlement state PDA for `program_id` and `bump`.
/// Takes the bump as given, so it costs one derivation where [`find_state_pda`]
/// searches.
#[must_use = "ignoring the output means ignoring the validation result"]
pub fn is_state_pda(program_id: &Address, address: &Address, bump: u8) -> bool {
    is_pda_with_signer_seeds(address, program_id, state_pda_signer_seeds(&[bump]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pda::tests::assert_distinct_versions_yield_distinct_pdas;

    #[test]
    fn find_state_pda_uses_canonical_seeds() {
        crate::pda::tests::assert_canonical_bump(find_state_pda, state_pda_seeds());
    }

    #[test]
    fn is_state_pda_accepts_only_the_canonical_pda_and_bump() {
        let program_id = Pubkey::new_unique();
        let (pda, bump) = find_state_pda(&program_id);

        assert!(is_state_pda(&program_id, &pda, bump));
        assert!(
            !is_state_pda(&program_id, &pda, bump ^ 1),
            "the canonical address must not validate under another bump",
        );
        assert!(
            !is_state_pda(&program_id, &Pubkey::new_unique(), bump),
            "an unrelated address must not validate as the state PDA",
        );
    }

    #[test]
    fn distinct_versions_yield_distinct_state_pdas() {
        let (pda, _) = find_state_pda(&crate::ID);

        assert_distinct_versions_yield_distinct_pdas(&pda, &[]);
    }
}
