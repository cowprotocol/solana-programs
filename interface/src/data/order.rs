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

use derive_more::Deref;
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
#[derive(Clone, Debug, Deref, Eq, PartialEq)]
pub struct EncodedOrderAccount([u8; Self::SIZE]);

impl EncodedOrderAccount {
    pub const OFF_CANCELLED: usize = 0;
    pub const OFF_AMOUNT_WITHDRAWN: usize = 1;
    pub const OFF_AMOUNT_RECEIVED: usize = 9;
    pub const OFF_CREATED_BY: usize = 17;
    pub const OFF_INTENT: usize = 49;

    pub const SIZE: usize = 199;

    /// Canonical bytes for a freshly created order: not cancelled, no fills
    /// yet, with the given `created_by` pubkey and the given encoded intent
    /// payload. The intent bytes must be a canonical [`EncodedOrderIntent`]
    /// encoding: validation is the caller's responsibility.
    pub fn init(created_by: &Pubkey, intent: &[u8; EncodedOrderIntent::SIZE]) -> Self {
        let mut out = [0u8; Self::SIZE];
        // cancelled / amount_withdrawn / amount_received start at zero.
        out[Self::OFF_CREATED_BY..Self::OFF_INTENT].copy_from_slice(created_by.as_ref());
        out[Self::OFF_INTENT..Self::SIZE].copy_from_slice(intent);
        Self(out)
    }
}

impl From<EncodedOrderAccount> for [u8; EncodedOrderAccount::SIZE] {
    fn from(encoded: EncodedOrderAccount) -> Self {
        encoded.0
    }
}

impl From<OrderAccount> for EncodedOrderAccount {
    fn from(account: OrderAccount) -> Self {
        let mut out = [0u8; Self::SIZE];
        out[Self::OFF_CANCELLED] = account.cancelled as u8;
        out[Self::OFF_AMOUNT_WITHDRAWN..Self::OFF_AMOUNT_RECEIVED]
            .copy_from_slice(&account.amount_withdrawn.to_be_bytes());
        out[Self::OFF_AMOUNT_RECEIVED..Self::OFF_CREATED_BY]
            .copy_from_slice(&account.amount_received.to_be_bytes());
        out[Self::OFF_CREATED_BY..Self::OFF_INTENT].copy_from_slice(account.created_by.as_ref());
        let intent_encoded = EncodedOrderIntent::from(&account.intent);
        out[Self::OFF_INTENT..Self::SIZE].copy_from_slice(intent_encoded.as_slice());
        Self(out)
    }
}

impl TryFrom<[u8; EncodedOrderAccount::SIZE]> for OrderAccount {
    type Error = ProgramError;

    fn try_from(bytes: [u8; EncodedOrderAccount::SIZE]) -> Result<Self, Self::Error> {
        fn cancelled(byte: u8) -> Result<bool, ProgramError> {
            match byte {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(ProgramError::InvalidAccountData),
            }
        }
        fn intent(intent_bytes: [u8; 150]) -> Result<OrderIntent, ProgramError> {
            OrderIntent::try_from(&intent_bytes).map_err(|_| ProgramError::InvalidAccountData)
        }

        // Pull a fixed-size byte array out of `bytes` between `$start` and
        // `$end`. The width is inferred from the caller's target type
        // (e.g. `[u8; 8]` for a u64 slot, `[u8; 32]` for a pubkey slot).
        // Offset constants pin the layout, so the slice has the expected
        // width at compile time and the `try_into` cannot fail.
        macro_rules! field_at {
            ($start:expr, $end:expr) => {
                bytes[$start..$end]
                    .try_into()
                    .expect("offset constants pin the slice to the field's width")
            };
        }

        Ok(OrderAccount {
            cancelled: cancelled(bytes[EncodedOrderAccount::OFF_CANCELLED])?,
            amount_withdrawn: u64::from_be_bytes(field_at!(
                EncodedOrderAccount::OFF_AMOUNT_WITHDRAWN,
                EncodedOrderAccount::OFF_AMOUNT_RECEIVED
            )),
            amount_received: u64::from_be_bytes(field_at!(
                EncodedOrderAccount::OFF_AMOUNT_RECEIVED,
                EncodedOrderAccount::OFF_CREATED_BY
            )),
            created_by: Pubkey::new_from_array(field_at!(
                EncodedOrderAccount::OFF_CREATED_BY,
                EncodedOrderAccount::OFF_INTENT
            )),
            intent: intent(field_at!(
                EncodedOrderAccount::OFF_INTENT,
                EncodedOrderAccount::SIZE
            ))?,
        })
    }
}

impl TryFrom<EncodedOrderAccount> for OrderAccount {
    type Error = ProgramError;

    fn try_from(encoded: EncodedOrderAccount) -> Result<Self, Self::Error> {
        OrderAccount::try_from(encoded.0)
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use crate::data::intent::{tests::sample_intent, OrderKind};

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

    // Pin the layout: every consecutive offset gap must equal the width of
    // the field it represents, and the final field plus its size must land
    // exactly at `SIZE`. Catches a field reorder or a size change in any
    // CI run.
    #[test]
    fn layout_offsets_match_field_sizes() {
        assert_eq!(
            EncodedOrderAccount::OFF_AMOUNT_WITHDRAWN - EncodedOrderAccount::OFF_CANCELLED,
            size_of::<bool>()
        );
        assert_eq!(
            EncodedOrderAccount::OFF_AMOUNT_RECEIVED - EncodedOrderAccount::OFF_AMOUNT_WITHDRAWN,
            size_of::<u64>()
        );
        assert_eq!(
            EncodedOrderAccount::OFF_CREATED_BY - EncodedOrderAccount::OFF_AMOUNT_RECEIVED,
            size_of::<u64>()
        );
        assert_eq!(
            EncodedOrderAccount::OFF_INTENT - EncodedOrderAccount::OFF_CREATED_BY,
            size_of::<Pubkey>()
        );
        assert_eq!(
            EncodedOrderAccount::SIZE - EncodedOrderAccount::OFF_INTENT,
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

    #[test]
    fn decode_rejects_non_boolean_cancelled() {
        let mut bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();
        for bad in 0x02u8..=0xff {
            bytes[EncodedOrderAccount::OFF_CANCELLED] = bad;
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
        let kind_offset = EncodedOrderAccount::OFF_INTENT + EncodedOrderIntent::OFF_KIND;
        bytes[kind_offset] = 0x02;
        let err = OrderAccount::try_from(bytes)
            .expect_err("an invalid intent kind byte must propagate as a decode failure");
        assert_eq!(err, ProgramError::InvalidAccountData);
    }

    #[test]
    fn init_matches_from_order_account() {
        let intent = sample_intent(OrderKind::Sell, false);
        let created_by = Pubkey::new_from_array([0x42u8; 32]);

        let direct = EncodedOrderAccount::init(
            &created_by,
            &<[u8; EncodedOrderIntent::SIZE]>::from(&EncodedOrderIntent::from(&intent)),
        );
        let via_order_account = EncodedOrderAccount::from(OrderAccount {
            cancelled: false,
            amount_withdrawn: 0,
            amount_received: 0,
            created_by,
            intent,
        });

        assert_eq!(direct, via_order_account);
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
                bytes[EncodedOrderAccount::OFF_CANCELLED] = cancelled as u8;
                bytes[EncodedOrderAccount::OFF_INTENT + EncodedOrderIntent::OFF_KIND] = kind as u8;
                bytes[EncodedOrderAccount::OFF_INTENT + EncodedOrderIntent::OFF_PARTIALLY_FILLABLE] =
                    partially_fillable as u8;
                let account = OrderAccount::try_from(bytes)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(*EncodedOrderAccount::from(account), bytes);
            }
        }
    }
}
