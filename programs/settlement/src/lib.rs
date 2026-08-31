//! On-chain CoW Protocol settlement program.

use cow_settlement_interface::{recover_discriminator, SettlementInstruction};
use pinocchio::{entrypoint, AccountView, Address, ProgramResult};

mod buffer;
mod initialize;
mod order;
mod processor;
mod settle;
mod solver_auth;
mod transfer_authority;

use buffer::{process_create_buffer, process_reclaim_buffer};
use initialize::process_initialize;
use order::{process_create_order, process_reclaim_order};
use settle::{process_begin_settle, process_finalize_settle};
use solver_auth::{process_add_solver, process_remove_solver};
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
        SettlementInstruction::RemoveSolver => {
            process_remove_solver(program_id, accounts, instruction_data)
        }
    }
}
