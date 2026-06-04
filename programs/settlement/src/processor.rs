//! Shared program plumbing: instruction-input parsing and PDA creation.

use pinocchio::{
    address::MAX_SEEDS,
    cpi::{Seed, Signer},
    error::ProgramError,
    AccountView, Address, ProgramResult,
};
use pinocchio_system::instructions::CreateAccount;
use settlement_interface::{recover_discriminator, SettlementInstruction};

/// Shared components for parsing generic instruction input.
///
/// Implementations declare which [`SettlementInstruction`] discriminator they
/// belong to and parse the remaining instruction data and accounts. The
/// discriminator check is shared via the default [`parse`] implementation; an
/// impl only needs to provide [`parse_body`].
pub trait InstructionInputParsing<'a>: Sized {
    const DISCRIMINATOR: SettlementInstruction;

    fn parse_body(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError>;

    fn parse(
        instruction_data: &[u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        match recover_discriminator(instruction_data)? {
            (discriminator, remaining_data) if discriminator == Self::DISCRIMINATOR => {
                Self::parse_body(remaining_data, accounts)
            }
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

/// Create the program-owned account at the PDA `pda`, funded by `payer`.
///
/// `seeds` are the canonical PDA seeds *without* the bump and the canonical
/// bump is derived and appended here. Signing `CreateAccount` with these seeds
/// implicitly checks that `pda` is the canonical address: the runtime grants
/// the PDA signature only for the address the seeds derive, so any other `pda`
/// fails the CPI.
#[must_use = "ignoring the output means processing continues without the PDA having been created"]
pub fn create_canonical_pda<const N: usize>(
    program_id: &Address,
    payer: &AccountView,
    pda: &AccountView,
    space: u64,
    seeds: [&[u8]; N],
) -> ProgramResult {
    let (_, bump) = Address::find_program_address(&seeds, program_id);
    let bump = [bump];

    // A PDA has at most `MAX_SEEDS` seeds, so `N` stays well below `usize::MAX`
    // and the `N + 1` below cannot overflow. Asserting it in a `const` block
    // makes that a compile-time guarantee rather than a runtime risk.
    const { assert!(N < MAX_SEEDS, "a PDA has at most MAX_SEEDS seeds") };

    // The signer needs the base seeds followed by the bump. Stable Rust can't
    // name `[Seed; N + 1]`, so collect into a `Vec` sized for exactly that:
    // the `N` base seeds plus the trailing bump.
    let mut signer_seeds = Vec::with_capacity(const { N + 1 });
    signer_seeds.extend(seeds.iter().map(|seed| Seed::from(*seed)));
    signer_seeds.push(Seed::from(&bump[..]));
    let signer = Signer::from(&signer_seeds[..]);

    CreateAccount::with_minimum_balance(payer, pda, space, program_id, None)?
        .invoke_signed(&[signer])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_parsing_rejects_different_discriminator() {
        struct TestInputParsing {}
        impl<'a> InstructionInputParsing<'a> for TestInputParsing {
            const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::BeginSettle;

            fn parse_body(
                _instruction_data: &[u8],
                _accounts: &'a mut [AccountView],
            ) -> Result<Self, ProgramError> {
                Ok(Self {})
            }
        }

        let mut data = [0; 42];
        let different_discriminator = SettlementInstruction::CreateOrder;
        assert_ne!(TestInputParsing::DISCRIMINATOR, different_discriminator);
        data[0] = different_discriminator.discriminator();
        assert_eq!(
            TestInputParsing::parse(&data, &mut []).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }
}
