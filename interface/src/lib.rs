//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::Instruction;
use solana_program_error::ProgramError;
pub use solana_pubkey::Pubkey;

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
    pub fn discriminator(&self) -> u8 {
        *self as u8
    }

    fn unknown_discriminator(_: u8) -> ProgramError {
        ProgramError::InvalidInstructionData
    }
}

pub fn begin_settle(program_id: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![],
        data: vec![SettlementInstruction::BeginSettle.discriminator()],
    }
}

pub fn finalize_settle(program_id: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![],
        data: vec![SettlementInstruction::FinalizeSettle.discriminator()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
