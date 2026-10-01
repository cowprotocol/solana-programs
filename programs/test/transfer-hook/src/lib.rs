//! Test-only Token-2022 transfer hook program.
//!
//! Token-2022 executes it as `[source, mint, destination, authority,
//! validation_state, extra accounts...]`. It approves the transfer unless the
//! first extra account's data starts with a nonzero byte, so a test can both
//! require an extra account and observe that the hook ran.

use pinocchio::error::ProgramError;
use pinocchio::{entrypoint, AccountView, Address, ProgramResult};

entrypoint!(process_instruction);

/// Where the first extra account sits in the accounts Token-2022 passes.
const SWITCH_ACCOUNT_INDEX: usize = 5;

/// The error the hook rejects a transfer with.
pub const REJECTED: u32 = 0xb10c;

pub fn process_instruction(
    _program_id: &Address,
    accounts: &mut [AccountView],
    _instruction_data: &[u8],
) -> ProgramResult {
    let switch = accounts
        .get(SWITCH_ACCOUNT_INDEX)
        .ok_or(ProgramError::NotEnoughAccountKeys)?;
    match switch.try_borrow()?.first() {
        None | Some(0) => Ok(()),
        Some(_) => Err(ProgramError::Custom(REJECTED)),
    }
}
