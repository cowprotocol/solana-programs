//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::Instruction;
pub use solana_pubkey::Pubkey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SettlementInstruction {
    BeginSettle = 0,
    FinalizeSettle = 1,
}

impl SettlementInstruction {
    pub fn discriminator(&self) -> u8 {
        *self as u8
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
