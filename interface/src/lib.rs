//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
pub use solana_pubkey::Pubkey;

pub mod settle;

#[derive(Clone, Copy, Debug, Eq, PartialEq, num_enum::TryFromPrimitive)]
#[repr(u8)]
#[num_enum(error_type(
    name = ProgramError,
    constructor = SettlementInstruction::unknown_discriminator,
))]
pub enum SettlementInstruction {
    BeginSettle = 0,
    FinalizeSettle = 1,
}

impl SettlementInstruction {
    pub fn discriminator(self) -> u8 {
        self as u8
    }

    fn unknown_discriminator(_: u8) -> ProgramError {
        ProgramError::InvalidInstructionData
    }
}

/// For any valid settlement instruction, recover the discriminator from the
/// first byte of the payload. Returns `InvalidInstructionData` for any other
/// length or an unknown discriminator byte.
pub fn recover_discriminator(
    instruction_data: &[u8],
) -> Result<SettlementInstruction, ProgramError> {
    instruction_data
        .first()
        .copied()
        .ok_or(ProgramError::InvalidInstructionData)
        .and_then(SettlementInstruction::try_from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_discriminator(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_unknown_discriminator() {
        // 42 is outside the set of valid discriminators.
        assert_eq!(
            recover_discriminator(&[42]),
            Err(ProgramError::InvalidInstructionData),
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

    #[test]
    fn settlement_instruction_try_from_partitions_all_bytes() {
        for i in u8::MIN..=u8::MAX {
            match SettlementInstruction::try_from(i) {
                Ok(ix) => assert_eq!(ix as u8, i),
                Err(err) => assert_eq!(err, ProgramError::InvalidInstructionData),
            }
        }
    }

    #[test]
    fn settlement_instruction_try_from_matches_begin_settle() {
        assert_eq!(
            SettlementInstruction::try_from(0),
            Ok(SettlementInstruction::BeginSettle)
        );
    }
}
