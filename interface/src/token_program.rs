//! Utilities related to the token programs supported by the settlement program.

use crate::Pubkey;
use solana_program_error::ProgramError;

/// The address an `OrderIntent` uses as `buy_mint` to trade native SOL rather than a token.
pub const NATIVE_SOL_MINT: Pubkey = solana_system_interface::program::ID;

/// What an order's buy side pays out, as named by an `OrderIntent`'s
/// `buy_mint`.
///
/// The wire format is a single 32-byte address either way — [`NATIVE_SOL_MINT`]
/// for [`NativeSol`](Self::NativeSol), the mint itself for
/// [`Token`](Self::Token) — so the enum costs nothing on the wire. What it buys
/// is on the Rust side: the two payout paths (lamports out of the settlement
/// state PDA, tokens out of the mint's canonical buffer PDA) become a match the
/// compiler checks exhaustively, rather than an address comparison every caller
/// has to remember to make.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum BuyMint {
    /// Proceeds are lamports. Also what `Default` yields, since
    /// [`NATIVE_SOL_MINT`] is the all-zero address a defaulted `Pubkey`
    /// encodes to.
    #[default]
    NativeSol,
    /// Proceeds are SPL tokens of this mint. Never holds [`NATIVE_SOL_MINT`]:
    /// [`BuyMint::from`] resolves that address to
    /// [`NativeSol`](Self::NativeSol), so each variant owns a disjoint set of
    /// addresses and the encoding stays a bijection.
    Token(Pubkey),
}

impl BuyMint {
    /// The mint address the encoding carries for this payout.
    #[must_use]
    pub const fn address(self) -> Pubkey {
        match self {
            Self::NativeSol => NATIVE_SOL_MINT,
            Self::Token(mint) => mint,
        }
    }
}

impl From<Pubkey> for BuyMint {
    /// Resolves an encoded `buy_mint` address: [`NATIVE_SOL_MINT`] is the
    /// native SOL sentinel, every other address names a token mint. Total, so
    /// no `buy_mint` byte pattern can fail to decode.
    fn from(mint: Pubkey) -> Self {
        if mint == NATIVE_SOL_MINT {
            Self::NativeSol
        } else {
            Self::Token(mint)
        }
    }
}

impl From<BuyMint> for Pubkey {
    fn from(buy_mint: BuyMint) -> Self {
        buy_mint.address()
    }
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
    /// Every supported token program, in no particular order. The single list
    /// [`TryFrom`] resolves addresses against.
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
        assert_eq!(BuyMint::from(NATIVE_SOL_MINT), BuyMint::NativeSol);
        for program in TokenProgram::ALL {
            assert_eq!(
                BuyMint::from(program.address()),
                BuyMint::Token(program.address()),
            );
        }
    }

    #[test]
    fn an_spl_mint_is_not_native_sol() {
        let mint = pubkey_from_seed("some mint");
        assert_eq!(BuyMint::from(mint), BuyMint::Token(mint));
    }

    /// The sentinel has exactly one representation, so an intent buying native
    /// SOL has exactly one encoding, and therefore one UID.
    #[test]
    fn every_address_round_trips_through_its_variant() {
        for mint in [NATIVE_SOL_MINT, pubkey_from_seed("some mint")] {
            let buy_mint = BuyMint::from(mint);
            assert_eq!(buy_mint.address(), mint);
            assert_eq!(BuyMint::from(buy_mint.address()), buy_mint);
        }
    }

    /// `Default` has to keep meaning what it meant when `buy_mint` was a bare
    /// `Pubkey`: a defaulted intent buys native SOL.
    #[test]
    fn default_buy_mint_is_the_default_address() {
        assert_eq!(BuyMint::default().address(), Pubkey::default());
    }

    #[test]
    fn unrelated_program_cannot_be_resolved_as_token_program() {
        assert_eq!(
            TokenProgram::try_from(&pubkey_from_seed("not a token program")),
            Err(ProgramError::IncorrectProgramId),
        );
    }
}
