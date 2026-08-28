//! The token programs settlement transfers may be issued against.
//!
//! An instruction that moves tokens has to name the program to issue its
//! transfers against, and that program has to be one of [`TokenProgram::ALL`],
//! which is what [`TokenProgram::try_from`] resolves an address against. How it
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
use solana_program_error::ProgramError;

/// The program a [`TokenPrograms`] slot carries when the settlement moves no
/// token under that program. The system program is named by nearly every
/// settlement transaction already, so standing it in costs one more account
/// index rather than another 32-byte address.
pub use solana_system_interface::program::ID as SYSTEM_PROGRAM_ID;

/// A token program a token-moving instruction accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenProgram {
    /// The legacy SPL Token program.
    SplToken,
    /// The SPL Token-2022 program.
    Token2022,
}

impl TokenProgram {
    /// Every supported token program. The single list [`TryFrom`] resolves
    /// addresses against, and the order `BeginSettle` and `FinalizeSettle` lay
    /// their token-program accounts out in; see [`TokenPrograms::addresses`].
    pub const ALL: [Self; 2] = [Self::SplToken, Self::Token2022];

    /// The address the program is deployed at.
    pub const fn address(self) -> Pubkey {
        match self {
            Self::SplToken => spl_token_2022_interface::inline_spl_token::ID,
            Self::Token2022 => spl_token_2022_interface::ID,
        }
    }
}

impl TryFrom<&Pubkey> for TokenProgram {
    type Error = ProgramError;

    /// Resolves a program address to the token program it identifies,
    /// rejecting any address that isn't a supported token program.
    fn try_from(address: &Pubkey) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|program| program.address() == *address)
            .ok_or(ProgramError::IncorrectProgramId)
    }
}

/// Which of [`TokenProgram::ALL`] a `BeginSettle`/`FinalizeSettle` pair
/// carries.
///
/// Both instructions take one account per supported program, at fixed positions
/// and in [`TokenProgram::ALL`] order, and issue each transfer against the
/// program that owns the account it moves — so a single settlement may mix
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

    /// The addresses to pass, one per entry of [`TokenProgram::ALL`] and in
    /// that order: the program itself where the settlement needs it, and
    /// [`SYSTEM_PROGRAM_ID`] where it doesn't.
    pub const fn addresses(self) -> [Pubkey; TokenProgram::ALL.len()] {
        let [spl_token, token_2022] = TokenProgram::ALL;
        [self.slot(spl_token), self.slot(token_2022)]
    }

    /// The address `program`'s own slot holds.
    const fn slot(self, program: TokenProgram) -> Pubkey {
        if self.carries(program) {
            program.address()
        } else {
            SYSTEM_PROGRAM_ID
        }
    }

    /// Whether `program`'s slot carries it rather than the placeholder. The one
    /// place a new [`TokenProgram`] variant has to be given a slot.
    const fn carries(self, program: TokenProgram) -> bool {
        match program {
            TokenProgram::SplToken => self.spl_token,
            TokenProgram::Token2022 => self.token_2022,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::pubkey_from_seed;

    /// Adding a variant makes this match non-exhaustive, the compile-time reminder
    /// to add it to [`TokenProgram::ALL`] as well.
    const _: () = match TokenProgram::SplToken {
        TokenProgram::SplToken | TokenProgram::Token2022 => (),
    };

    #[test]
    fn every_program_resolves_from_its_own_address() {
        for program in TokenProgram::ALL {
            let address = program.address();
            assert_eq!(
                TokenProgram::try_from(&address),
                Ok(program),
                "{program:?} should resolve from {address}",
            );
        }
    }

    #[test]
    fn unrelated_program_cannot_be_resolved_as_token_program() {
        assert_eq!(
            TokenProgram::try_from(&pubkey_from_seed("not a token program")),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    /// The placeholder has to be something no token account can be owned by,
    /// or a slot carrying it would still dispatch transfers somewhere.
    #[test]
    fn the_placeholder_is_not_a_token_program() {
        assert_eq!(
            TokenProgram::try_from(&SYSTEM_PROGRAM_ID),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    /// Every combination puts each program in its own slot, and the placeholder
    /// wherever the settlement said it isn't needed.
    #[test]
    fn addresses_fill_each_slot_with_its_program_or_the_placeholder() {
        let spl_token = TokenProgram::SplToken.address();
        let token_2022 = TokenProgram::Token2022.address();
        assert_eq!(TokenPrograms::BOTH.addresses(), [spl_token, token_2022]);
        assert_eq!(
            TokenPrograms::SPL_TOKEN.addresses(),
            [spl_token, SYSTEM_PROGRAM_ID],
        );
        assert_eq!(
            TokenPrograms::TOKEN_2022.addresses(),
            [SYSTEM_PROGRAM_ID, token_2022],
        );
        assert_eq!(
            TokenPrograms::NONE.addresses(),
            [SYSTEM_PROGRAM_ID, SYSTEM_PROGRAM_ID],
        );
    }

    /// The slots are laid out in [`TokenProgram::ALL`] order, which is what
    /// lets the on-chain side pair a slot with the program it stands for by
    /// position alone.
    #[test]
    fn addresses_follow_the_supported_program_order() {
        assert_eq!(
            TokenPrograms::BOTH.addresses(),
            TokenProgram::ALL.map(TokenProgram::address),
        );
    }

    /// Carrying nothing is the default, so a builder that forgets its token
    /// programs settles no tokens rather than silently picking one.
    #[test]
    fn no_program_is_carried_by_default() {
        assert_eq!(TokenPrograms::default(), TokenPrograms::NONE);
    }
}
