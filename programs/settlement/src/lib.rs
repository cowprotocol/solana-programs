//! On-chain CoW Protocol settlement program.

use pinocchio::{entrypoint, error::ProgramError, AccountView, Address, ProgramResult};
use settlement_interface::SettlementInstruction;

entrypoint!(process_instruction);

pub fn process_instruction(
    _program_id: &Address,
    _accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = SettlementInstruction::try_from_bytes(instruction_data)
        .ok_or(ProgramError::InvalidInstructionData)?;
    match instruction {
        SettlementInstruction::BeginSettle => process_begin_settle(),
        SettlementInstruction::FinalizeSettle => process_finalize_settle(),
    }
}

fn process_begin_settle() -> ProgramResult {
    Ok(())
}

fn process_finalize_settle() -> ProgramResult {
    Ok(())
}
