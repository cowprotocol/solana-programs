//! Test-only CPI forwarder program.
//!
//! Forwards its instruction data and accounts to whatever program is passed as
//! `accounts[0]`.  Useful for testing that programs reject CPI invocations:
//! build a transaction that calls this program, and any instruction data /
//! accounts the inner program needs appear starting at `accounts[1]`.

use core::{mem::MaybeUninit, slice::from_raw_parts};
use pinocchio::error::ProgramError;
use pinocchio::{
    cpi::{invoke_signed_unchecked, CpiAccount},
    entrypoint,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

entrypoint!(process_instruction);

const MAX_FORWARDED_ACCOUNTS: usize = 16;

pub fn process_instruction(
    _program_id: &Address,
    accounts: &mut [AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let (program, forwarded) = accounts
        .split_first()
        .ok_or(ProgramError::NotEnoughAccountKeys)?;

    let ix_accounts: Vec<InstructionAccount> =
        forwarded.iter().map(InstructionAccount::from).collect();

    let instruction = InstructionView {
        program_id: program.address(),
        accounts: ix_accounts.as_slice(),
        data: instruction_data,
    };

    invoke_signed_with_slice(&instruction, forwarded, &[])
}
