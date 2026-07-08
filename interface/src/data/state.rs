//! Settlement state PDA body.

use solana_program_error::ProgramError;

use crate::SettlementAccount;

/// Canonical size of the settlement state PDA's body: just the discriminator.
pub const SIZE: usize = 1;

/// Single-byte account discriminator. See [`crate::SettlementAccount`].
pub const DISCRIMINATOR: [u8; SIZE] = [SettlementAccount::SettlementState.discriminator()];

/// Validate that `bytes` carries the expected discriminator.
pub fn decode(bytes: &[u8; SIZE]) -> Result<(), ProgramError> {
    if *bytes != DISCRIMINATOR {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_accepts_discriminator() {
        assert_eq!(decode(&DISCRIMINATOR), Ok(()));
    }

    #[test]
    fn decode_rejects_wrong_discriminator() {
        let mut bytes = DISCRIMINATOR;
        bytes[0] ^= 0xff;
        assert_eq!(decode(&bytes), Err(ProgramError::InvalidAccountData));
    }
}
