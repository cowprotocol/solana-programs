//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;

use crate::SettlementInstruction;

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
/// `partner_index` byte from the payload, assuming the discriminator was
/// already stripped.
/// Trailing bytes are ignored, so it can be used with instruction input
/// directly. The leading discriminator byte is *not* validated here.
/// Returns `InvalidInstructionData` if fewer than two bytes are provided.
pub fn recover_partner_index(instruction_data: &[u8]) -> Result<u8, ProgramError> {
    match instruction_data {
        [partner_index, ..] => Ok(*partner_index),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_partner_index(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn ignores_trailing_bytes() {
        assert_eq!(
            recover_partner_index(&[
                42, // partner index
                67, // unused
            ]),
            Ok(42),
        );
    }
}
