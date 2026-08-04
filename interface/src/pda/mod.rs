//! Program-derived addresses under the settlement program.
//!
//! Every PDA shares the [`SETTLEMENT_SEED`] prefix, which carries
//! [`STATE_VERSION`]; each submodule defines the additional seeds and the
//! derivation helper for one kind of PDA.

pub mod buffer;
pub mod order;
pub mod state;

/// The seed used as a base for all account storage
pub const SETTLEMENT_SEED: &[u8] = concat!(
    "settlement v",
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
)
.as_bytes();

#[cfg(test)]
mod tests {
    use solana_pubkey::Pubkey;

    use super::SETTLEMENT_SEED;

    pub(crate) const SAMPLE_VERSIONS: &[&str] = &["0.0", "0.2", "0.10", "1.0", "1.1", "10.0"];

    pub(crate) fn settlement_seed_for(version: &str) -> Vec<u8> {
        format!("settlement v{version}").into_bytes()
    }

    #[test]
    fn settlement_seed_is_printable_ascii() {
        assert!(
            SETTLEMENT_SEED
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' '),
            "the seed must stay readable: {:?}",
            core::str::from_utf8(SETTLEMENT_SEED),
        );
    }

    #[test]
    fn settlement_seed_fits_a_pda_seed() {
        // A version wide enough to overflow this would break every derivation.
        assert!(
            SETTLEMENT_SEED.len() <= solana_pubkey::MAX_SEED_LEN,
            "the seed is {} bytes, over the {}-byte limit",
            SETTLEMENT_SEED.len(),
            solana_pubkey::MAX_SEED_LEN,
        );
    }

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
