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
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::pda::SETTLEMENT_SEED;
use crate::SettlementError;

/// Canonical seed components for the settlement state PDA.
pub const STATE_PDA_SEEDS: [&[u8]; 1] = [SETTLEMENT_SEED];

/// Canonical bump of the state PDA under [`crate::ID`].
/// `pinned_state_pda_is_canonical` fails with the new value when it needs updating.
pub const STATE_PDA_BUMP: u8 = 255;

/// The settlement state PDA under [`crate::ID`], derived at compile time so
/// handlers compare against it instead of searching for it on-chain.
pub const STATE_PDA: Address =
    Address::derive_address_const(&STATE_PDA_SEEDS, Some(STATE_PDA_BUMP), &crate::ID);

/// Seeds for signing as [`STATE_PDA`]: its canonical seeds followed by
/// [`STATE_PDA_BUMP`]. The on-chain settlement handlers use this to construct
/// the CPI signer.
pub const STATE_PDA_SIGNER_SEEDS: [&[u8]; 2] = [STATE_PDA_SEEDS[0], &[STATE_PDA_BUMP]];

/// Derive the canonical settlement state PDA address (and bump).
pub fn find_state_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&STATE_PDA_SEEDS, program_id)
}

/// Confirm `address` matches the settlement state PDA for `program_id` and
/// `bump`. Takes the bump as given, so it costs one derivation where
/// [`find_state_pda`] searches.
#[inline]
#[must_use = "ignoring the output means ignoring the validation result"]
pub fn validate_is_state_pda(prospective_state_address: &[u8; 32]) -> Result<(), ProgramError> {
    if prospective_state_address != STATE_PDA.as_array() {
        Err(SettlementError::PushSourceNotStatePda.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pda::tests::assert_distinct_versions_yield_distinct_pdas;

    #[test]
    fn find_state_pda_uses_canonical_seeds() {
        crate::pda::tests::assert_canonical_bump(find_state_pda, STATE_PDA_SEEDS);
    }

    #[test]
    fn pinned_state_pda_is_canonical() {
        let (pda, bump) = find_state_pda(&crate::ID);
        assert_eq!(
            (STATE_PDA, STATE_PDA_BUMP),
            (pda, bump),
            "set STATE_PDA_BUMP to {bump}",
        );
    }

    #[test]
    fn signer_seeds_sign_for_the_pinned_state_pda() {
        assert_eq!(
            Pubkey::create_program_address(&STATE_PDA_SIGNER_SEEDS, &crate::ID),
            Ok(STATE_PDA),
        );
    }

    #[test]
    fn accepts_the_state_pda() {
        validate_is_state_pda(STATE_PDA.as_array()).expect("the state PDA itself must be accepted");
    }

    #[test]
    fn rejects_any_other_address() {
        let err = validate_is_state_pda(Pubkey::new_unique().as_array())
            .expect_err("an address other than the state PDA must be rejected");
        assert_eq!(err, SettlementError::PushSourceNotStatePda.into());
    }

    #[test]
    fn distinct_versions_yield_distinct_state_pdas() {
        let (pda, _) = find_state_pda(&crate::ID);

        assert_distinct_versions_yield_distinct_pdas(&pda, &[]);
    }
}
