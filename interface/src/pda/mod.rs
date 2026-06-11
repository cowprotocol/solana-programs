//! Program-derived addresses under the settlement program.
//!
//! Every PDA shares the [`SETTLEMENT_SEED`] prefix; each submodule defines
//! the additional seeds and the derivation helper for one kind of PDA.

pub mod order;
pub mod state;

/// First seed of every PDA derived under the settlement program.
pub const SETTLEMENT_SEED: &[u8] = b"settlement";

#[cfg(test)]
mod tests {
    use solana_pubkey::Pubkey;

    /// Assert that the PDA returned by `find_pda` is derived with the canonical
    /// bump for the seed scheme `signer_seeds` encapsulates.
    ///
    /// The canonical bump is the largest value in `0..=255` that yields a valid
    /// (off-curve) address: any higher bump must be rejected, and the canonical
    /// one must reproduce the derived address.
    ///
    /// `find_pda` is `find_*_pda` with the program id (and any other
    /// parameters) captured. `seeds` are the base seeds of the scheme under
    /// test, without a bump; each candidate bump is appended here to form the
    /// full signer seeds.
    pub(crate) fn assert_canonical_bump<const SIZE: usize, F1>(find_pda: F1, seeds: [&[u8]; SIZE])
    where
        F1: Fn(&Pubkey) -> (solana_pubkey::Pubkey, u8),
    {
        let program_id = Pubkey::new_unique();
        let (pda, canonical_bump) = find_pda(&program_id);

        let try_create_address = |bump| {
            let bump = [bump];
            let mut signer_seeds = seeds.to_vec();
            signer_seeds.push(&bump);
            Pubkey::create_program_address(&signer_seeds, &program_id)
        };

        if let Some(first_invalid_bump) = canonical_bump.checked_add(1) {
            for candidate in first_invalid_bump..=u8::MAX {
                assert!(
                    try_create_address(candidate).is_err(),
                    "bump {candidate} above the canonical bump {canonical_bump} must be invalid",
                );
            }
        }

        let expected = try_create_address(canonical_bump)
            .expect("canonical bump must produce a valid address");
        assert_eq!(pda, expected);
    }
}
