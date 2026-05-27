//! Order intents and their canonical byte representation.
//!
//! Two types live here:
//!
//! - [`OrderIntent`] is the idiomatic Rust representation. Every value is valid
//!   by construction: `kind` is an [`OrderKind`] enum, `partially_fillable` is
//!   a `bool`. Callers pattern-match on it directly.
//! - [`EncodedOrderIntent`] is its canonical byte representation: the only
//!   thing sent on the wire and also the data encoding used to generate the
//!   order UID.
//!
//! Conversion is asymmetric: [`EncodedOrderIntent`]`::from(OrderIntent)` is
//! infallible; decoding raw bytes via [`OrderIntent`]`::try_from` returns
//! `Result` and rejects out-of-range `kind` or `partially_fillable` bytes up
//! front. There is no path that produces an `OrderIntent` whose `kind` byte or
//! `partially_fillable` byte was not validated.

use core::mem::size_of;
use core::ops::Deref;

use arrayref::{array_refs, mut_array_refs};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

/// Direction of the trade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OrderKind {
    Sell = 0,
    Buy = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

    /// Whether `sell_amount` or `buy_amount` is the exact figure; the
    /// other side is treated as the limit (minimum to receive for `Sell`,
    /// maximum to spend for `Buy`).
    pub kind: OrderKind,

    /// If `true`, the order may be filled across multiple settlements;
    /// proceeds and consumption scale proportionally with the amount of
    /// the sell side that's been used. If `false`, a single settlement
    /// must consume the full sell amount (fill-or-kill).
    pub partially_fillable: bool,

    /// Opaque 32 bytes set by the order creator. Not interpreted by the
    /// settlement program; used off-chain for metadata such as the
    /// frontend version, slippage hints, or attribution.
    pub app_data: [u8; 32],
}

/// Canonical 150-byte representation of an [`OrderIntent`]. The wire format and
/// the order UID preimage.
///
/// Layout: one character per byte, cell widths proportional to field size,
/// each divider belongs to the cell on its right. The byte range is
/// annotated below. Amounts and `valid_to` are big-endian encoded.
///
/// ```text
///                                                                                              partially_fillable ─────┐
///                                                                                                            kind ────┐│
/// ┌───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────┬───────┬───┬┬┬───────────────────────────────┐
/// │                               │                               │                               │sell_  │buy_   │val│││                               │
/// │             owner             │       buy_token_account       │       sell_token_account      │       │       │id_│││            app_data           │
/// │                               │                               │                               │amount │amount │to │││                               │
/// └───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────┴───────┴───┴┴┴───────────────────────────────┘
/// 0                               32                              64                              96      104    112 116 118                            150
///                                                                                                                     117
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedOrderIntent([u8; Self::SIZE]);

impl EncodedOrderIntent {
    // Per-field widths, derived from the `OrderIntent` field types.
    const W_OWNER: usize = size_of::<Pubkey>();
    const W_BUY_TOKEN: usize = size_of::<Pubkey>();
    const W_SELL_TOKEN: usize = size_of::<Pubkey>();
    const W_SELL_AMOUNT: usize = size_of::<u64>();
    const W_BUY_AMOUNT: usize = size_of::<u64>();
    const W_VALID_TO: usize = size_of::<u32>();
    const W_KIND: usize = size_of::<OrderKind>();
    const W_PARTIALLY_FILLABLE: usize = size_of::<bool>();
    const W_APP_DATA: usize = size_of::<[u8; 32]>();

    pub const SIZE: usize = 150;

    /// Canonical hash of the bytes.
    pub fn hash(&self) -> [u8; 32] {
        solana_sha256_hasher::hashv(&[self.as_slice()]).to_bytes()
    }

    /// Decode raw bytes to an [`OrderIntent`] and compute the UID in one shot.
    /// Returns [`ProgramError::InvalidInstructionData`] for an out-of-range
    /// `kind` or `partially_fillable` byte; every other byte combination
    /// decodes.
    pub fn decode_and_hash(
        bytes: &[u8; Self::SIZE],
    ) -> Result<(OrderIntent, [u8; 32]), ProgramError> {
        let intent = OrderIntent::try_from(bytes)?;
        // The UID is the SHA-256 of the input bytes. Hashing the input
        // (no re-encode) is correct because encode/decode is a bijection on
        // inputs that pass validation. Any normalization added to the `From`
        // or `TryFrom` impls later would break this and the UID would silently
        // diverge from `OrderIntent::uid()`.
        let uid = solana_sha256_hasher::hashv(&[bytes.as_slice()]).to_bytes();
        Ok((intent, uid))
    }
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
        let (
            owner,
            buy_token,
            sell_token,
            sell_amount,
            buy_amount,
            valid_to,
            kind,
            partially_fillable,
            app_data,
        ) = mut_array_refs![
            &mut out,
            EncodedOrderIntent::W_OWNER,
            EncodedOrderIntent::W_BUY_TOKEN,
            EncodedOrderIntent::W_SELL_TOKEN,
            EncodedOrderIntent::W_SELL_AMOUNT,
            EncodedOrderIntent::W_BUY_AMOUNT,
            EncodedOrderIntent::W_VALID_TO,
            EncodedOrderIntent::W_KIND,
            EncodedOrderIntent::W_PARTIALLY_FILLABLE,
            EncodedOrderIntent::W_APP_DATA
        ];
        *owner = intent.owner.to_bytes();
        *buy_token = intent.buy_token_account.to_bytes();
        *sell_token = intent.sell_token_account.to_bytes();
        *sell_amount = intent.sell_amount.to_be_bytes();
        *buy_amount = intent.buy_amount.to_be_bytes();
        *valid_to = intent.valid_to.to_be_bytes();
        *kind = [intent.kind as u8];
        *partially_fillable = [intent.partially_fillable as u8];
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

        let (
            owner,
            buy_token,
            sell_token,
            sell_amount,
            buy_amount,
            valid_to,
            kind,
            partially_fillable,
            app_data,
        ) = array_refs![
            bytes,
            EncodedOrderIntent::W_OWNER,
            EncodedOrderIntent::W_BUY_TOKEN,
            EncodedOrderIntent::W_SELL_TOKEN,
            EncodedOrderIntent::W_SELL_AMOUNT,
            EncodedOrderIntent::W_BUY_AMOUNT,
            EncodedOrderIntent::W_VALID_TO,
            EncodedOrderIntent::W_KIND,
            EncodedOrderIntent::W_PARTIALLY_FILLABLE,
            EncodedOrderIntent::W_APP_DATA
        ];

        Ok(OrderIntent {
            owner: Pubkey::new_from_array(*owner),
            buy_token_account: Pubkey::new_from_array(*buy_token),
            sell_token_account: Pubkey::new_from_array(*sell_token),
            sell_amount: u64::from_be_bytes(*sell_amount),
            buy_amount: u64::from_be_bytes(*buy_amount),
            valid_to: u32::from_be_bytes(*valid_to),
            kind: match kind {
                [0] => OrderKind::Sell,
                [1] => OrderKind::Buy,
                _ => return Err(ProgramError::InvalidInstructionData),
            },
            partially_fillable: match partially_fillable {
                [0] => false,
                [1] => true,
                _ => return Err(ProgramError::InvalidInstructionData),
            },
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

impl Deref for EncodedOrderIntent {
    type Target = [u8; Self::SIZE];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl OrderIntent {
    /// SHA-256 of the canonical bytes. Doubles as the order UID and the
    /// middle seed of the order PDA. On SBF this compiles to a single
    /// `sol_sha256` syscall; off-target it goes through the `sha2` crate.
    pub fn uid(&self) -> [u8; 32] {
        EncodedOrderIntent::from(self).hash()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ORDER_KINDS: [OrderKind; 2] = [OrderKind::Sell, OrderKind::Buy];

    // Full Cartesian product of `OrderKind × bool` for tests that need to
    // exercise every shape an `OrderIntent` can take on these axes.
    fn all_kind_and_fillable() -> impl Iterator<Item = (OrderKind, bool)> {
        ALL_ORDER_KINDS
            .into_iter()
            .flat_map(|kind| core::iter::repeat(kind).zip([false, true]))
    }

    // Hand-picked example used for both the roundtrip and the digest
    // regression. Distinct pubkeys, non-zero amounts, `valid_to` with both
    // halves set, recognizable `app_data` pattern.
    fn default_order_intent(kind: OrderKind, partially_fillable: bool) -> OrderIntent {
        OrderIntent {
            owner: Pubkey::new_from_array([0x11; 32]),
            buy_token_account: Pubkey::new_from_array([0x22; 32]),
            sell_token_account: Pubkey::new_from_array([0x33; 32]),
            sell_amount: 0x0123_4567_89ab_cdef,
            buy_amount: 0xfedc_ba98_7654_3210,
            valid_to: 0xdead_beef,
            kind,
            partially_fillable,
            app_data: [0x44; 32],
        }
    }

    // Pin each width to the size of the `OrderIntent` field it encodes. The
    // widths summing to `SIZE` is enforced separately, at compile time, by the
    // `array_refs!` / `mut_array_refs!` invocations in the codec.
    #[test]
    fn widths_match_field_sizes() {
        use core::mem::size_of_val;

        // Any `OrderIntent` works: `size_of_val` only consults the field
        // type, never the data.
        let i = default_order_intent(OrderKind::Sell, false);

        assert_eq!(EncodedOrderIntent::W_OWNER, size_of_val(&i.owner));
        assert_eq!(
            EncodedOrderIntent::W_BUY_TOKEN,
            size_of_val(&i.buy_token_account)
        );
        assert_eq!(
            EncodedOrderIntent::W_SELL_TOKEN,
            size_of_val(&i.sell_token_account)
        );
        assert_eq!(
            EncodedOrderIntent::W_SELL_AMOUNT,
            size_of_val(&i.sell_amount)
        );
        assert_eq!(EncodedOrderIntent::W_BUY_AMOUNT, size_of_val(&i.buy_amount));
        assert_eq!(EncodedOrderIntent::W_VALID_TO, size_of_val(&i.valid_to));
        assert_eq!(EncodedOrderIntent::W_KIND, size_of_val(&i.kind));
        assert_eq!(
            EncodedOrderIntent::W_PARTIALLY_FILLABLE,
            size_of_val(&i.partially_fillable)
        );
        assert_eq!(EncodedOrderIntent::W_APP_DATA, size_of_val(&i.app_data));
    }

    #[test]
    fn roundtrip_all_kind_and_bool_combinations() {
        for (kind, partially_fillable) in all_kind_and_fillable() {
            let intent = default_order_intent(kind, partially_fillable);
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
        for (kind, partially_fillable) in all_kind_and_fillable() {
            let encoded = EncodedOrderIntent::from(&default_order_intent(kind, partially_fillable));
            let (_intent, uid) =
                EncodedOrderIntent::decode_and_hash(&encoded).expect("example must decode");
            assert_eq!(uid, encoded.hash());
        }
    }

    // Hardcoded but verified in a sanity-check test.
    const KIND_OFFSET: usize = 116;
    const PARTIALLY_FILLABLE_OFFSET: usize = KIND_OFFSET + EncodedOrderIntent::W_KIND;

    #[test]
    fn sanity_check_offsets() {
        fn first_differing_byte(lhs: &[u8], rhs: &[u8]) -> Option<usize> {
            lhs.iter()
                .zip(rhs)
                .enumerate()
                .find(|(_, (l, r))| l != r)
                .map(|(i, _)| i)
        }
        let sell_false: EncodedOrderIntent = (&default_order_intent(OrderKind::Sell, false)).into();
        let sell_true: EncodedOrderIntent = (&default_order_intent(OrderKind::Sell, true)).into();
        let buy_true: EncodedOrderIntent = (&default_order_intent(OrderKind::Buy, true)).into();

        assert_eq!(
            first_differing_byte(sell_false.as_slice(), sell_true.as_slice())
                .expect("should have different partially fillable byte"),
            PARTIALLY_FILLABLE_OFFSET
        );
        assert_eq!(
            first_differing_byte(buy_true.as_slice(), sell_true.as_slice())
                .expect("should have different kind byte"),
            KIND_OFFSET
        );
    }

    #[test]
    fn decode_rejects_out_of_range_kind() {
        let encoded = EncodedOrderIntent::from(&default_order_intent(OrderKind::Sell, false));
        let mut bytes: [u8; EncodedOrderIntent::SIZE] = *encoded;
        for bad in 0x02u8..=0xff {
            bytes[KIND_OFFSET] = bad;
            let err = EncodedOrderIntent::decode_and_hash(&bytes)
                .expect_err("should reject out of range kind");
            assert_eq!(err, ProgramError::InvalidInstructionData);
        }
    }

    #[test]
    fn decode_rejects_non_boolean_partially_fillable() {
        let encoded = EncodedOrderIntent::from(&default_order_intent(OrderKind::Sell, false));
        let mut bytes: [u8; EncodedOrderIntent::SIZE] = *encoded;
        for bad in 0x02u8..=0xff {
            bytes[PARTIALLY_FILLABLE_OFFSET] = bad;
            let err = EncodedOrderIntent::decode_and_hash(&bytes)
                .expect_err("should reject out of range partially fillable");
            assert_eq!(err, ProgramError::InvalidInstructionData);
        }
    }

    #[test]
    fn uid_digest_regression() {
        let intent = default_order_intent(OrderKind::Buy, true);
        let expected: [u8; 32] = [
            0x09, 0x1d, 0x7e, 0x19, 0x59, 0xac, 0x6f, 0x7a, 0x40, 0x0a, 0x91, 0xf1, 0xdc, 0xd9,
            0xce, 0x43, 0x6f, 0x8f, 0x53, 0xe2, 0xb7, 0xa1, 0xd9, 0x68, 0xac, 0xb0, 0x8f, 0x79,
            0xd3, 0xc1, 0x23, 0x1d,
        ];
        assert_eq!(intent.uid(), expected);
    }

    #[test]
    #[rustfmt::skip]
    fn encoding_regression() {
        let encoded = EncodedOrderIntent::from(&default_order_intent(OrderKind::Buy, true));
        let encoding: [u8; EncodedOrderIntent::SIZE] = *encoded;
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
            // sell_amount (0x0123_4567_89ab_cdef, BE u64)
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
            // buy_amount (0xfedc_ba98_7654_3210, BE u64)
            0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10,
            // valid_to (0xdead_beef, BE u32)
            0xde, 0xad, 0xbe, 0xef,
            // kind (Buy = 1)
            0x01,
            // partially_fillable (true = 1)
            0x01,
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
        use ::proptest::{prelude::*, strategy::Union, test_runner::TestCaseError};

        use super::*;

        // Any valid `OrderKind`.
        fn arb_order_kind() -> impl Strategy<Value = OrderKind> {
            Union::new(ALL_ORDER_KINDS.map(Just))
        }

        // Any byte not decoding to a valid order type.
        fn arb_bad_order_kind_byte() -> impl Strategy<Value = u8> {
            2u8..=255
        }

        // Any byte not decoding to a valid bool.
        fn arb_bad_bool_byte() -> impl Strategy<Value = u8> {
            2u8..=255
        }

        // Any valid `OrderIntent`.
        fn arb_order_intent() -> impl Strategy<Value = OrderIntent> {
            (
                any::<[u8; 32]>(),
                any::<[u8; 32]>(),
                any::<[u8; 32]>(),
                any::<u64>(),
                any::<u64>(),
                any::<u32>(),
                arb_order_kind(),
                any::<bool>(),
                any::<[u8; 32]>(),
            )
                .prop_map(
                    |(
                        owner,
                        buy_tok,
                        sell_tok,
                        sell_amount,
                        buy_amount,
                        valid_to,
                        kind,
                        pf,
                        app,
                    )| {
                        OrderIntent {
                            owner: Pubkey::new_from_array(owner),
                            buy_token_account: Pubkey::new_from_array(buy_tok),
                            sell_token_account: Pubkey::new_from_array(sell_tok),
                            sell_amount,
                            buy_amount,
                            valid_to,
                            kind,
                            partially_fillable: pf,
                            app_data: app,
                        }
                    },
                )
        }

        proptest! {
            // For any `OrderIntent`, `encode().decode_and_hash()` returns
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

            // For any bytes whose `kind` and `partially_fillable` slots
            // are valid, `decode_and_hash` + re-`encode` produces back
            // the original bytes.
            #[test]
            fn bytes_roundtrip(
                mut bytes in any::<[u8; EncodedOrderIntent::SIZE]>(),
                kind in arb_order_kind(),
                partially_fillable in any::<bool>(),
            ) {
                bytes[KIND_OFFSET] = kind as u8;
                bytes[PARTIALLY_FILLABLE_OFFSET] = partially_fillable as u8;
                let (intent, _uid) = EncodedOrderIntent::decode_and_hash(&bytes)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(*EncodedOrderIntent::from(&intent), bytes);
            }

            // For any bytes with an invalid `kind` byte (and a valid
            // `partially_fillable`), `decode_and_hash` returns
            // `InvalidInstructionData`.
            #[test]
            fn rejects_invalid_kind_byte(
                mut bytes in any::<[u8; EncodedOrderIntent::SIZE]>(),
                bad_kind in arb_bad_order_kind_byte(),
                partially_fillable in any::<bool>(),
            ) {
                bytes[KIND_OFFSET] = bad_kind;
                bytes[PARTIALLY_FILLABLE_OFFSET] = partially_fillable as u8;
                prop_assert_eq!(
                    EncodedOrderIntent::decode_and_hash(&bytes),
                    Err(ProgramError::InvalidInstructionData),
                );
            }

            // Symmetric: any bytes with an out-of-range
            // `partially_fillable` byte (and a valid `kind`) return
            // `InvalidInstructionData`.
            #[test]
            fn rejects_invalid_partially_fillable_byte(
                mut bytes in any::<[u8; EncodedOrderIntent::SIZE]>(),
                kind in arb_order_kind(),
                bad_pf in arb_bad_bool_byte(),
            ) {
                bytes[KIND_OFFSET] = kind as u8;
                bytes[PARTIALLY_FILLABLE_OFFSET] = bad_pf;
                prop_assert_eq!(
                    EncodedOrderIntent::decode_and_hash(&bytes),
                    Err(ProgramError::InvalidInstructionData),
                );
            }
        }
    }
}
