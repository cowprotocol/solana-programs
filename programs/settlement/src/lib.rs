//! On-chain CoW Protocol settlement program.

mod settle;

#[cfg(test)]
mod test_utils;

use pinocchio::{entrypoint, AccountView, Address, ProgramResult};
use settle::{process_begin_settle, process_finalize_settle};
use settlement_interface::{recover_discriminator, SettlementInstruction};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    match recover_discriminator(instruction_data)? {
        SettlementInstruction::BeginSettle => {
            process_begin_settle(program_id, accounts, instruction_data)
        }
        SettlementInstruction::FinalizeSettle => {
            process_finalize_settle(program_id, accounts, instruction_data)
        }
    }
}
