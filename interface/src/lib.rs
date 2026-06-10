//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
pub use solana_pubkey::Pubkey;

solana_pubkey::declare_id!("MooohhPEAAHwAwEozL7JPEmnDvaahuUpccYN4Yb8ccK");

pub mod data;
pub mod instruction;
pub mod pda;

#[derive(Clone, Copy, Debug, Eq, PartialEq, num_enum::TryFromPrimitive)]
#[repr(u8)]
#[num_enum(error_type(
    name = ProgramError,
    constructor = SettlementInstruction::unknown_discriminator,
))]
pub enum SettlementInstruction {
    BeginSettle = 0,
    FinalizeSettle = 1,
    CreateOrder = 2,
    Initialize = 3,
}

impl SettlementInstruction {
    pub fn discriminator(self) -> u8 {
        self as u8
    }

    fn unknown_discriminator(_: u8) -> ProgramError {
        ProgramError::InvalidInstructionData
    }
}

/// Recover the discriminator from the first byte of the payload and the
/// remaining bytes to parse.
/// Returns `InvalidInstructionData` for an insufficient length or an
/// unknown discriminator.
pub fn recover_discriminator(
    instruction_data: &[u8],
) -> Result<(SettlementInstruction, &[u8]), ProgramError> {
    let discriminator = instruction_data
        .first()
        .copied()
        .ok_or(ProgramError::InvalidInstructionData)
        .and_then(SettlementInstruction::try_from)?;
    Ok((discriminator, &instruction_data[1..]))
}

/// Program-side errors surfaced by the settlement program.
/// The discriminant value is the on-chain `ProgramError::Custom` code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SettlementError {
    /// `BeginSettle`/`FinalizeSettle` did not form a valid pair:
    /// the counterpart index on one of the two instructions points at the wrong
    /// instruction kind, the pointed data doesn't match, the ordering is
    /// reversed, or another settlement pair appears nested inside this one.
    FinalizeBeforeInitialize = 0,
    BeginFinalizePairOverlap = 1,
    MissingCounterpartInstruction = 2,
    CounterpartIsExternal = 3,
    InvalidCounterpartDiscriminator = 4,
    InvalidCounterpartCounterpart = 5,
    MismatchedCounterpartDiscriminator = 6,
    /// `CreateOrder` instruction wasn't signed by the created `OrderIntent`
    /// owner.
    OwnerMismatch = 7,
}

impl From<SettlementError> for u32 {
    fn from(e: SettlementError) -> Self {
        e as u32
    }
}

impl From<SettlementError> for solana_program_error::ProgramError {
    fn from(e: SettlementError) -> Self {
        Self::Custom(e.into())
    }
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
    fn forwards_trailing_bytes() {
        assert!(matches!(
            recover_discriminator(&[
                SettlementInstruction::BeginSettle.discriminator(),
                42 // unused
            ]),
            Ok((SettlementInstruction::BeginSettle, [42])),
        ));
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
