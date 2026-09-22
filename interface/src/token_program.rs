//! The token programs settlement transfers may be issued against.

use crate::Pubkey;
use solana_program_error::ProgramError;
pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;

/// The address an encoded intent carries as a mint to trade native SOL rather
/// than a token; see [`Asset::Native`](crate::data::intent::Asset::Native).
pub const NATIVE_SOL_MINT: Pubkey = solana_system_interface::program::ID;

/// Whether `mint` names native SOL; see
/// [`NATIVE_SOL_MINT`].
#[must_use]
pub fn is_native_sol(mint: &Pubkey) -> bool {
    mint == &NATIVE_SOL_MINT
}

/// A token program a token-moving instruction accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenProgram {
    /// The legacy SPL Token program.
    SplToken,
    /// The SPL Token-2022 program.
    Token2022,
}

impl TokenProgram {
    /// Every supported token program.
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
    fn native_sol_mint_is_the_system_program() {
        assert!(is_native_sol(&NATIVE_SOL_MINT));
        for program in TokenProgram::ALL {
            assert!(!is_native_sol(&program.address()));
        }
    }

    #[test]
    fn an_spl_mint_is_not_native_sol() {
        assert!(!is_native_sol(&pubkey_from_seed("some mint")));
    }

    #[test]
    fn unrelated_program_cannot_be_resolved_as_token_program() {
        assert_eq!(
            TokenProgram::try_from(&pubkey_from_seed("not a token program")),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
