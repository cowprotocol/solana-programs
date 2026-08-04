//! Program-derived addresses under the settlement program.
//!
//! Every PDA shares the [`SETTLEMENT_SEED`] prefix, which carries
//! [`STATE_VERSION`]; each submodule defines the additional seeds and the
//! derivation helper for one kind of PDA.

pub mod buffer;
pub mod order;
pub mod state;

/// Version of the state stored by the settlement program.
///
/// Every PDA the program derives carries this version in its
/// [`SETTLEMENT_SEED`] prefix, so incrementing it relocates the program's whole
/// address space at once: the state account, every buffer, and every order.
/// Accounts written by an older version become unreachable to the new one
/// instead of being reinterpreted under a layout they were not written with.
///
/// Increment this for any state-breaking change: the layout of
/// [`crate::data::state`], [`crate::data::order`] or [`crate::data::intent`],
/// the meaning of an existing field, or the seed scheme of any PDA. Read the
/// "State versioning" section of `DESIGN.md` before bumping: a bump invalidates
/// every user delegation and strands whatever the current buffers still hold.
pub const STATE_VERSION: u8 = 1;

/// ASCII prefix every settlement PDA seed carries, ahead of [`STATE_VERSION`].
const SETTLEMENT_PREFIX: &[u8] = b"settlement";

/// Number of decimal digits needed to spell `version`. A [`u8`] never needs
/// more than three.
const fn decimal_width(version: u8) -> usize {
    if version < 10 {
        1
    } else if version < 100 {
        2
    } else {
        3
    }
}

/// Length of [`SETTLEMENT_SEED`]: [`SETTLEMENT_PREFIX`] plus the decimal digits
/// of [`STATE_VERSION`].
const SEED_LEN: usize = SETTLEMENT_PREFIX.len() + decimal_width(STATE_VERSION);

/// Backing storage for [`SETTLEMENT_SEED`].
///
/// Built here rather than in a `const fn` because the workspace denies
/// `clippy::arithmetic_side_effects`, and that lint skips the bodies of `const`
/// items but not those of `const fn`s. `copy_from_slice` isn't available in a
/// `const` context either, hence the explicit copy loop.
const SETTLEMENT_SEED_BYTES: [u8; SEED_LEN] = {
    let mut seed = [0u8; SEED_LEN];

    let mut i = 0;
    while i < SETTLEMENT_PREFIX.len() {
        seed[i] = SETTLEMENT_PREFIX[i];
        i += 1;
    }

    // Fill the digits right to left. `SEED_LEN` is sized for exactly as many
    // digits as `STATE_VERSION` has, so this consumes it completely.
    let mut remaining = STATE_VERSION;
    let mut j = SEED_LEN;
    while j > SETTLEMENT_PREFIX.len() {
        j -= 1;
        seed[j] = b'0' + remaining % 10;
        remaining /= 10;
    }

    seed
};

/// First seed of every PDA derived under the settlement program: the ASCII
/// string `settlement` followed by [`STATE_VERSION`] in decimal, for example
/// `settlement1`.
///
/// The seed is printable ASCII so that it stays legible wherever seeds surface
/// — explorers, logs, `solana account` output — rather than showing up as a raw
/// byte.
pub const SETTLEMENT_SEED: &[u8] = &SETTLEMENT_SEED_BYTES;

#[cfg(test)]
mod tests {
    use solana_pubkey::Pubkey;

    use super::{SETTLEMENT_SEED, STATE_VERSION};

    /// [`SETTLEMENT_SEED`] for an arbitrary `version`.
    ///
    /// Spelled out the straightforward way, so that it cross-checks the
    /// compile-time construction rather than restating it. Also lets the
    /// per-PDA tests derive addresses for versions other than the current one,
    /// which [`STATE_VERSION`] being a constant otherwise rules out.
    pub(crate) fn settlement_seed_for(version: u8) -> Vec<u8> {
        format!("settlement{version}").into_bytes()
    }

    #[test]
    fn settlement_seed_matches_formatted_version() {
        assert_eq!(SETTLEMENT_SEED, settlement_seed_for(STATE_VERSION));
    }

    #[test]
    fn settlement_seed_is_printable_ascii() {
        assert!(
            SETTLEMENT_SEED.iter().all(u8::is_ascii_graphic),
            "the seed must stay readable: {:?}",
            core::str::from_utf8(SETTLEMENT_SEED),
        );
    }

    #[test]
    fn settlement_seed_for_is_injective() {
        // Distinct versions must never share a seed, or two versions would
        // collide on the same address space.
        let mut seen = std::collections::HashSet::new();
        for version in u8::MIN..=u8::MAX {
            assert!(
                seen.insert(settlement_seed_for(version)),
                "version {version} reuses an earlier version's seed",
            );
        }
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
