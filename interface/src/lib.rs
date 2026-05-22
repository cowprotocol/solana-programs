//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::{AccountMeta, Instruction};
pub use solana_pubkey::Pubkey;

pub mod settle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SettlementInstruction {
    BeginSettle = 0,
    FinalizeSettle = 1,
}

impl SettlementInstruction {
    pub fn discriminator(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for SettlementInstruction {
    type Error = solana_program_error::ProgramError;

    fn try_from(b: u8) -> Result<Self, Self::Error> {
        match b {
            0 => Ok(Self::BeginSettle),
            1 => Ok(Self::FinalizeSettle),
            _ => Err(Self::Error::InvalidInstructionData),
        }
    }
}

/// For any valid settlement instruction, recover the discriminator from the
/// first byte of the payload. Returns `InvalidInstructionData` for any other
/// length or an unknown discriminator byte.
pub fn recover_discriminator(
    instruction_data: &[u8],
) -> Result<SettlementInstruction, solana_program_error::ProgramError> {
    instruction_data
        .first()
        .copied()
        .ok_or(solana_program_error::ProgramError::InvalidInstructionData)
        .and_then(SettlementInstruction::try_from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_discriminator(&[]),
            Err(solana_program_error::ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_unknown_discriminator() {
        // 42 is outside the set of valid discriminators.
        assert_eq!(
            recover_discriminator(&[42]),
            Err(solana_program_error::ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn ignores_trailing_bytes() {
        assert_eq!(
            recover_discriminator(&[
                SettlementInstruction::BeginSettle.discriminator(),
                42 // unused
            ]),
            Ok(SettlementInstruction::BeginSettle),
        );
    }
}
