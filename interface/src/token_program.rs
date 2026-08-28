//! The token programs settlement transfers may be issued against.
//!
//! An instruction that moves tokens has to name the program to issue its
//! transfers against, and that program has to be one of
//! [`SUPPORTED_TOKEN_PROGRAMS`], which is what [`is_supported`] checks. How it
//! names them differs by instruction:
//!
//! - `CreateBuffer` and `ReclaimBuffer` take a single `token_program` account.
//!   Each works on one program's accounts at a time, so a mint under the other
//!   needs its own instruction.
//! - `BeginSettle` and `FinalizeSettle` take one account per supported program,
//!   described by [`TokenPrograms`], and issue each transfer against the
//!   program that owns the account it moves. One settlement can therefore mix
//!   tokens from both programs.

use crate::Pubkey;

/// The legacy SPL Token program.
///
/// Taken from the Token-2022 crate, which carries the address precisely so a
/// program that has to recognize both doesn't grow a second dependency for the
/// one it never calls directly.
pub use spl_token_2022_interface::inline_spl_token::ID as SPL_TOKEN_PROGRAM_ID;

/// The SPL Token-2022 program. Its instruction encoding is a superset of the
/// legacy program's, so the transfers this program issues are byte-identical
/// either way and only the CPI target changes.
pub use spl_token_2022_interface::ID as TOKEN_2022_PROGRAM_ID;

/// The program a [`TokenPrograms`] slot carries when the settlement moves no
/// token under that program. The system program is named by nearly every
/// settlement transaction already, so standing it in costs one more account
/// index rather than another 32-byte address.
pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

/// Every token program a token-moving instruction accepts. The order is the one
/// `BeginSettle` and `FinalizeSettle` lay their token-program accounts out in;
/// see [`TokenPrograms::addresses`].
pub const SUPPORTED_TOKEN_PROGRAMS: [Pubkey; 2] = [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID];

/// Which of [`SUPPORTED_TOKEN_PROGRAMS`] a `BeginSettle`/`FinalizeSettle` pair
/// carries.
///
/// Both instructions take one account per supported program, at fixed positions
/// and in [`SUPPORTED_TOKEN_PROGRAMS`] order, and issue each transfer against
/// the program that owns the account it moves — so a single settlement may mix
/// tokens from both. A program the settlement doesn't touch is left out by
/// putting [`SYSTEM_PROGRAM_ID`] in its slot: the transfers still need their
/// program to be named by the transaction, and the placeholder says this one
/// isn't. A token account under a left-out program has nothing to be settled
/// against and is rejected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TokenPrograms {
    /// Whether the legacy SPL Token program's slot carries the program rather
    /// than the placeholder.
    pub spl_token: bool,
    /// Whether Token-2022's slot carries the program rather than the
    /// placeholder.
    pub token_2022: bool,
}

impl TokenPrograms {
    /// The legacy SPL Token program alone.
    pub const SPL_TOKEN: Self = Self {
        spl_token: true,
        token_2022: false,
    };

    /// Token-2022 alone.
    pub const TOKEN_2022: Self = Self {
        spl_token: false,
        token_2022: true,
    };

    /// Both programs, for a settlement mixing tokens from each.
    pub const BOTH: Self = Self {
        spl_token: true,
        token_2022: true,
    };

    /// Neither program: every slot is the placeholder. Only a settlement that
    /// moves no tokens at all can be built this way.
    pub const NONE: Self = Self {
        spl_token: false,
        token_2022: false,
    };

    /// The addresses to pass, one per entry of [`SUPPORTED_TOKEN_PROGRAMS`] and
    /// in that order: the program itself where the settlement needs it, and
    /// [`SYSTEM_PROGRAM_ID`] where it doesn't.
    pub const fn addresses(self) -> [Pubkey; SUPPORTED_TOKEN_PROGRAMS.len()] {
        [
            if self.spl_token {
                SPL_TOKEN_PROGRAM_ID
            } else {
                SYSTEM_PROGRAM_ID
            },
            if self.token_2022 {
                TOKEN_2022_PROGRAM_ID
            } else {
                SYSTEM_PROGRAM_ID
            },
        ]
    }
}

/// Whether `address` is a token program settlement transfers may be issued
/// against, that is, whether it is one of [`SUPPORTED_TOKEN_PROGRAMS`].
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

    /// The placeholder has to be something no token account can be owned by,
    /// or a slot carrying it would still dispatch transfers somewhere.
    #[test]
    fn the_placeholder_is_not_a_token_program() {
        assert!(!is_supported(&SYSTEM_PROGRAM_ID));
    }

    /// Every combination puts each program in its own slot, and the placeholder
    /// wherever the settlement said it isn't needed.
    #[test]
    fn addresses_fill_each_slot_with_its_program_or_the_placeholder() {
        assert_eq!(
            TokenPrograms::BOTH.addresses(),
            [SPL_TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID],
        );
        assert_eq!(
            TokenPrograms::SPL_TOKEN.addresses(),
            [SPL_TOKEN_PROGRAM_ID, SYSTEM_PROGRAM_ID],
        );
        assert_eq!(
            TokenPrograms::TOKEN_2022.addresses(),
            [SYSTEM_PROGRAM_ID, TOKEN_2022_PROGRAM_ID],
        );
        assert_eq!(
            TokenPrograms::NONE.addresses(),
            [SYSTEM_PROGRAM_ID, SYSTEM_PROGRAM_ID],
        );
    }

    /// The slots are laid out in [`SUPPORTED_TOKEN_PROGRAMS`] order, which is
    /// what lets the on-chain side pair a slot with the program it stands for
    /// by position alone.
    #[test]
    fn addresses_follow_the_supported_program_order() {
        assert_eq!(TokenPrograms::BOTH.addresses(), SUPPORTED_TOKEN_PROGRAMS);
    }

    /// Carrying nothing is the default, so a builder that forgets its token
    /// programs settles no tokens rather than silently picking one.
    #[test]
    fn no_program_is_carried_by_default() {
        assert_eq!(TokenPrograms::default(), TokenPrograms::NONE);
    }
}
