//! Order intents and their canonical byte representation.
//!
//! The intent has two representations:
//!
//! - [`OrderIntent`] is the idiomatic Rust representation.
//! - [`EncodedOrderIntent`] is its canonical byte representation: the only
//!   thing sent on the wire and also the data encoding used to generate the
//!   order UID.
//!
//! Conversion is asymmetric: [`EncodedOrderIntent`]`::from(OrderIntent)` is
//! infallible, but decoding raw bytes via [`OrderIntent`]`::try_from` returns
//! `Result` and rejects a flags byte carrying a bit the encoding doesn't
//! define.

use core::mem::size_of;

use arrayref::{array_refs, mut_array_refs};
use derive_more::Deref;
use solana_hash::Hash;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

/// Direction of the trade. The discriminants are the values the `kind` bit of
/// the encoded flags byte takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
#[repr(u8)]
pub enum OrderKind {
    #[default]
    Sell = 0,
    Buy = 1,
}

/// Collection of [`OrderIntent`] fields that can be represented as a single bit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct Flags {
    /// Whether `sell_amount` or `buy_amount` is the exact figure; the
    /// other side is treated as the limit (minimum to receive for `Sell`,
    /// maximum to spend for `Buy`).
    pub kind: OrderKind,

    /// If `true`, the order may be filled across multiple settlements;
    /// proceeds and consumption scale proportionally with the amount of
    /// the sell side that's been used. If `false`, a single settlement
    /// must consume the full sell amount (fill-or-kill).
    pub partially_fillable: bool,
}

impl Flags {
    // The bit each field occupies
    const PARTIALLY_FILLABLE: u8 = 1 << 1;
    const KIND: u8 = 1 << 0;

    /// Every bit the encoding defines; the others are reserved.
    const DEFINED: u8 = Self::PARTIALLY_FILLABLE | Self::KIND;
}

impl From<Flags> for [u8; 1] {
    /// The canonical flags byte. Reserved bits are left clear.
    fn from(flags: Flags) -> Self {
        let mut byte = 0;
        if flags.partially_fillable {
            byte |= Flags::PARTIALLY_FILLABLE;
        }
        if flags.kind == OrderKind::Buy {
            byte |= Flags::KIND;
        }
        [byte]
    }
}

impl TryFrom<[u8; 1]> for Flags {
    type Error = ProgramError;

    /// Decodes a flags byte, rejecting any reserved bit with
    /// [`ProgramError::InvalidInstructionData`]. A reserved bit carries no
    /// meaning to this version of the program, so accepting it would give the
    /// same flags several encodings, and with them several UIDs.
    fn try_from(bytes: [u8; 1]) -> Result<Self, Self::Error> {
        let [byte] = bytes;
        if byte & !Self::DEFINED != 0 {
            return Err(ProgramError::InvalidInstructionData);
        }
        Ok(Flags {
            kind: if byte & Self::KIND == 0 {
                OrderKind::Sell
            } else {
                OrderKind::Buy
            },
            partially_fillable: byte & Self::PARTIALLY_FILLABLE != 0,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct OrderIntent {
    /// Account authorized to create and invalidate this order and whose
    /// signature authenticates it. For off-chain orders this is the Ed25519
    /// signer; for on-chain creation it must be the transaction signer.
    pub owner: Pubkey,

    /// Token account that receives the buy-side proceeds. Implicitly
    /// encodes both the recipient and the buy token, since SPL token
    /// accounts are token-specific.
    pub buy_token_account: Pubkey,

    /// Token account the sell-side funds are pulled from. Implicitly
    /// encodes both the spender and the sell token. The settlement state
    /// PDA must hold the SPL `delegate` on this account for the order to
    /// be settleable.
    /// This token account must be owned by the intent owner. An intent
    /// that doesn't satisfy this property will be rejected.
    pub sell_token_account: Pubkey,

    /// Amount of the sell token. For `Sell` orders this is the exact
    /// amount to be sold (subject to `partially_fillable`); for `Buy`
    /// orders it is the maximum the user is willing to spend.
    pub sell_amount: u64,

    /// Amount of the buy token. For `Buy` orders this is the exact amount
    /// to be received (subject to `partially_fillable`); for `Sell`
    /// orders it is the minimum the user is willing to receive.
    pub buy_amount: u64,

    /// Unix timestamp after which the order expires.
    /// The order cannot be executed after expiration.
    pub valid_to: u32,

    /// The settings the encoding packs bit by bit into a single byte; see
    /// [`Flags`].
    pub flags: Flags,

    /// Opaque 32 bytes set by the order creator. Not interpreted by the
    /// settlement program; used off-chain for metadata such as the
    /// frontend version, slippage hints, or attribution.
    pub app_data: [u8; 32],
}

/// Canonical 149-byte representation of an [`OrderIntent`]. The wire format and
/// the order UID preimage.
///
/// Layout: one character per byte, cell widths proportional to field size,
/// each divider belongs to the cell on its right. The byte range is
/// annotated below. Amounts and `valid_to` are little-endian encoded.
///
/// ```text
///                                                                                                            flags ───┐
/// ┌───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────┬───────┬───┬┬───────────────────────────────┐
/// │                               │                               │                               │sell_  │buy_   │val││                               │
/// │             owner             │       buy_token_account       │       sell_token_account      │       │       │id_││            app_data           │
/// │                               │                               │                               │amount │amount │to ││                               │
/// └───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────┴───────┴───┴┴───────────────────────────────┘
/// 0                               32                              64                              96      104     112 116                              149
///                                                                                                                      117
/// ```
///
#[derive(Clone, Debug, Deref, Eq, PartialEq)]
pub struct EncodedOrderIntent([u8; Self::SIZE]);

impl EncodedOrderIntent {
    // Per-field widths, derived from the `OrderIntent` field types.
    const WIDTH_OWNER: usize = size_of::<Pubkey>();
    const WIDTH_BUY_TOKEN: usize = size_of::<Pubkey>();
    const WIDTH_SELL_TOKEN: usize = size_of::<Pubkey>();
    const WIDTH_SELL_AMOUNT: usize = size_of::<u64>();
    const WIDTH_BUY_AMOUNT: usize = size_of::<u64>();
    const WIDTH_VALID_TO: usize = size_of::<u32>();
    const WIDTH_FLAGS: usize = size_of::<u8>();
    const WIDTH_APP_DATA: usize = size_of::<[u8; 32]>();

    pub const SIZE: usize = 149;

    /// Canonical hash of the bytes.
    pub fn hash(&self) -> Hash {
        hash_bytes(&self.0)
    }

    /// Decode raw bytes to an [`OrderIntent`] and compute the UID in one shot.
    /// Returns [`ProgramError::InvalidInstructionData`] for a flags byte that
    /// doesn't encode correctly; every other byte combination decodes.
    pub fn decode_and_hash(bytes: &[u8; Self::SIZE]) -> Result<(OrderIntent, Hash), ProgramError> {
        let intent = OrderIntent::try_from(bytes)?;
        // The UID is the SHA-256 of the input bytes. Hashing the input
        // (no re-encode) is correct because encode/decode is a bijection on
        // inputs that pass validation. Any normalization added to the `From`
        // or `TryFrom` impls later would break this and the UID would silently
        // diverge from `OrderIntent::uid()`.
        let uid = hash_bytes(bytes);
        Ok((intent, uid))
    }
}

pub fn hash_bytes(bytes: &[u8; EncodedOrderIntent::SIZE]) -> Hash {
    solana_sha256_hasher::hashv(&[bytes.as_slice()])
}

impl From<&EncodedOrderIntent> for [u8; EncodedOrderIntent::SIZE] {
    fn from(encoded: &EncodedOrderIntent) -> Self {
        encoded.0
    }
}

impl From<&OrderIntent> for EncodedOrderIntent {
    fn from(intent: &OrderIntent) -> Self {
        // `mut_array_refs` checks that `SIZE` is consistent with the sum of
        // the widths.
        let mut out = [0u8; Self::SIZE];
        let (owner, buy_token, sell_token, sell_amount, buy_amount, valid_to, flags, app_data) = mut_array_refs![
            &mut out,
            EncodedOrderIntent::WIDTH_OWNER,
            EncodedOrderIntent::WIDTH_BUY_TOKEN,
            EncodedOrderIntent::WIDTH_SELL_TOKEN,
            EncodedOrderIntent::WIDTH_SELL_AMOUNT,
            EncodedOrderIntent::WIDTH_BUY_AMOUNT,
            EncodedOrderIntent::WIDTH_VALID_TO,
            EncodedOrderIntent::WIDTH_FLAGS,
            EncodedOrderIntent::WIDTH_APP_DATA
        ];
        *owner = intent.owner.to_bytes();
        *buy_token = intent.buy_token_account.to_bytes();
        *sell_token = intent.sell_token_account.to_bytes();
        *sell_amount = intent.sell_amount.to_le_bytes();
        *buy_amount = intent.buy_amount.to_le_bytes();
        *valid_to = intent.valid_to.to_le_bytes();
        *flags = intent.flags.into();
        *app_data = intent.app_data;
        Self(out)
    }
}

impl TryFrom<&[u8; EncodedOrderIntent::SIZE]> for OrderIntent {
    type Error = ProgramError;

    fn try_from(bytes: &[u8; EncodedOrderIntent::SIZE]) -> Result<Self, Self::Error> {
        // It's important that the byte representation of an intent is unique.
        // This function should be injective: there shouldn't be two byte
        // sequences that decode to the same order intent.
        // If this were to happen, then the user intent may not be recognized
        // as valid or it might be possible to replay the same order more
        // than once.

        let (owner, buy_token, sell_token, sell_amount, buy_amount, valid_to, flags, app_data) = array_refs![
            bytes,
            EncodedOrderIntent::WIDTH_OWNER,
            EncodedOrderIntent::WIDTH_BUY_TOKEN,
            EncodedOrderIntent::WIDTH_SELL_TOKEN,
            EncodedOrderIntent::WIDTH_SELL_AMOUNT,
            EncodedOrderIntent::WIDTH_BUY_AMOUNT,
            EncodedOrderIntent::WIDTH_VALID_TO,
            EncodedOrderIntent::WIDTH_FLAGS,
            EncodedOrderIntent::WIDTH_APP_DATA
        ];

        Ok(OrderIntent {
            owner: Pubkey::new_from_array(*owner),
            buy_token_account: Pubkey::new_from_array(*buy_token),
            sell_token_account: Pubkey::new_from_array(*sell_token),
            sell_amount: u64::from_le_bytes(*sell_amount),
            buy_amount: u64::from_le_bytes(*buy_amount),
            valid_to: u32::from_le_bytes(*valid_to),
            flags: Flags::try_from(*flags)?,
            app_data: *app_data,
        })
    }
}

impl TryFrom<&EncodedOrderIntent> for OrderIntent {
    type Error = ProgramError;

    fn try_from(encoded: &EncodedOrderIntent) -> Result<Self, Self::Error> {
        OrderIntent::try_from(&encoded.0)
    }
}

impl OrderIntent {
    /// SHA-256 of the canonical bytes. Doubles as the order UID and the
    /// middle seed of the order PDA. On SBF this compiles to a single
    /// `sol_sha256` syscall; off-target it goes through the `sha2` crate.
    pub fn uid(&self) -> Hash {
        EncodedOrderIntent::from(self).hash()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::{prelude::*, strategy::Union};

    use super::{Flags, OrderIntent, OrderKind, Pubkey};

    /// Every valid [`OrderKind`].
    pub const ALL_ORDER_KINDS: [OrderKind; 2] = [OrderKind::Sell, OrderKind::Buy];

    // Hardcoded but verified in a sanity-check test.
    pub const FLAGS_OFFSET: usize = 116;

    pub fn sample_intent(flags: Flags) -> OrderIntent {
        OrderIntent {
            owner: Pubkey::new_from_array([0x11; 32]),
            buy_token_account: Pubkey::new_from_array([0x22; 32]),
            sell_token_account: Pubkey::new_from_array([0x33; 32]),
            sell_amount: 0x0123_4567_89ab_cdef,
            buy_amount: 0xfedc_ba98_7654_3210,
            valid_to: 0xdead_beef,
            flags,
            app_data: [0x44; 32],
        }
    }

    /// Any valid [`OrderKind`].
    pub fn arb_order_kind() -> impl Strategy<Value = OrderKind> {
        Union::new(ALL_ORDER_KINDS.map(Just))
    }

    /// Any valid [`Flags`].
    pub fn arb_flags() -> impl Strategy<Value = Flags> {
        (arb_order_kind(), any::<bool>()).prop_map(|(kind, partially_fillable)| Flags {
            kind,
            partially_fillable,
        })
    }

    /// Any flags byte the decoder accepts.
    pub fn arb_flags_byte() -> impl Strategy<Value = u8> {
        any::<u8>().prop_map(|byte| byte & Flags::DEFINED)
    }

    /// Any flags byte the decoder rejects.
    pub fn arb_invalid_flags_byte() -> impl Strategy<Value = u8> {
        any::<u8>().prop_filter("must have at least one bit that is undefined", |byte| {
            byte & !Flags::DEFINED > 0
        })
    }

    /// Any valid [`OrderIntent`].
    pub fn arb_order_intent() -> impl Strategy<Value = OrderIntent> {
        (
            any::<[u8; 32]>(),
            any::<[u8; 32]>(),
            any::<[u8; 32]>(),
            any::<u64>(),
            any::<u64>(),
            any::<u32>(),
            arb_flags(),
            any::<[u8; 32]>(),
        )
            .prop_map(
                |(owner, buy_tok, sell_tok, sell_amount, buy_amount, valid_to, flags, app)| {
                    OrderIntent {
                        owner: Pubkey::new_from_array(owner),
                        buy_token_account: Pubkey::new_from_array(buy_tok),
                        sell_token_account: Pubkey::new_from_array(sell_tok),
                        sell_amount,
                        buy_amount,
                        valid_to,
                        flags,
                        app_data: app,
                    }
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use hex_literal::hex;

    use super::fixtures::{sample_intent, FLAGS_OFFSET};
    use super::*;

    // Every shape an `OrderIntent` can take on its validated axes: the `kind`
    // enum and the `partially_fillable` flag bit.
    fn all_flag_shapes() -> impl Iterator<Item = OrderIntent> {
        fixtures::ALL_ORDER_KINDS.into_iter().flat_map(|kind| {
            [false, true].into_iter().map(move |partially_fillable| {
                sample_intent(Flags {
                    kind,
                    partially_fillable,
                })
            })
        })
    }

    // Pin each width to the size of the `OrderIntent` field it encodes. The
    // widths summing to `SIZE` is enforced separately, at compile time, by the
    // `array_refs!` / `mut_array_refs!` invocations in the codec.
    #[test]
    fn widths_match_field_sizes() {
        use core::mem::{size_of, size_of_val};

        // Any `OrderIntent` works: `size_of_val` only consults the field
        // type, never the data.
        let intent = sample_intent(Default::default());

        assert_eq!(EncodedOrderIntent::WIDTH_OWNER, size_of_val(&intent.owner));
        assert_eq!(
            EncodedOrderIntent::WIDTH_BUY_TOKEN,
            size_of_val(&intent.buy_token_account)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_SELL_TOKEN,
            size_of_val(&intent.sell_token_account)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_SELL_AMOUNT,
            size_of_val(&intent.sell_amount)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_BUY_AMOUNT,
            size_of_val(&intent.buy_amount)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_VALID_TO,
            size_of_val(&intent.valid_to)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_FLAGS,
            // in truth if there was a problem here it would actually cause a compilation error
            size_of_val::<[u8; 1]>(&Flags::default().into())
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_APP_DATA,
            size_of_val(&intent.app_data)
        );

        assert_eq!(EncodedOrderIntent::SIZE, size_of::<EncodedOrderIntent>());
    }

    #[test]
    fn every_flag_owns_a_distinct_bit() {
        let byte = |flags: Flags| <[u8; 1]>::from(flags)[0];
        let cleared = Flags {
            kind: OrderKind::Sell,
            partially_fillable: false,
        };
        assert_eq!(byte(cleared), 0);

        let set_one_by_one = [
            (
                Flags::PARTIALLY_FILLABLE,
                Flags {
                    partially_fillable: true,
                    ..cleared
                },
            ),
            (
                Flags::KIND,
                Flags {
                    kind: OrderKind::Buy,
                    ..cleared
                },
            ),
        ];
        let mut seen = 0u8;
        for (bit, flags) in set_one_by_one {
            assert_eq!(bit.count_ones(), 1, "a flag must occupy a single bit");
            assert_eq!(seen & bit, 0, "two flags must not share a bit");
            assert!(
                seen == 0 || bit < seen,
                "each flag must be less significant than the ones before it"
            );
            seen |= bit;
            assert_eq!(byte(flags), bit);
        }
        assert_eq!(seen, Flags::DEFINED);
    }

    #[test]
    fn roundtrip_all_kind_and_flag_combinations() {
        for intent in all_flag_shapes() {
            let encoded = EncodedOrderIntent::from(&intent);
            let (decoded, _uid) =
                EncodedOrderIntent::decode_and_hash(&encoded).expect("example must decode");
            assert_eq!(decoded, intent);
        }
    }

    // Locks the bijection invariant called out in `decode_and_hash`: the
    // UID computed over the raw input bytes must equal the hash of the
    // canonical re-encoding. If anything ever normalizes during
    // encode/decode, this test fails.
    #[test]
    fn decode_and_hash_uid_matches_encoded_hash() {
        for intent in all_flag_shapes() {
            let encoded = EncodedOrderIntent::from(&intent);
            let (_intent, uid) =
                EncodedOrderIntent::decode_and_hash(&encoded).expect("example must decode");
            assert_eq!(uid, encoded.hash());
        }
    }

    #[test]
    fn sanity_check_offsets() {
        fn first_differing_byte(lhs: &[u8], rhs: &[u8]) -> Option<usize> {
            lhs.iter().zip(rhs).position(|(l, r)| l != r)
        }
        let sell_false: EncodedOrderIntent = (&sample_intent(Flags {
            kind: OrderKind::Sell,
            partially_fillable: false,
        }))
            .into();
        let sell_true: EncodedOrderIntent = (&sample_intent(Flags {
            kind: OrderKind::Sell,
            partially_fillable: true,
        }))
            .into();
        let buy_true: EncodedOrderIntent = (&sample_intent(Flags {
            kind: OrderKind::Buy,
            partially_fillable: true,
        }))
            .into();

        assert_eq!(
            first_differing_byte(sell_false.as_slice(), sell_true.as_slice())
                .expect("should have different flags byte"),
            FLAGS_OFFSET
        );
        assert_eq!(
            first_differing_byte(buy_true.as_slice(), sell_true.as_slice())
                .expect("should have different flags byte"),
            FLAGS_OFFSET
        );
    }

    #[test]
    fn decode_accepts_defined_flag_bits_only() {
        let encoded = EncodedOrderIntent::from(&sample_intent(Default::default()));
        let mut bytes: [u8; EncodedOrderIntent::SIZE] = *encoded;
        for flags in u8::MIN..=u8::MAX {
            bytes[FLAGS_OFFSET] = flags;
            let decoded = EncodedOrderIntent::decode_and_hash(&bytes);
            if flags & !Flags::DEFINED != 0 {
                assert_eq!(
                    decoded.err(),
                    Some(ProgramError::InvalidInstructionData),
                    "flags {flags:#04x} sets a reserved bit and must be rejected",
                );
            }
        }
    }

    #[test]
    fn uid_digest_regression() {
        let intent = sample_intent(Flags {
            kind: OrderKind::Buy,
            partially_fillable: true,
        });
        let expected = hex!("d2a82e919ec3d5e8b21c512cf14251e98bf79cdf01f0a2bdd0ecbed3007a9761");
        assert_eq!(intent.uid(), Hash::from(expected));
    }

    #[test]
    fn encoding_regression() {
        let encoded = EncodedOrderIntent::from(&sample_intent(Flags {
            kind: OrderKind::Buy,
            partially_fillable: true,
        }));
        let encoding: [u8; EncodedOrderIntent::SIZE] = *encoded;
        #[rustfmt::skip]
        let expected: [u8; EncodedOrderIntent::SIZE] = [
            // owner ([0x11; 32])
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
            // buy_token_account ([0x22; 32])
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            // sell_token_account ([0x33; 32])
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            // sell_amount (0x0123_4567_89ab_cdef, LE u64)
            0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01,
            // buy_amount (0xfedc_ba98_7654_3210, LE u64)
            0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe,
            // valid_to (0xdead_beef, LE u32)
            0xef, 0xbe, 0xad, 0xde,
            // flags (partially_fillable | kind (Buy = 1))
            0b00000011,
            // app_data ([0x44; 32])
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        ];
        assert_eq!(encoding, expected);
    }

    // Property-based tests, non-deterministic.
    mod proptest {
        use ::proptest::{prelude::*, test_runner::TestCaseError};

        use super::*;
        use crate::data::intent::fixtures::{
            arb_flags_byte, arb_invalid_flags_byte, arb_order_intent, FLAGS_OFFSET,
        };

        proptest! {
            // For any `OrderIntent`, encoding an intent into an encoded
            // intent and then decoding it with `decode_and_hash()` returns
            // the same intent plus a UID that matches the encoded bytes'
            // hash.
            #[test]
            fn intent_roundtrip(intent in arb_order_intent()) {
                let encoded = EncodedOrderIntent::from(&intent);
                let (decoded, uid) = EncodedOrderIntent::decode_and_hash(&encoded)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(decoded, intent);
                prop_assert_eq!(uid, encoded.hash());
            }

            // For any bytes whose flags slot is valid, `decode_and_hash` and
            // then re-encoding produces back the original bytes.
            #[test]
            fn bytes_roundtrip(
                mut bytes in any::<[u8; EncodedOrderIntent::SIZE]>(),
                flags in arb_flags_byte(),
            ) {
                bytes[FLAGS_OFFSET] = flags;
                let (intent, _uid) = EncodedOrderIntent::decode_and_hash(&bytes)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(*EncodedOrderIntent::from(&intent), bytes);
            }

            // Symmetric: any bytes whose flags byte carries a reserved bit
            // return `InvalidInstructionData`.
            #[test]
            fn rejects_reserved_flag_bits(
                mut bytes in any::<[u8; EncodedOrderIntent::SIZE]>(),
                bad_flags in arb_invalid_flags_byte(),
            ) {
                bytes[FLAGS_OFFSET] = bad_flags;
                prop_assert_eq!(
                    EncodedOrderIntent::decode_and_hash(&bytes),
                    Err(ProgramError::InvalidInstructionData),
                );
            }
        }
    }
}
