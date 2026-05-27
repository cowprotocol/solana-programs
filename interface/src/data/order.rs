//! Order PDA bodies and their canonical byte representation.
//!
//! The settlement program stores each authenticated order in a dedicated
//! program-derived account. That account's data area is laid out here.
//!
//! Two types live in this module:
//!
//! - [`OrderAccount`] is the idiomatic Rust representation. Every value is
//!   valid by construction: `cancelled` is a `bool`, `intent` is a fully
//!   decoded [`OrderIntent`].
//! - [`EncodedOrderAccount`] is its canonical byte representation, that is,
//!   the exact bytes written to/read from the PDA.
//!
//! Conversion is asymmetric: [`EncodedOrderAccount`]`::from(OrderAccount)`
//! is infallible; decoding raw bytes via [`OrderAccount`]`::try_from`
//! returns `Result` and rejects an out-of-range `cancelled` byte or any
//! intent byte the intent decoder rejects. There is no path that produces
//! an [`OrderAccount`] whose `cancelled` byte or `intent` slot was not
//! validated.

use core::mem::size_of;
use core::ops::Deref;

use arrayref::{array_refs, mut_array_refs};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::data::intent::{EncodedOrderIntent, OrderIntent};

/// Idiomatic representation of an order PDA's body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderAccount {
    /// `false` = the order is still active and can be filled; `true` = the
    /// order has been cancelled by the owner and must not be filled.
    pub cancelled: bool,

    /// Cumulative amount of the sell token withdrawn for this order
    /// across settlements. Starts at zero; monotonically increases.
    pub amount_withdrawn: u64,

    /// Cumulative amount of the buy token received for this order
    /// across settlements. Starts at zero; monotonically increases.
    pub amount_received: u64,

    /// Account that funded the rent for this PDA. The rent is refunded
    /// here when the order is cleared.
    pub created_by: Pubkey,

    /// The order intent stored in this PDA.
    pub intent: OrderIntent,
}

/// Canonical 199-byte representation of an [`OrderAccount`]. The bytes
/// written to/read from the order PDA's data area.
///
/// Layout: one character per byte, cell widths proportional to field size,
/// each divider belongs to the cell on its right. Integers are big-endian.
/// The intent slot holds a verbatim [`EncodedOrderIntent`]; see that
/// type's docs for its inner layout.
///
/// ```text
/// ┌──── cancelled
/// ┌┬───────┬───────┬───────────────────────────────┬─────────────────...─────────────────┐
/// ││amount_│amount_│                               │                                     │
/// ││with-  │re-    │           created_by          │     intent (EncodedOrderIntent)     │
/// ││drawn  │ceived │                               │                                     │
/// └┴───────┴───────┴───────────────────────────────┴─────────────────...─────────────────┘
/// 0 1      9       17                              49                ...                 199
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedOrderAccount([u8; Self::SIZE]);

impl EncodedOrderAccount {
    // Per-field widths, derived from the `OrderAccount` field types.
    const W_CANCELLED: usize = size_of::<bool>();
    const W_AMOUNT_WITHDRAWN: usize = size_of::<u64>();
    const W_AMOUNT_RECEIVED: usize = size_of::<u64>();
    const W_CREATED_BY: usize = size_of::<Pubkey>();
    const W_INTENT: usize = EncodedOrderIntent::SIZE;

    pub const SIZE: usize = 199;
}

impl From<EncodedOrderAccount> for [u8; EncodedOrderAccount::SIZE] {
    fn from(encoded: EncodedOrderAccount) -> Self {
        encoded.0
    }
}

impl From<OrderAccount> for EncodedOrderAccount {
    fn from(account: OrderAccount) -> Self {
        let mut out = [0u8; Self::SIZE];
        let (cancelled, amount_withdrawn, amount_received, created_by, intent) = mut_array_refs![
            &mut out,
            EncodedOrderAccount::W_CANCELLED,
            EncodedOrderAccount::W_AMOUNT_WITHDRAWN,
            EncodedOrderAccount::W_AMOUNT_RECEIVED,
            EncodedOrderAccount::W_CREATED_BY,
            EncodedOrderAccount::W_INTENT
        ];
        *cancelled = [account.cancelled as u8];
        *amount_withdrawn = account.amount_withdrawn.to_be_bytes();
        *amount_received = account.amount_received.to_be_bytes();
        *created_by = account.created_by.to_bytes();
        *intent = *EncodedOrderIntent::from(&account.intent);
        Self(out)
    }
}

impl TryFrom<[u8; EncodedOrderAccount::SIZE]> for OrderAccount {
    type Error = ProgramError;

    fn try_from(bytes: [u8; EncodedOrderAccount::SIZE]) -> Result<Self, Self::Error> {
        let (cancelled, amount_withdrawn, amount_received, created_by, intent) = array_refs![
            &bytes,
            EncodedOrderAccount::W_CANCELLED,
            EncodedOrderAccount::W_AMOUNT_WITHDRAWN,
            EncodedOrderAccount::W_AMOUNT_RECEIVED,
            EncodedOrderAccount::W_CREATED_BY,
            EncodedOrderAccount::W_INTENT
        ];

        Ok(OrderAccount {
            cancelled: match cancelled {
                [0] => false,
                [1] => true,
                _ => return Err(ProgramError::InvalidAccountData),
            },
            amount_withdrawn: u64::from_be_bytes(*amount_withdrawn),
            amount_received: u64::from_be_bytes(*amount_received),
            created_by: Pubkey::new_from_array(*created_by),
            intent: OrderIntent::try_from(intent).map_err(|_| ProgramError::InvalidAccountData)?,
        })
    }
}

impl TryFrom<EncodedOrderAccount> for OrderAccount {
    type Error = ProgramError;

    fn try_from(encoded: EncodedOrderAccount) -> Result<Self, Self::Error> {
        OrderAccount::try_from(encoded.0)
    }
}

impl Deref for EncodedOrderAccount {
    type Target = [u8; Self::SIZE];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use crate::data::intent::{
        tests::{sample_intent, KIND_OFFSET, PARTIALLY_FILLABLE_OFFSET},
        OrderKind,
    };

    use super::*;

    fn sample_account(cancelled: bool) -> OrderAccount {
        OrderAccount {
            cancelled,
            amount_withdrawn: 0x0112_2334_4556_6778,
            amount_received: 0x899a_abbc_cdde_eff0,
            created_by: Pubkey::new_from_array([0x43; 32]),
            intent: sample_intent(OrderKind::Sell, false),
        }
    }

    // Pin each width to the size of the `OrderAccount` field it encodes. The
    // widths summing to `SIZE` is enforced separately, at compile time, by the
    // `array_refs!` / `mut_array_refs!` invocations in the codec.
    #[test]
    fn widths_match_field_sizes() {
        use core::mem::size_of_val;

        // Any `OrderAccount` works: `size_of_val` only consults the field
        // type, never the data.
        let OrderAccount {
            cancelled,
            amount_withdrawn,
            amount_received,
            created_by,
            // `OrderAccount` decodes the intent, but the encoded order uses
            // `EncodedOrderIntent`, not `OrderIntent`.
            intent: _intent,
        } = sample_account(false);

        assert_eq!(EncodedOrderAccount::W_CANCELLED, size_of_val(&cancelled));
        assert_eq!(
            EncodedOrderAccount::W_AMOUNT_WITHDRAWN,
            size_of_val(&amount_withdrawn)
        );
        assert_eq!(
            EncodedOrderAccount::W_AMOUNT_RECEIVED,
            size_of_val(&amount_received)
        );
        assert_eq!(EncodedOrderAccount::W_CREATED_BY, size_of_val(&created_by));

        assert_eq!(
            EncodedOrderAccount::W_INTENT,
            size_of::<EncodedOrderIntent>()
        );
    }

    #[test]
    fn roundtrip_both_cancelled_states() {
        for cancelled in [false, true] {
            let account = sample_account(cancelled);
            let encoded = EncodedOrderAccount::from(account.clone());
            let decoded = OrderAccount::try_from(encoded).expect("example must decode");
            assert_eq!(decoded, account);
        }
    }

    // Hardcoded but verified in a sanity-check test.
    const CANCELLED_OFFSET: usize = 0;
    const INTENT_OFFSET: usize = 49;

    #[test]
    fn sanity_check_offsets() {
        fn first_differing_byte(lhs: &[u8], rhs: &[u8]) -> Option<usize> {
            lhs.iter()
                .zip(rhs)
                .enumerate()
                .find(|(_, (l, r))| l != r)
                .map(|(i, _)| i)
        }

        let mut sample_account_base = sample_account(false);
        let base: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account_base.clone()).into();
        let cancelled: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(true)).into();
        assert_eq!(
            first_differing_byte(&base, &cancelled).expect("should differ in the cancelled byte"),
            CANCELLED_OFFSET
        );

        // Differs only in the embedded intent.
        let encoded_intent: [u8; EncodedOrderIntent::SIZE] =
            (&EncodedOrderIntent::from(&sample_account_base.intent)).into();
        // Hack: xoring each byte makes sure all bytes are different.
        // In general, it isn't guaranteed that the result encodes to a
        // valid intent, but in this case we know it because the only bytes
        // that may fail decoding are `kind` and `partially_fillable`, both
        // of which stay valid if flipped with `^0x01`.
        let bitwise_different_encoded_intent: [u8; EncodedOrderIntent::SIZE] =
            encoded_intent.map(|b| b ^ 0x01);
        sample_account_base.intent =
            OrderIntent::try_from(&bitwise_different_encoded_intent).expect("hack should work");
        let changed_intent: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account_base).into();
        assert_eq!(
            first_differing_byte(&base, &changed_intent).expect("should differ in the intent slot"),
            INTENT_OFFSET
        );
    }

    #[test]
    fn decode_rejects_non_boolean_cancelled() {
        let mut bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();
        for bad in 0x02u8..=0xff {
            bytes[CANCELLED_OFFSET] = bad;
            let err = OrderAccount::try_from(bytes)
                .expect_err("non-boolean cancelled byte must be rejected");
            assert_eq!(err, ProgramError::InvalidAccountData);
        }
    }

    #[test]
    fn decode_propagates_invalid_intent() {
        let mut bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();
        // Corrupt the `kind` byte inside the intent slot: the intent
        // decoder rejects it and the order-account decode surfaces that
        // failure as `InvalidAccountData`.
        let kind_offset = INTENT_OFFSET + KIND_OFFSET;
        bytes[kind_offset] = 0x02;
        let err = OrderAccount::try_from(bytes)
            .expect_err("an invalid intent kind byte must propagate as a decode failure");
        assert_eq!(err, ProgramError::InvalidAccountData);
    }

    // Property-based tests, non-deterministic.
    mod proptest {
        use ::proptest::{prelude::*, test_runner::TestCaseError};

        use crate::data::intent::tests::proptest::{arb_order_intent, arb_order_kind};

        use super::*;

        // Any valid `OrderAccount`.
        fn arb_order_account() -> impl Strategy<Value = OrderAccount> {
            (
                any::<bool>(),
                any::<u64>(),
                any::<u64>(),
                any::<[u8; 32]>(),
                arb_order_intent(),
            )
                .prop_map(
                    |(cancelled, amount_withdrawn, amount_received, created_by, intent)| {
                        OrderAccount {
                            cancelled,
                            amount_withdrawn,
                            amount_received,
                            created_by: Pubkey::new_from_array(created_by),
                            intent,
                        }
                    },
                )
        }

        proptest! {
            // For any `OrderAccount`, encode then decode returns the same
            // account.
            #[test]
            fn account_roundtrip(account in arb_order_account()) {
                let encoded = EncodedOrderAccount::from(account.clone());
                let decoded = OrderAccount::try_from(encoded)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(decoded, account);
            }

            // For any bytes whose `cancelled` and embedded intent
            // discriminants are valid, decode + re-encode produces the
            // same bytes back.
            #[test]
            fn bytes_roundtrip(
                mut bytes in any::<[u8; EncodedOrderAccount::SIZE]>(),
                cancelled in any::<bool>(),
                kind in arb_order_kind(),
                partially_fillable in any::<bool>(),
            ) {
                bytes[CANCELLED_OFFSET] = cancelled as u8;
                bytes[INTENT_OFFSET + KIND_OFFSET] = kind as u8;
                bytes[INTENT_OFFSET + PARTIALLY_FILLABLE_OFFSET] = partially_fillable as u8;
                let account = OrderAccount::try_from(bytes)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(*EncodedOrderAccount::from(account), bytes);
            }
        }
    }
}
