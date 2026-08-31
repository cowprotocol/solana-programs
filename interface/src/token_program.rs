//! Utilities related to the token programs supported by the settlement program.

use crate::Pubkey;

/// The legacy SPL Token program.
pub use spl_token_2022_interface::inline_spl_token::ID as SPL_TOKEN_PROGRAM_ID;

/// The SPL Token-2022 program.
pub use spl_token_2022_interface::ID as TOKEN_2022_PROGRAM_ID;

/// Every token program a token-moving instruction accepts, in no particular
/// order.
pub const SUPPORTED_TOKEN_PROGRAMS: [Pubkey; 2] = [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID];

/// Whether `address` is a supported token program
pub fn is_supported(address: &Pubkey) -> bool {
    SUPPORTED_TOKEN_PROGRAMS.contains(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::pubkey_from_seed;

    #[test]
    fn supported_programs_are_supported() {
        for program in SUPPORTED_TOKEN_PROGRAMS {
            assert!(is_supported(&program), "{program} should be supported");
        }
    }

    #[test]
    fn unrelated_program_is_not_supported() {
        assert!(!is_supported(&pubkey_from_seed("not a token program")));
    }

    #[test]
    fn supported_programs_are_distinct() {
        assert_ne!(SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID);
    }
}
