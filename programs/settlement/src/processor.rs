//! Trait for parsing instruction inputs.

use pinocchio::{error::ProgramError, AccountView};
use settlement_interface::{recover_discriminator, SettlementInstruction};

/// Shared components for parsing generic instruction input.
///
/// Implementations declare which [`SettlementInstruction`] discriminator they
/// belong to and parse the remaining instruction data and accounts. The
/// discriminator check is shared via the default [`parse`] implementation; an
/// impl only needs to provide [`parse_body`].
pub trait InstructionInputParsing<'a>: Sized {
    const DISCRIMINATOR: SettlementInstruction;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError>;

    fn parse(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        match recover_discriminator(instruction_data)? {
            (discriminator, remaining_data) if discriminator == Self::DISCRIMINATOR => {
                Self::parse_body(remaining_data, accounts)
            }
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}
