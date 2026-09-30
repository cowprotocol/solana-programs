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

use crate::{
    pda::{buffer::BUFFER_SEED, SETTLEMENT_SEED},
    SettlementError,
};

/// Canonical seed components for the settlement state PDA.
pub const STATE_PDA_SEEDS: [&[u8]; 1] = [SETTLEMENT_SEED];

/// Canonical seed components for the native SOL buffer PDA: the
/// [`super::buffer::buffer_pda_seeds`] of the native SOL marker mint, so
/// `CreateBuffer` creates it like any other buffer.
pub const NATIVE_SOL_BUFFER_PDA_SEEDS: [&[u8]; 3] = [
    SETTLEMENT_SEED,
    solana_system_interface::program::ID.as_array(),
    BUFFER_SEED,
];

pub const STATE_PDA_AND_BUMP: ([u8; 32], u8) =
    const_crypto::ed25519::derive_program_address(&STATE_PDA_SEEDS, crate::ID.as_array());

pub const NATIVE_SOL_BUFFER_PDA_AND_BUMP: ([u8; 32], u8) =
    const_crypto::ed25519::derive_program_address(
        &NATIVE_SOL_BUFFER_PDA_SEEDS,
        crate::ID.as_array(),
    );

/// The settlement state PDA under [`crate::ID`], derived at compile time so
/// handlers compare against it instead of searching for it on-chain.
pub const STATE_PDA: Address = Address::new_from_array(STATE_PDA_AND_BUMP.0);

/// The buffer holding the lamports paid out to orders buying native SOL,
/// derived at compile time like [`STATE_PDA`].
pub const NATIVE_SOL_BUFFER_PDA: Address =
    Address::new_from_array(NATIVE_SOL_BUFFER_PDA_AND_BUMP.0);

/// Seeds for signing as [`STATE_PDA`].
pub const STATE_PDA_SIGNER_SEEDS: [&[u8]; 2] = {
    let [s0] = STATE_PDA_SEEDS;
    [s0, &[STATE_PDA_AND_BUMP.1]]
};

/// Confirm `prospective_state_address` matches the settlement state PDA constant
/// encoded in the program bytecode.
#[inline]
#[must_use = "ignoring the output means ignoring the validation result"]
pub fn validate_is_state_pda(prospective_state_address: &[u8; 32]) -> Result<(), ProgramError> {
    if prospective_state_address != &STATE_PDA_AND_BUMP.0 {
        Err(SettlementError::StateAccountMismatch.into())
    } else {
        Ok(())
    }
}

/// Confirm `prospective_buffer_address` matches the native SOL buffer PDA
/// constant encoded in the program bytecode.
#[inline]
#[must_use = "ignoring the output means ignoring the validation result"]
pub fn validate_is_native_sol_buffer_pda(
    prospective_buffer_address: &[u8; 32],
) -> Result<(), ProgramError> {
    if prospective_buffer_address != &NATIVE_SOL_BUFFER_PDA_AND_BUMP.0 {
        Err(SettlementError::PushSourceNotBuffer.into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pda::tests::assert_distinct_versions_yield_distinct_pdas;
    use solana_pubkey::Pubkey;

    #[test]
    fn pinned_state_pda_is_canonical() {
        let (pda, bump) = Pubkey::find_program_address(&STATE_PDA_SEEDS, &crate::ID);
        assert_eq!(
            STATE_PDA_AND_BUMP,
            (*pda.as_array(), bump),
            "const-crypto's compile-time derivation disagrees with the runtime canonical PDA (bump {bump})",
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
        assert_eq!(err, SettlementError::StateAccountMismatch.into());
    }

    #[test]
    fn pinned_native_sol_buffer_pda_is_the_native_marker_buffer() {
        let marker = solana_system_interface::program::ID;
        assert_eq!(NATIVE_SOL_BUFFER_PDA_AND_BUMP, {
            let (pda, bump) = crate::pda::buffer::find_buffer_pda(&crate::ID, &marker);
            (*pda.as_array(), bump)
        },);
        assert_ne!(NATIVE_SOL_BUFFER_PDA, STATE_PDA);
    }

    #[test]
    fn accepts_the_native_sol_buffer_pda() {
        validate_is_native_sol_buffer_pda(NATIVE_SOL_BUFFER_PDA.as_array())
            .expect("the native SOL buffer PDA itself must be accepted");
    }

    #[test]
    fn rejects_the_state_pda_as_the_native_sol_buffer() {
        let err = validate_is_native_sol_buffer_pda(STATE_PDA.as_array())
            .expect_err("the state PDA is no longer the native SOL buffer");
        assert_eq!(err, SettlementError::PushSourceNotBuffer.into());
    }

    #[test]
    fn distinct_versions_yield_distinct_state_pdas() {
        assert_distinct_versions_yield_distinct_pdas(&STATE_PDA, &[]);
    }
}
