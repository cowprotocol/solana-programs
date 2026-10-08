//! `BeginSettle`/`FinalizeSettle` instruction tools, the instructions-sysvar
//! account ID they all reference, and the off-chain instruction builders.

use solana_account_view::AccountView;
use solana_address::Address;
use solana_program_error::ProgramError;

pub use crate::token_program::TokenProgram;
pub use solana_sdk_ids::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;

/// Only some tokens which necessitate the use of the `TransferChecked` instruction
/// require the RO mint account to be specified. To reduce unnecessary account
/// dependency, the instructions sysvar may be provided instead of the mint to
/// call `Transfer` instead.
pub const MINT_PLACEHOLDER: solana_pubkey::Pubkey = INSTRUCTIONS_SYSVAR_ID;

/// The on-chain address of an account representation, that is, the generic `A`
/// used in our parser. This can be used by implementations to use custom
/// address types in the parser, as long as they implement `Keyed`.
pub trait Keyed {
    fn key(&self) -> &Address;
}

impl Keyed for AccountView {
    fn key(&self) -> &Address {
        self.address()
    }
}

impl Keyed for Address {
    fn key(&self) -> &Address {
        self
    }
}

/// A settle instruction's mint slot: a value whose address is either a real
/// mint, naming a `TransferChecked`, or the [`MINT_PLACEHOLDER`] sentinel,
/// naming a plain `Transfer`. The two are indistinguishable as raw addresses,
/// so this wrapper forces callers through [`MaybeMint::get`] to resolve which,
/// rather than handling a bare slot that is secretly one or the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaybeMint<'a, A>(&'a A);

impl<'a, A> MaybeMint<'a, A> {
    /// Wrap a mint-slot value, deferring the placeholder check to
    /// [`MaybeMint::get`].
    pub fn new(slot: &'a A) -> Self {
        Self(slot)
    }
}

impl<'a, A: Keyed> MaybeMint<'a, A> {
    /// The mint to settle against, or `None` when the slot holds the
    /// [`MINT_PLACEHOLDER`] sentinel selecting a plain `Transfer`.
    pub fn get(&self) -> Option<&'a A> {
        if self.0.key() == &MINT_PLACEHOLDER {
            None
        } else {
            Some(self.0)
        }
    }
}

mod begin;
mod finalize;

pub use begin::{BeginSettle, BeginSettleInput, Pull, SettledOrder, SettledOrders};
pub use finalize::{
    finalize_push_data, FinalizeSettle, FinalizeSettleInput, Push, Pushes, FINALIZE_FIXED_ACCOUNTS,
    FINALIZE_PUSH_ACCOUNTS,
};

/// Reads the first two bytes of a byte slice (instruction data) and
/// interprets them as a little-endian u16, returning it together with the
/// remaining bytes to parse.
/// It's meant to be used for BeginSettle and FinalizeSettle to extract the
/// counterpart index, that is, the index linking that instruction to the
/// opposite instruction which is encoded as the first
/// 2 bytes of the instruction data: `[0x37, 0x13]` → `0x1337`.
/// Returns `InvalidInstructionData` if fewer than two bytes are provided.
pub fn recover_counterpart(instruction_data: &[u8]) -> Result<(u16, &[u8]), ProgramError> {
    match instruction_data {
        [b1, b2, rest @ ..] => Ok((u16::from_le_bytes([*b1, *b2]), rest)),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::prelude::*;
    use solana_pubkey::Pubkey;

    /// Random pushes as the parallel lists the
    /// [`FinalizeSettle`](super::FinalizeSettle) builder takes.
    #[derive(Debug)]
    pub struct ArbPushes {
        pub source_buffers: Vec<Pubkey>,
        pub destinations: Vec<Pubkey>,
        pub mints: Vec<Option<Pubkey>>,
        pub bumps: Vec<u8>,
        pub amounts: Vec<u64>,
    }

    /// Strategy producing `count` random [`ArbPushes`].
    pub fn arb_pushes(
        count: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = ArbPushes> {
        prop::collection::vec(
            (
                any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
                any::<[u8; 32]>().prop_map(Pubkey::new_from_array),
                any::<Option<[u8; 32]>>().prop_map(|mint| mint.map(Pubkey::new_from_array)),
                any::<u8>(),
                any::<u64>(),
            ),
            count,
        )
        .prop_map(|pushes| ArbPushes {
            source_buffers: pushes.iter().map(|push| push.0).collect(),
            destinations: pushes.iter().map(|push| push.1).collect(),
            mints: pushes.iter().map(|push| push.2).collect(),
            bumps: pushes.iter().map(|push| push.3).collect(),
            amounts: pushes.iter().map(|push| push.4).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::pubkey_from_seed;
    use crate::instruction::fixtures::fake_account;
    use hex_literal::hex;
    use solana_address::Address;

    /// Builds an instruction-data byte vector from a list of field chunks, so a
    /// test can spell out the wire layout one field per line without repeating
    /// the `&[..][..]` slicing. Each chunk is anything sliceable to `[u8]` (a
    /// byte array, a `Vec<u8>`, the result of `to_le_bytes()`, ...).
    macro_rules! ix_data {
        ($($chunk:expr),* $(,)?) => {
            [$(&$chunk[..]),*].concat()
        };
    }
    pub(crate) use ix_data;

    /// The address behind a mint slot, asserting it names a real mint rather
    /// than the [`MINT_PLACEHOLDER`] placeholder. For parser tests reading a
    /// settled order's or push's mint.
    pub(crate) fn mint_address(mint: MaybeMint<'_, AccountView>) -> &Address {
        mint.get().expect("a real mint").address()
    }

    #[test]
    fn rejects_empty_payload() {
        assert_eq!(
            recover_counterpart(&[]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn rejects_too_short_payload() {
        assert_eq!(
            recover_counterpart(&[42]),
            Err(ProgramError::InvalidInstructionData),
        );
    }

    #[test]
    fn returns_trailing_bytes() {
        assert_eq!(
            recover_counterpart(
                &[
                    &hex!("3713")[..], // counterpart index, little-endian
                    &[42][..],         // trailing
                ]
                .concat()
            ),
            Ok((0x1337, [42].as_slice())),
        );
    }

    #[test]
    fn maybe_mint_get_resolves_an_account_slot() {
        let mint = pubkey_from_seed("a real mint");
        let account = fake_account(mint);
        assert_eq!(mint_address(MaybeMint::new(&account)), &mint);

        let placeholder = fake_account(MINT_PLACEHOLDER);
        assert!(MaybeMint::new(&placeholder).get().is_none());
    }

    #[test]
    fn maybe_mint_get_resolves_an_address_slot() {
        let mint = pubkey_from_seed("a real mint");
        assert_eq!(MaybeMint::new(&mint).get(), Some(&mint));

        assert_eq!(MaybeMint::new(&MINT_PLACEHOLDER).get(), None);
    }
}
