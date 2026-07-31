//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use solana_program_error::ProgramError;

pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

mod begin;
mod finalize;

pub use begin::{BeginSettle, BeginSettleInput, Pull, SettledOrder};
pub use finalize::{
    finalize_push_amounts, FinalizeSettle, FinalizeSettleInput, Push, Pushes,
    FINALIZE_FIXED_ACCOUNTS,
};

/// Reads the first two bytes of a byte slice (instruction data) and
/// interprets them as a little-endian u16, returning it together with the
/// remaining bytes to parse.
/// It's meant to be used for BeginSettle and FinalizeSettle to extract the
/// counterpart index, that is, the index linking that instruction to the
/// opposite instruction which is encoded as the first
/// 2 bytes of the instruction data: `[0x37, 0x13]` → `0x1337`.
/// Returns `InvalidInstructionData` if fewer than two bytes are provided.
pub fn recover_counterpart(instruction_data: &[u8]) -> Result<(u16, &[u8]), ProgramError> {
    match instruction_data {
        [b1, b2, rest @ ..] => Ok((u16::from_le_bytes([*b1, *b2]), rest)),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::prelude::*;
    use solana_pubkey::Pubkey;

    /// Strategy producing `count` random pushes as the parallel
    /// `(source_buffers, destinations, bumps, amounts)` lists the
    /// [`FinalizeSettle`](super::FinalizeSettle) builder takes.
    pub fn arb_pushes(
        count: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = (Vec<Pubkey>, Vec<Pubkey>, Vec<u8>, Vec<u64>)> {
        prop::collection::vec(
            (
                any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
                any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
                any::<u8>(),
                any::<u64>(),
            ),
            count,
        )
        .prop_map(|pushes| {
            let source_buffers = pushes.iter().map(|&(source, ..)| source).collect();
            let destinations = pushes.iter().map(|&(_, dest, ..)| dest).collect();
            let bumps = pushes.iter().map(|&(.., bump, _)| bump).collect();
            let amounts = pushes.iter().map(|&(.., amount)| amount).collect();
            (source_buffers, destinations, bumps, amounts)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_literal::hex;

    /// Builds an instruction-data byte vector from a list of field chunks, so a
    /// test can spell out the wire layout one field per line without repeating
    /// the `&[..][..]` slicing. Each chunk is anything sliceable to `[u8]` (a
    /// byte array, a `Vec<u8>`, the result of `to_le_bytes()`, ...).
    macro_rules! ix_data {
        ($($chunk:expr),* $(,)?) => {
            [$(&$chunk[..]),*].concat()
        };
    }
    pub(crate) use ix_data;

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_counterpart(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_too_short_payload() {
        assert_eq!(
            recover_counterpart(&[42]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn returns_trailing_bytes() {
        assert_eq!(
            recover_counterpart(
                &[
                    &hex!("3713")[..], // counterpart index, little-endian
                    &[42][..],         // trailing
                ]
                .concat()
            ),
            Ok((0x1337, [42].as_slice())),
        );
    }
}
