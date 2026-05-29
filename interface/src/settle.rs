//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use std::vec;

use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::SettlementInstruction;

pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;

pub fn begin_settle(program_id: &Pubkey, finalize_ix_index: u16) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)],
        data: [
            &[SettlementInstruction::BeginSettle.discriminator()],
            &finalize_ix_index.to_be_bytes()[..],
        ]
        .concat(),
    }
}

pub fn finalize_settle(program_id: &Pubkey, begin_ix_index: u16) -> Instruction {
    Instruction {
        program_id: *program_id,
        accounts: vec![AccountMeta::new_readonly(INSTRUCTIONS_SYSVAR_ID, false)],
        data: [
            &[SettlementInstruction::FinalizeSettle.discriminator()],
            &begin_ix_index.to_be_bytes()[..],
        ]
        .concat(),
    }
}

/// Reads the first two bytes of a byte slice (instruction data) and
/// interprets them as a big-endian u16.
/// It's meant to be used for BeginSettle and FinalizeSettle to extract the
/// paired_index, that is, their settle pair which is encoded as the first
/// 2 bytes of the instruction data: `[0x13, 0x37]` → `0x1337`.
/// Trailing bytes are ignored, so it can be used with instruction input
/// directly.
/// Returns `InvalidInstructionData` if fewer than two bytes are provided.
pub fn recover_paired_index(instruction_data: &[u8]) -> Result<u16, ProgramError> {
    match instruction_data {
        [b1, b2, ..] => Ok(u16::from_be_bytes([*b1, *b2])),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_paired_index(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_too_short_payload() {
        assert_eq!(
            recover_paired_index(&[42]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn ignores_trailing_bytes() {
        assert_eq!(
            recover_paired_index(&[
                0x13, // paired index
                0x37, // paired index
                42,   // unused
            ]),
            Ok(0x1337),
        );
    }

    #[test]
    fn expected_encoding_begin_settle() {
        let program_id = Pubkey::new_unique();
        let ix = begin_settle(&program_id, 0x1337);
        assert_eq!(
            ix.data,
            [
                SettlementInstruction::BeginSettle.discriminator(),
                0x13,
                0x37
            ]
        );
    }

    #[test]
    fn expected_encoding_finalize_settle() {
        let program_id = Pubkey::new_unique();
        let ix = finalize_settle(&program_id, 0x1337);
        assert_eq!(
            ix.data,
            [
                SettlementInstruction::FinalizeSettle.discriminator(),
                0x13,
                0x37
            ]
        );
    }
}
