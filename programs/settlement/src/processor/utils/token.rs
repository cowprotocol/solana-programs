//! SPL Token program validation shared across instruction handlers.

use cow_settlement_interface::instruction::create_buffer::SPL_TOKEN_PROGRAM_ID;
use pinocchio::{error::ProgramError, AccountView, ProgramResult};

/// Validate that `token_program_account` is the legacy SPL Token program, the
/// only token program the settlement program issues CPIs against.
#[must_use = "ignoring the output may lead to an unintended on-chain state"]
pub fn validate_token_program_account(token_program_account: &AccountView) -> ProgramResult {
    if token_program_account.address() != &SPL_TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    Ok(())
}
