//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::Instruction;
pub use solana_pubkey::Pubkey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementInstruction {
    BeginSettle,
    FinalizeSettle,
}

impl SettlementInstruction {
    pub const BEGIN_SETTLE_DISC: u8 = 0;
    pub const FINALIZE_SETTLE_DISC: u8 = 1;

    pub fn discriminator(&self) -> u8 {
        match self {
            Self::BeginSettle => Self::BEGIN_SETTLE_DISC,
            Self::FinalizeSettle => Self::FINALIZE_SETTLE_DISC,
        }
    }

    pub fn try_from_bytes(data: &[u8]) -> Option<Self> {
        match data {
            [Self::BEGIN_SETTLE_DISC] => Some(Self::BeginSettle),
            [Self::FINALIZE_SETTLE_DISC] => Some(Self::FinalizeSettle),
            _ => None,
        }
    }
}

pub fn begin_settle(program_id: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![],
        data: vec![SettlementInstruction::BEGIN_SETTLE_DISC],
    }
}

pub fn finalize_settle(program_id: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![],
        data: vec![SettlementInstruction::FINALIZE_SETTLE_DISC],
    }
}
