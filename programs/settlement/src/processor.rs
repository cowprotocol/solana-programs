//! Shared program plumbing: canonical PDA creation.

use pinocchio::{
    address::MAX_SEEDS,
    cpi::{Seed, Signer},
    AccountView, Address, ProgramResult,
};

use pinocchio_system::instructions::CreateAccount;

use solana_instruction::{syscalls::get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};

/// Description of a canonical PDA to create: the account at `pda`, assigned to
/// `owner` and funded by `payer`.
///
/// `seeds` are the canonical PDA seeds *without* the bump; the canonical bump
/// is derived under `program_id` and appended in [`Self::create`]. Signing
/// `CreateAccount` with these seeds implicitly checks that `pda` is the
/// canonical address: the runtime grants the PDA signature only for the address
/// the seeds derive, so any other `pda` fails the CPI.
///
/// `owner` is the program the new account is assigned to. It is usually
/// `program_id` (a program-owned PDA), but differs when the account must be
/// owned by another program, as for example a buffer token account owned by the
/// SPL Token program.
pub struct CanonicalPda<'a, const N: usize> {
    pub program_id: &'a Address,
    pub payer: &'a AccountView,
    pub pda: &'a AccountView,
    pub size: u64,
    pub owner: &'a Address,
    pub seeds: [&'a [u8]; N],
}

impl<const N: usize> CanonicalPda<'_, N> {
    /// Create the described account, funding it from `payer` and signing the
    /// allocation with the canonical seeds.
    #[must_use = "ignoring the output means processing continues without the PDA having been created"]
    pub fn create(self) -> ProgramResult {
        let (_, bump) = Address::find_program_address(&self.seeds, self.program_id);
        let bump = [bump];

        // A PDA has at most `MAX_SEEDS` seeds, so `N` stays well below
        // `usize::MAX` and the `N + 1` below cannot overflow. Asserting it in a
        // `const` block makes that a compile-time guarantee rather than a
        // runtime risk.
        const { assert!(N < MAX_SEEDS, "a PDA has at most MAX_SEEDS seeds") };

        // The signer needs the base seeds followed by the bump. Stable Rust
        // can't name `[Seed; N + 1]`, so collect into a `Vec` sized for exactly
        // that: the `N` base seeds plus the trailing bump.
        let mut signer_seeds = Vec::with_capacity(const { N + 1 });
        signer_seeds.extend(self.seeds.iter().map(|seed| Seed::from(*seed)));
        signer_seeds.push(Seed::from(&bump[..]));
        let signer = Signer::from(&signer_seeds[..]);

        CreateAccount::with_minimum_balance(self.payer, self.pda, self.size, self.owner, None)?
            .invoke_signed(&[signer])?;
        Ok(())
    }
}

pub fn is_cpi_call() -> bool {
    get_stack_height() > TRANSACTION_LEVEL_STACK_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_cpi_false_outside_solana_lib() {
        assert!(!is_cpi_call());
    }
}
