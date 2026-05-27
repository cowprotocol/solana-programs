//! On-chain CoW Protocol settlement program.

mod create_order;
mod processor;
mod settle;

#[cfg(test)]
mod test_utils;

use create_order::process_create_order;
use pinocchio::{entrypoint, AccountView, Address, ProgramResult};
use settle::{process_begin_settle, process_finalize_settle};
use settlement_interface::{recover_discriminator, SettlementInstruction};

entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let (discriminator, _) = recover_discriminator(instruction_data)?;
    match discriminator {
        SettlementInstruction::BeginSettle => {
            process_begin_settle(program_id, accounts, instruction_data)
        }
        SettlementInstruction::FinalizeSettle => {
            process_finalize_settle(program_id, accounts, instruction_data)
        }
        SettlementInstruction::CreateOrder => {
            process_create_order(program_id, accounts, instruction_data)
        }
    }
}
