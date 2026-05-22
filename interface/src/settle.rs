//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::SettlementInstruction;

/// Instructions sysvar account, referenced by every settlement instruction so
/// the program can introspect the surrounding transaction.
const INSTRUCTIONS_SYSVAR_ID: Pubkey =
    Pubkey::from_str_const("Sysvar1nstructions1111111111111111111111111");

pub fn begin_settle(program_id: &Pubkey, finalize_ix_index: u8) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)],
        data: vec![
            SettlementInstruction::BeginSettle.discriminator(),
            finalize_ix_index,
        ],
    }
}

pub fn finalize_settle(program_id: &Pubkey, begin_ix_index: u8) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)],
        data: vec![
            SettlementInstruction::FinalizeSettle.discriminator(),
            begin_ix_index,
        ],
    }
}

/// For both `BeginSettle` and `FinalizeSettle` instructions, recover the
/// `(discriminator, partner_index)` pair from the leading two bytes of the
/// `[discriminator, partner_index, ..]` payload. Trailing bytes are ignored.
/// Returns `InvalidInstructionData` if fewer than two bytes are provided or
/// the discriminator byte is unknown.
/// Note that the returned discriminator could be any valid discriminator for
/// the settlement program, it doesn't have to be `BeginSettle` or
/// `FinalizeSettle`.
pub fn recover_discriminator_and_partner_index(
    instruction_data: &[u8],
) -> Result<(SettlementInstruction, u8), ProgramError> {
    match instruction_data {
        [discriminator, partner_index, ..] => Ok(((*discriminator).try_into()?, *partner_index)),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_discriminator_and_partner_index(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_single_byte_payload() {
        let only_discriminator = [SettlementInstruction::BeginSettle.discriminator()];
        assert_eq!(
            recover_discriminator_and_partner_index(&only_discriminator),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_unknown_discriminator() {
        // 42 is outside the set of valid discriminators.
        assert_eq!(
            recover_discriminator_and_partner_index(&[42, 0]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn ignores_trailing_bytes() {
        assert_eq!(
            recover_discriminator_and_partner_index(&[
                SettlementInstruction::BeginSettle.discriminator(),
                42, // partner index
                67, // unused
            ]),
            Ok((SettlementInstruction::BeginSettle, 42)),
        );
    }
}
