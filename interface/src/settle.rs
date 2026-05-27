//! `BeginSettle`/`FinalizeSettle` off-chain instruction builders.

use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::SettlementInstruction;

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
