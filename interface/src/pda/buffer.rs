//! Buffer PDA seed and address derivation.
//!
//! Each buffer is a per-token SPL token account that holds funds controlled
//! by the settlement state PDA. It lives at a PDA keyed by the token mint, so
//! there is exactly one buffer address per token.
//!
//! The token account stored at this address is initialized by the
//! `CreateBuffer` instruction; its SPL `owner` (token authority) is the
//! settlement state PDA (see [`crate::pda::state`]), the single authority
//! controlling every buffer.
//!
//! The seeds start with [`SETTLEMENT_SEED`], which carries
//! the cargo package major and minor version, so a version bump moves every buffer.
//! Since only the state PDA can spend a buffer and that address moves too,
//! buffers must be drained under the old version before a bump ships, or their
//! contents are stranded.
//!
//! Unlike the order PDA, which stores its own bump (see
//! [`crate::data::order::OrderAccount`]), a buffer's layout belongs entirely to
//! the token program, leaving no room for one.

use solana_address::Address;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::pda::{is_pda_with_signer_seeds, SETTLEMENT_SEED};
use crate::SettlementError;

mod known_mints;
pub use known_mints::KNOWN_MINTS;

/// Trailing seed identifying the buffer PDAs.
pub const BUFFER_SEED: &[u8] = b"buffer";

/// Canonical seed components for the buffer PDA holding the specified `mint`
/// token.
///
/// `mint` is the raw 32-byte token mint address, so the same helper serves
/// both the off-chain builder and the on-chain handler (which holds the mint
/// as an `Address`).
pub const fn buffer_pda_seeds(mint: &[u8; 32]) -> [&[u8]; 3] {
    [SETTLEMENT_SEED, mint, BUFFER_SEED]
}

/// Canonical seeds for re-deriving the buffer PDA for `mint` with `bump`.
pub fn buffer_pda_signer_seeds<'a>(mint: &'a [u8; 32], bump: &'a [u8; 1]) -> [&'a [u8]; 4] {
    let [s0, s1, s2] = buffer_pda_seeds(mint);
    [s0, s1, s2, bump]
}

/// Derive the canonical buffer PDA address (and bump) for the token `mint`.
pub fn find_buffer_pda(program_id: &Pubkey, mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&buffer_pda_seeds(mint.as_array()), program_id)
}

/// A buffer PDA under [`crate::ID`] derived at compile time.
struct KnownBuffer {
    mint: [u8; 32],
    address: [u8; 32],
}

/// The buffers of [`known_mints::KNOWN_MINTS`], in the same order.
// Deriving this many PDAs in const eval trips the compiler's infinite-loop
// guard, although it finishes in seconds.
#[allow(long_running_const_eval)]
const KNOWN_BUFFERS: [KnownBuffer; known_mints::KNOWN_MINTS.len()] = {
    let mut buffers = [const {
        KnownBuffer {
            mint: [0; 32],
            address: [0; 32],
        }
    }; known_mints::KNOWN_MINTS.len()];
    let mut i = 0;
    while i < buffers.len() {
        let mint = const_crypto::bs58::decode_pubkey(known_mints::KNOWN_MINTS[i]);
        let (address, _) = const_crypto::ed25519::derive_program_address(
            &buffer_pda_seeds(&mint),
            crate::ID.as_array(),
        );
        buffers[i] = KnownBuffer { mint, address };
        i += 1;
    }
    buffers
};

/// Bits of the hash that pick a slot: 512 slots keep collisions among 64 mints
/// The number of bits representing each input slot of `KNOWN_BUFFER_SLOTS`.
/// More bits make `KNOWN_BUFFER_SLOTS` exponentially larger, and therefore
/// increase the size of the program. However, more bits also make it easier to
/// find a multiplier with no collisions, and therefore reduce compilation
/// times.
///
/// The problem of avoiding collision is the same as the birthday paradox.
/// As a rule of thumb, finding a multiplier requires an expected `e^(n²/(2m))`
/// iterations, where `n` is the number of known mints and m is 2^SLOT_BITS.
const SLOT_BITS: u32 = 9;

/// Gives every one of [`KNOWN_BUFFERS`] its own slot in [`KNOWN_BUFFER_SLOTS`].
const KNOWN_BUFFER_MULTIPLIER: u64 = find_slot_multiplier(&KNOWN_BUFFERS);

/// The index into [`KNOWN_BUFFERS`] of the mint in each slot, or `u8::MAX` for
/// an empty slot, which [`known_buffer`] then finds no buffer at.
const KNOWN_BUFFER_SLOTS: [u8; 1 << SLOT_BITS] = {
    assert!(
        KNOWN_BUFFERS.len() < u8::MAX as usize,
        "too many known mints for u8 indices"
    );
    let mut slots = [u8::MAX; 1 << SLOT_BITS];
    let mut i = 0;
    while i < KNOWN_BUFFERS.len() {
        slots[slot(&KNOWN_BUFFERS[i].mint, KNOWN_BUFFER_MULTIPLIER)] = i as u8;
        i += 1;
    }
    slots
};

/// Multiplicative hash of the leading 8 bytes of `mint`, keeping the top
/// [`SLOT_BITS`] of the product.
const fn slot(mint: &[u8; 32], multiplier: u64) -> usize {
    let key = u64::from_le_bytes(*mint.first_chunk().expect("a mint is longer than 8 bytes"));
    (key.wrapping_mul(multiplier) >> (u64::BITS - SLOT_BITS)) as usize
}

/// Try the multipliers `SHA-256(0)`, `SHA-256(1)`, ... (each truncated to its
/// first 8 bytes) until one sends each of `buffers` to a distinct [`slot`].
///
/// SHA-256 is utilized to provide a pseudorandom source of multipliers, but any PRNG would work fine here.
///
/// Panics (at compile time, for [`KNOWN_BUFFER_MULTIPLIER`]) if none does,
/// which is certain if two mints share their leading 8 bytes.
const fn find_slot_multiplier(buffers: &[KnownBuffer]) -> u64 {
    const ATTEMPTS: u32 = 100_000;
    let mut attempt: u32 = 0;
    while attempt < ATTEMPTS {
        let digest = const_crypto::sha2::Sha256::new()
            .update(&attempt.to_le_bytes())
            .finalize();
        let multiplier = u64::from_le_bytes(
            *digest
                .first_chunk()
                .expect("a digest is longer than 8 bytes"),
        );
        if has_distinct_slots(buffers, multiplier) {
            return multiplier;
        }
        attempt = attempt
            .checked_add(1)
            .expect("attempt stays below ATTEMPTS");
    }
    panic!("no multiplier gives every known mint its own slot");
}

/// Whether `multiplier` sends each of `buffers` to a different [`slot`].
const fn has_distinct_slots(buffers: &[KnownBuffer], multiplier: u64) -> bool {
    let mut taken = [false; 1 << SLOT_BITS];
    let mut i = 0;
    while i < buffers.len() {
        let slot = slot(&buffers[i].mint, multiplier);
        if taken[slot] {
            return false;
        }
        taken[slot] = true;
        i = i.checked_add(1).expect("i stays below the buffer count");
    }
    true
}

/// The compile-time buffer for `mint`, if it's one of the known mints.
fn known_buffer(mint: &[u8; 32]) -> Option<&'static KnownBuffer> {
    let index = KNOWN_BUFFER_SLOTS[slot(mint, KNOWN_BUFFER_MULTIPLIER)];
    KNOWN_BUFFERS
        .get(usize::from(index))
        .filter(|known| &known.mint == mint)
}

/// Confirm `buffer` matches the derived buffer PDA for the mint bytes `mint`
/// and `bump`.
///
/// A known mint is checked against its compile-time buffer alone, ignoring
/// `bump`: nothing signs with a buffer's seeds, so the bump only matters for
/// re-deriving the address. That buffer is derived under [`crate::ID`] whatever
/// `program_id` is, which is sound because the program only works there (see
/// [`crate::pda::state::STATE_PDA`]).
#[inline]
#[must_use = "ignoring the output means ignoring the validation result"]
pub fn validate_buffer_pda(
    program_id: &Address,
    buffer: &Address,
    mint: &[u8; 32],
    bump: u8,
) -> Result<(), ProgramError> {
    let is_buffer = match known_buffer(mint) {
        Some(known) => buffer.as_array() == &known.address,
        None => {
            is_pda_with_signer_seeds(buffer, program_id, buffer_pda_signer_seeds(mint, &[bump]))
        }
    };
    is_buffer
        .then_some(())
        .ok_or(SettlementError::PushSourceNotBuffer.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pda::tests::assert_distinct_versions_yield_distinct_pdas;

    #[test]
    fn find_buffer_pda_uses_canonical_seeds() {
        let token = Pubkey::new_unique();

        crate::pda::tests::assert_canonical_bump(
            |program_id| find_buffer_pda(program_id, &token),
            buffer_pda_seeds(token.as_array()),
        );
    }

    #[test]
    fn distinct_versions_yield_distinct_buffer_pdas() {
        let mint = Pubkey::new_unique();
        let (pda, _) = find_buffer_pda(&crate::ID, &mint);

        assert_distinct_versions_yield_distinct_pdas(&pda, &[mint.as_array(), BUFFER_SEED]);
    }

    #[test]
    fn accepts_a_valid_address() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (pda, bump) = find_buffer_pda(&program_id, &mint);

        let buffer = crate::instruction::fixtures::fake_account(pda);
        validate_buffer_pda(&program_id, buffer.address(), mint.as_array(), bump)
            .expect("the canonical buffer PDA must be accepted");
    }

    #[test]
    fn rejects_an_invalid_address() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (_, bump) = find_buffer_pda(&program_id, &mint);

        // An account sitting at some other address is not the buffer.
        let buffer = crate::instruction::fixtures::fake_account(Pubkey::new_unique());
        let err = validate_buffer_pda(&program_id, buffer.address(), mint.as_array(), bump)
            .expect_err("a non-canonical address must be rejected");
        assert_eq!(err, SettlementError::PushSourceNotBuffer.into());
    }

    #[test]
    fn rejects_a_wrong_bump() {
        let program_id = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let (pda, bump) = find_buffer_pda(&program_id, &mint);

        // The address is canonical but the carried bump doesn't derive it.
        let buffer = crate::instruction::fixtures::fake_account(pda);
        let err = validate_buffer_pda(&program_id, buffer.address(), mint.as_array(), bump ^ 1)
            .expect_err("a wrong bump must be rejected");
        assert_eq!(err, SettlementError::PushSourceNotBuffer.into());
    }

    const A_KNOWN_MINT: Pubkey = Pubkey::from_str_const(KNOWN_MINTS[0]);

    #[test]
    fn known_buffers_are_canonical() {
        assert_eq!(KNOWN_BUFFERS.len(), known_mints::KNOWN_MINTS.len());
        for known in &KNOWN_BUFFERS {
            let (pda, _) = find_buffer_pda(&crate::ID, &Pubkey::new_from_array(known.mint));
            assert_eq!(known.address, *pda.as_array());
        }
    }

    #[test]
    fn every_known_mint_looks_up_its_own_buffer() {
        for mint in known_mints::KNOWN_MINTS {
            let mint = Pubkey::from_str_const(mint);
            let known = known_buffer(mint.as_array())
                .unwrap_or_else(|| panic!("{mint} must have a known buffer"));
            assert_eq!(known.mint, *mint.as_array());
        }
    }

    #[test]
    fn unknown_mint_sharing_a_slot_has_no_known_buffer() {
        let mut mint = *A_KNOWN_MINT.as_array();
        mint[31] ^= 1;
        assert!(known_buffer(&mint).is_none());
    }

    #[test]
    fn unknown_mint_in_an_empty_slot_has_no_known_buffer() {
        let mint = (0u64..)
            .map(|seed| *crate::fixtures::pubkey_from_seed(&seed.to_string()).as_array())
            .find(|mint| KNOWN_BUFFER_SLOTS[slot(mint, KNOWN_BUFFER_MULTIPLIER)] == u8::MAX)
            .expect("most slots are empty");
        assert!(known_buffer(&mint).is_none());
    }

    /// `find_slot_multiplier` runs in a const context, where this panic is a
    /// compile error; calling it at runtime is the only way to observe it.
    #[test]
    #[should_panic(expected = "no multiplier gives every known mint its own slot")]
    fn find_slot_multiplier_rejects_mints_sharing_their_leading_bytes() {
        let mut other = *A_KNOWN_MINT.as_array();
        other[31] ^= 1;
        let _ = find_slot_multiplier(&[
            KnownBuffer {
                mint: *A_KNOWN_MINT.as_array(),
                address: [0; 32],
            },
            KnownBuffer {
                mint: other,
                address: [0; 32],
            },
        ]);
    }

    #[test]
    fn accepts_the_known_buffer() {
        let (pda, bump) = find_buffer_pda(&crate::ID, &A_KNOWN_MINT);

        validate_buffer_pda(&crate::ID, &pda, A_KNOWN_MINT.as_array(), bump)
            .expect("the known buffer PDA must be accepted");
    }

    #[test]
    fn rejects_an_invalid_address_for_a_known_mint() {
        let (_, bump) = find_buffer_pda(&crate::ID, &A_KNOWN_MINT);

        let err = validate_buffer_pda(
            &crate::ID,
            &Pubkey::new_unique(),
            A_KNOWN_MINT.as_array(),
            bump,
        )
        .expect_err("a non-canonical address must be rejected");
        assert_eq!(err, SettlementError::PushSourceNotBuffer.into());
    }

    #[test]
    fn ignores_the_bump_for_a_known_mint() {
        let (pda, bump) = find_buffer_pda(&crate::ID, &A_KNOWN_MINT);

        validate_buffer_pda(&crate::ID, &pda, A_KNOWN_MINT.as_array(), bump ^ 1)
            .expect("the known buffer PDA must be accepted whatever the bump");
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn distinct_tokens_yield_distinct_pdas(
                program_id in any::<[u8; 32]>(),
                token1 in any::<[u8; 32]>(),
                token2 in any::<[u8; 32]>(),
            ) {
                prop_assume!(token1 != token2);
                let program_id = Pubkey::new_from_array(program_id);
                let (pda1, _) = find_buffer_pda(&program_id, &Pubkey::new_from_array(token1));
                let (pda2, _) = find_buffer_pda(&program_id, &Pubkey::new_from_array(token2));
                prop_assert_ne!(pda1, pda2);
            }
        }
    }
}
