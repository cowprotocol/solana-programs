//! On-chain CoW Protocol settlement program.

mod add_solver;
mod create_buffer;
mod create_order;
mod initialize;
mod processor;
mod reclaim_buffer;
mod reclaim_order;
mod settle;
mod token;
mod transfer_authority;

use add_solver::process_add_solver;
use cow_settlement_interface::{recover_discriminator, SettlementInstruction};
use create_buffer::process_create_buffer;
use create_order::process_create_order;
use initialize::process_initialize;
use pinocchio::{entrypoint, AccountView, Address, ProgramResult};
use reclaim_buffer::process_reclaim_buffer;
use reclaim_order::process_reclaim_order;
use settle::{process_begin_settle, process_finalize_settle};
use transfer_authority::process_transfer_authority;

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
        SettlementInstruction::Initialize => {
            process_initialize(program_id, accounts, instruction_data)
        }
        SettlementInstruction::CreateBuffer => {
            process_create_buffer(program_id, accounts, instruction_data)
        }
        SettlementInstruction::ReclaimOrder => {
            process_reclaim_order(program_id, accounts, instruction_data)
        }
        SettlementInstruction::ReclaimBuffer => {
            process_reclaim_buffer(program_id, accounts, instruction_data)
        }
        SettlementInstruction::TransferAuthority => {
            process_transfer_authority(program_id, accounts, instruction_data)
        }
        SettlementInstruction::AddSolver => {
            process_add_solver(program_id, accounts, instruction_data)
        }
    }
}
