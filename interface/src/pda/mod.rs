//! Program-derived addresses under the settlement program.
//!
//! Every PDA shares the [`SETTLEMENT_SEED`] prefix, which carries
//! the major and minor version of the cargo package; each submodule defines the additional seeds and the
//! derivation helper for one kind of PDA.

pub mod buffer;
pub mod order;
pub mod state;

/// Version-independent part of [`SETTLEMENT_SEED`].
const SETTLEMENT_SEED_PREFIX: &[u8] = b"settlement v";

/// Bytes reserved after [`SETTLEMENT_SEED_PREFIX`] for the version string. The
/// version is written directly after the prefix and right-padded with spaces to
/// fill this width. One character is a dot, the remaining characters are
/// reserved for the combined major/minor version.
const SETTLEMENT_SEED_VERSION_LEN: usize = 7;

/// Fixed width of [`SETTLEMENT_SEED`], in bytes: the prefix followed by the
/// reserved version field.
///
/// The seed has to be the same length for every version. This is done to avoid
/// prefix attacks on PDA addresses: a variable-width prefix could allow one
/// version's seeds be a data prefix of another's: `"…v1.10"` followed by some
/// bytes hashes the same as `"…v1.1"` followed by `'0'` and those same bytes.
pub const SETTLEMENT_SEED_LEN: usize = SETTLEMENT_SEED_PREFIX.len() + SETTLEMENT_SEED_VERSION_LEN;

/// The seed used as a base for all account storage
pub const SETTLEMENT_SEED: &[u8] = &build_padded_settlement_seed(concat!(
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
));

/// Lay out `SETTLEMENT_SEED_PREFIX` and `version` in a
/// [`SETTLEMENT_SEED_LEN`]-byte field, right-aligning the version and filling
/// the gap between the two with spaces.
///
/// Panics at compile time if the version no longer fits.
const fn build_padded_settlement_seed(version: &str) -> [u8; SETTLEMENT_SEED_LEN] {
    const OVERFLOW: &str = "the package version no longer fits the settlement seed; widen \
                            SETTLEMENT_SEED_LEN or shorten the major/minor version number, see \
                            DESIGN.md";

    let version = version.as_bytes();
    // Where the right-aligned version starts; also the first byte the prefix
    // may not reach.
    let offset = match SETTLEMENT_SEED_LEN.checked_sub(version.len()) {
        Some(offset) => offset,
        None => panic!("{}", OVERFLOW),
    };
    assert!(SETTLEMENT_SEED_PREFIX.len() <= offset, "{}", OVERFLOW);

    let mut seed = [b' '; SETTLEMENT_SEED_LEN];
    let mut i = 0;
    while i < SETTLEMENT_SEED_PREFIX.len() {
        seed[i] = SETTLEMENT_SEED_PREFIX[i];
        i = i.saturating_add(1);
    }
    let mut i = 0;
    while i < version.len() {
        seed[offset.saturating_add(i)] = version[i];
        i = i.saturating_add(1);
    }
    seed
}

#[cfg(test)]
mod tests {
    use solana_pubkey::Pubkey;

    use super::{
        build_padded_settlement_seed, SETTLEMENT_SEED, SETTLEMENT_SEED_LEN, SETTLEMENT_SEED_PREFIX,
    };

    pub(crate) const SAMPLE_VERSIONS: &[&str] = &["0.0", "0.2", "0.10", "1.0", "1.1", "10.0"];

    /// The widest `<major>.<minor>` the seed can still hold.
    fn longest_fitting_version() -> String {
        let major_width = SETTLEMENT_SEED_LEN
            .checked_sub(SETTLEMENT_SEED_PREFIX.len())
            .and_then(|width| width.checked_sub("v.9".len()))
            .expect("the seed must hold at least a one-digit major and minor");
        format!("{}.9", "9".repeat(major_width))
    }

    #[test]
    fn build_padded_settlement_seed_accepts_the_widest_fitting_version() {
        let version = longest_fitting_version();
        let seed = build_padded_settlement_seed(&format!("v{version}"));
        assert_eq!(seed.as_slice(), b"settlementv99999.9");
        // No padding left: the version butts right up against the prefix.
        assert!(!seed.contains(&b' '));
    }

    /// `build_padded_settlement_seed` is called in a const context, where these panics are
    /// compile errors rather than test failures. Calling it at runtime is the
    /// only way to observe them: the assertions are the same either way.
    #[test]
    #[should_panic(expected = "no longer fits the settlement seed")]
    fn build_padded_settlement_seed_rejects_a_version_one_byte_too_wide() {
        let version = longest_fitting_version();
        let _ = build_padded_settlement_seed(&format!("v{version}0"));
    }

    #[test]
    #[should_panic(expected = "no longer fits the settlement seed")]
    fn build_padded_settlement_seed_rejects_a_version_wider_than_the_whole_seed() {
        let _ = build_padded_settlement_seed(&"9".repeat(SETTLEMENT_SEED_LEN.saturating_add(1)));
    }

    #[test]
    fn current_settlement_seed_is_correct_length() {
        assert_eq!(SETTLEMENT_SEED.len(), SETTLEMENT_SEED_LEN);
    }

    #[test]
    fn current_settlement_seed_is_printable_ascii() {
        assert!(
            SETTLEMENT_SEED
                .iter()
                .all(|byte| byte.is_ascii_graphic() || *byte == b' '),
            "the seed must stay readable: {:?}",
            core::str::from_utf8(SETTLEMENT_SEED),
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
