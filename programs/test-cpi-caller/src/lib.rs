//! Test-only CPI forwarder program.
//!
//! Forwards its instruction data and accounts to whatever program is passed as
//! `accounts[0]`.  Useful for testing that programs reject CPI invocations:
//! build a transaction that calls this program, and any instruction data /
//! accounts the inner program needs appear starting at `accounts[1]`.

use core::{mem::MaybeUninit, slice::from_raw_parts};
use pinocchio::{
    cpi::{invoke_signed_unchecked, CpiAccount},
    entrypoint,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};
use pinocchio::error::ProgramError;

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

    let n = forwarded.len().min(MAX_FORWARDED_ACCOUNTS);

    let mut ix_accounts = [const { MaybeUninit::<InstructionAccount>::uninit() }; MAX_FORWARDED_ACCOUNTS];
    let mut cpi_accounts = [const { MaybeUninit::<CpiAccount>::uninit() }; MAX_FORWARDED_ACCOUNTS];

    for (i, account) in forwarded[..n].iter().enumerate() {
        ix_accounts[i].write(InstructionAccount::from(account));
        CpiAccount::init_from_account_view(account, &mut cpi_accounts[i]);
    }

    let instruction = InstructionView {
        program_id: program.address(),
        // SAFETY: ix_accounts[..n] was fully initialized in the loop above.
        accounts: unsafe { from_raw_parts(ix_accounts.as_ptr() as _, n) },
        data: instruction_data,
    };

    // SAFETY: cpi_accounts[..n] was fully initialized in the loop above.
    unsafe {
        invoke_signed_unchecked(
            &instruction,
            from_raw_parts(cpi_accounts.as_ptr() as _, n),
            &[],
        )
    };

    Ok(())
}
