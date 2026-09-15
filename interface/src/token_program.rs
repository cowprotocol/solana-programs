//! The token programs settlement transfers may be issued against.

use crate::Pubkey;
use solana_program_error::ProgramError;
pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;

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
    /// their token-program accounts out in; see [`Self::addresses`].
    pub const ALL: [Self; 2] = [Self::SplToken, Self::Token2022];

    /// The address the program is deployed at.
    pub const fn address(self) -> Pubkey {
        match self {
            Self::SplToken => spl_token_2022_interface::inline_spl_token::ID,
            Self::Token2022 => spl_token_2022_interface::ID,
        }
    }

    /// The addresses a `BeginSettle`/`FinalizeSettle` pair puts in its
    /// token-program slots, one per entry of [`Self::ALL`] and in that order.
    ///
    /// Both instructions take one account per supported program, at fixed
    /// positions, and issue each transfer against the program that owns the
    /// account it moves — so a settlement naming every program may mix tokens
    /// from both. `only_token_program` is what narrows that: `None` names them
    /// all, and `Some(program)` names just that one, leaving
    /// [`INSTRUCTIONS_SYSVAR_ID`] in every other slot.
    pub const fn addresses(only_token_program: Option<Self>) -> [Pubkey; Self::ALL.len()] {
        let [spl_token, token_2022] = Self::ALL;
        [
            spl_token.slot(only_token_program),
            token_2022.slot(only_token_program),
        ]
    }

    /// The address this program's own slot holds. The slots are not read
    /// on-chain, so a program the settlement doesn't touch is left out by
    /// standing [`INSTRUCTIONS_SYSVAR_ID`] in: nearly every settlement transaction
    /// names the system program already, so it costs one more account index
    /// rather than another 32-byte address.
    const fn slot(self, only_token_program: Option<Self>) -> Pubkey {
        match only_token_program {
            // Compared as discriminants because `PartialEq` isn't const. That
            // keeps the narrowing correct for any variant added to `ALL`,
            // rather than making this a second place to list them.
            Some(only) if only as u8 != self as u8 => INSTRUCTIONS_SYSVAR_ID,
            _ => self.address(),
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
    fn unrelated_program_cannot_be_resolved_as_token_program() {
        assert_eq!(
            TokenProgram::try_from(&pubkey_from_seed("not a token program")),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    /// The placeholder has to be something no token account can be owned by,
    /// or a slot carrying it would still execute transfers somewhere.
    #[test]
    fn the_placeholder_is_not_a_token_program() {
        assert_eq!(
            TokenProgram::try_from(&INSTRUCTIONS_SYSVAR_ID),
            Err(ProgramError::IncorrectProgramId),
        );
    }

    /// Naming every program puts each of them in its own slot, in the order
    /// the on-chain side pairs a slot with the program it stands for by.
    #[test]
    fn every_program_is_named_when_the_settlement_is_not_narrowed() {
        assert_eq!(
            TokenProgram::addresses(None),
            TokenProgram::ALL.map(TokenProgram::address),
        );
    }

    /// Narrowing to one program keeps that program in its own slot and leaves
    /// the placeholder everywhere else, so a settlement pays for the addresses
    /// of only the programs it touches.
    #[test]
    fn narrowing_to_one_program_leaves_the_placeholder_in_every_other_slot() {
        for (named, only) in TokenProgram::ALL.into_iter().enumerate() {
            let addresses = TokenProgram::addresses(Some(only));
            for (slot, (address, program)) in
                addresses.into_iter().zip(TokenProgram::ALL).enumerate()
            {
                let expected = if slot == named {
                    only.address()
                } else {
                    INSTRUCTIONS_SYSVAR_ID
                };
                assert_eq!(
                    address, expected,
                    "a settlement narrowed to {only:?} should not name {program:?}",
                );
            }
        }
    }
}
