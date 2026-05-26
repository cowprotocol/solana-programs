//! Trait for parsing instruction inputs.

use pinocchio::{error::ProgramError, AccountView};
use settlement_interface::{recover_discriminator, SettlementInstruction};

/// Shared components for parsing generic instruction input.
///
/// Implementations declare which [`SettlementInstruction`] discriminator they
/// belong to and parse the remaining instruction data and accounts. The
/// discriminator check is shared via the default [`parse`] implementation; an
/// impl only needs to provide [`parse_body`].
pub trait InstructionInputParsing: Sized {
    const DISCRIMINATOR: SettlementInstruction;

    fn parse_body(instruction_data: &[u8], accounts: &[AccountView]) -> Result<Self, ProgramError>;

    fn parse(instruction_data: &[u8], accounts: &[AccountView]) -> Result<Self, ProgramError> {
        if recover_discriminator(instruction_data)? != Self::DISCRIMINATOR {
            return Err(ProgramError::InvalidInstructionData);
        }
        Self::parse_body(instruction_data, accounts)
    }
}
