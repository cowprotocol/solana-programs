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

/// Writes the canonical encoding of the settlement state PDA's body into
/// `buffer`. There are no fields beyond the discriminator.
pub fn write_account(buffer: &mut [u8; SIZE]) {
    *buffer = DISCRIMINATOR;
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

    #[test]
    fn write_account_produces_decodable_bytes() {
        let mut buffer = [0u8; SIZE];
        write_account(&mut buffer);
        assert_eq!(decode(&buffer), Ok(()));
        assert_eq!(buffer, DISCRIMINATOR);
    }
}
