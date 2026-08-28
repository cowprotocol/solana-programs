//! The token programs a buffer may be created under.
//!
//! `CreateBuffer` and `ReclaimBuffer` take a single `token_program` account and
//! issue every one of their CPIs against it. That account has to be one of
//! [`SUPPORTED_TOKEN_PROGRAMS`], which is what [`is_supported`] checks.
//!
//! Because the program account is shared by the whole instruction, the mints an
//! instruction touches must all live under the same token program: a legacy SPL
//! mint and a Token-2022 mint can't have their buffers created by one
//! `CreateBuffer`. Splitting them across two is the caller's job.
//!
//! `BeginSettle` and `FinalizeSettle` still accept only the legacy program.

use crate::Pubkey;

/// The legacy SPL Token program.
pub use spl_token_interface::ID as SPL_TOKEN_PROGRAM_ID;

/// The SPL Token-2022 program. Its instruction encoding is a superset of the
/// legacy program's, so the instructions this program issues are byte-identical
/// either way and only the CPI target changes.
pub use spl_token_2022_interface::ID as TOKEN_2022_PROGRAM_ID;

/// Every token program a token-moving instruction accepts, in no particular
/// order.
pub const SUPPORTED_TOKEN_PROGRAMS: [Pubkey; 2] = [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID];

/// Whether `address` is a token program buffers may be created under, that is,
/// whether it is one of [`SUPPORTED_TOKEN_PROGRAMS`].
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

    /// The list is two distinct programs, so it can't have been built from one
    /// program repeated.
    #[test]
    fn supported_programs_are_distinct() {
        assert_ne!(SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID);
    }
}
