//! Order account: its byte layout and the zero-copy accessor over it.
//!
//! The settlement program stores each authenticated order in a dedicated
//! program-derived account. That account's data area is laid out here.
//!
//! ```text
//!  ┌───── discriminator
//!  │┌──── bump
//!  ││┌─── cancelled
//!  ┌┬┬┬───────┬───────┬───────────────────────────────┬─────────────────...─────────────────┐
//!  ││││amount_│amount_│                               │                                     │
//!  ││││with-  │re-    │           created_by          │     intent (EncodedOrderIntent)     │
//!  ││││drawn  │ceived │                               │                                     │
//!  └┴┴┴───────┴───────┴───────────────────────────────┴─────────────────...─────────────────┘
//! 0 1 2 3     11      19                              51                ...               264
//! ```
//!
//! [`OrderAccount`] is a zero-copy accessor over those bytes, generic over the
//! borrow backing it (`&[u8]`, `&mut [u8]`): the reads are available for any
//! borrow, the in-place writes only for a mutable one. It reads and updates the
//! account data directly, so the program never copies the account into an owned
//! struct.

use core::mem::size_of;
use core::ops::{Deref, DerefMut};

use arrayref::{array_refs, mut_array_refs};
use solana_account_view::{AccountView, Ref};
use solana_address::Address;
use solana_hash::Hash;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::data::intent::{self, EncodedOrderIntent, OrderIntent, OrderKind};
use crate::pda::is_pda_with_signer_seeds;
use crate::pda::order::order_pda_signer_seeds;
use crate::{SettlementAccount, SettlementError};

/// Single-byte account discriminator at the front of the body.
pub const DISCRIMINATOR: u8 = SettlementAccount::OrderAccount.discriminator();

/// Canonical byte length of an order account's data area.
pub const SIZE: usize = 264;

// Per-field widths, derived from the order account's field types.
const WIDTH_DISCRIMINATOR: usize = size_of::<u8>();
const WIDTH_BUMP: usize = size_of::<u8>();
const WIDTH_CANCELLED: usize = size_of::<bool>();
const WIDTH_AMOUNT_WITHDRAWN: usize = size_of::<u64>();
const WIDTH_AMOUNT_RECEIVED: usize = size_of::<u64>();
const WIDTH_CREATED_BY: usize = size_of::<Pubkey>();
const WIDTH_INTENT: usize = EncodedOrderIntent::SIZE;

/// A borrowed view over an order's bytes, split into named slots so each field
/// can be named. The slots hold raw encoded bytes, not decoded values.
struct OrderSlots<'a> {
    discriminator: &'a [u8; WIDTH_DISCRIMINATOR],
    bump: &'a [u8; WIDTH_BUMP],
    cancelled: &'a [u8; WIDTH_CANCELLED],
    amount_withdrawn: &'a [u8; WIDTH_AMOUNT_WITHDRAWN],
    amount_received: &'a [u8; WIDTH_AMOUNT_RECEIVED],
    created_by: &'a [u8; WIDTH_CREATED_BY],
    intent: &'a [u8; WIDTH_INTENT],
}

/// The mutable counterpart of [`OrderSlots`], for in-place writes.
struct OrderSlotsMut<'a> {
    discriminator: &'a mut [u8; WIDTH_DISCRIMINATOR],
    bump: &'a mut [u8; WIDTH_BUMP],
    cancelled: &'a mut [u8; WIDTH_CANCELLED],
    amount_withdrawn: &'a mut [u8; WIDTH_AMOUNT_WITHDRAWN],
    amount_received: &'a mut [u8; WIDTH_AMOUNT_RECEIVED],
    created_by: &'a mut [u8; WIDTH_CREATED_BY],
    intent: &'a mut [u8; WIDTH_INTENT],
}

/// Split a body into its named slots.
#[inline]
fn order_slots(body: &[u8; SIZE]) -> OrderSlots<'_> {
    let (discriminator, bump, cancelled, amount_withdrawn, amount_received, created_by, intent) = array_refs![
        body,
        WIDTH_DISCRIMINATOR,
        WIDTH_BUMP,
        WIDTH_CANCELLED,
        WIDTH_AMOUNT_WITHDRAWN,
        WIDTH_AMOUNT_RECEIVED,
        WIDTH_CREATED_BY,
        WIDTH_INTENT
    ];
    OrderSlots {
        discriminator,
        bump,
        cancelled,
        amount_withdrawn,
        amount_received,
        created_by,
        intent,
    }
}

/// [`order_slots`] over a mutable body, for in-place writes.
#[inline]
fn order_slots_mut(body: &mut [u8; SIZE]) -> OrderSlotsMut<'_> {
    let (discriminator, bump, cancelled, amount_withdrawn, amount_received, created_by, intent) = mut_array_refs![
        body,
        WIDTH_DISCRIMINATOR,
        WIDTH_BUMP,
        WIDTH_CANCELLED,
        WIDTH_AMOUNT_WITHDRAWN,
        WIDTH_AMOUNT_RECEIVED,
        WIDTH_CREATED_BY,
        WIDTH_INTENT
    ];
    OrderSlotsMut {
        discriminator,
        bump,
        cancelled,
        amount_withdrawn,
        amount_received,
        created_by,
        intent,
    }
}

/// A pair of order fill amounts: sell token withdrawn and buy token received.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FillAmounts {
    pub withdrawn: u64,
    pub received: u64,
}

/// Extract the values relevant for understanding the fill of an order.
/// Returns a tuple. First return value is the amount currently filled, and the
/// second return value is the amount that has been requested to be filled by
/// the intent.
pub fn fill_progress(intent: &OrderIntent, fill: FillAmounts) -> (u64, u64) {
    match intent.flags.kind {
        OrderKind::Sell => (fill.withdrawn, intent.sell_amount),
        OrderKind::Buy => (fill.received, intent.buy_amount),
    }
}

/// A zero-copy accessor over an order account's canonical byte representation.
///
/// `T` is the borrow backing it, anything that dereferences to the account's
/// bytes: `&[u8]` grants read access; `&mut [u8]` grants write access.
pub struct OrderAccount<T>(T);

impl<T: Deref<Target = [u8]>> OrderAccount<T> {
    /// Wrap an account's bytes, checking they are exactly one order body long
    /// and begin with the order discriminator. Every accessor relies on that
    /// guarantee not to panic.
    /// No further byte validation is performed: the intent may be invalid, or
    /// the intent fail to decode, and `attach` would still return `Ok`.
    pub fn attach(bytes: T) -> Result<Self, ProgramError> {
        let body: &[u8; SIZE] = (&*bytes)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        if order_slots(body).discriminator != &[DISCRIMINATOR] {
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(Self(bytes))
    }

    fn body(&self) -> &[u8; SIZE] {
        self.0
            .first_chunk::<SIZE>()
            .expect("body length is guaranteed by any constructor of `OrderAccount`")
    }

    pub fn bump_slice(&self) -> &[u8; 1] {
        order_slots(self.body()).bump
    }

    /// Canonical bump of the PDA this account lives at.
    pub fn bump(&self) -> u8 {
        self.bump_slice()[0]
    }

    /// Whether the order has been cancelled by its owner. Fails with
    /// [`ProgramError::InvalidAccountData`] if the stored byte is out of range.
    pub fn cancelled(&self) -> Result<bool, ProgramError> {
        match order_slots(self.body()).cancelled {
            [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }

    /// The order's cumulative fill so far: the sell token withdrawn and the buy
    /// token received across settlements.
    pub fn filled_amounts(&self) -> FillAmounts {
        let slots = order_slots(self.body());
        FillAmounts {
            withdrawn: u64::from_le_bytes(*slots.amount_withdrawn),
            received: u64::from_le_bytes(*slots.amount_received),
        }
    }

    /// Account that funded the rent for this PDA; the rent is refunded here
    /// when the order is cleared.
    pub fn created_by(&self) -> Pubkey {
        Pubkey::new_from_array(*order_slots(self.body()).created_by)
    }

    /// The verbatim [`EncodedOrderIntent`] bytes stored in the account.
    pub fn intent_bytes(&self) -> &[u8; EncodedOrderIntent::SIZE] {
        order_slots(self.body()).intent
    }

    /// Decode the stored intent. Fails with [`ProgramError::InvalidAccountData`]
    /// if any intent byte the intent decoder rejects is out of range.
    pub fn intent(&self) -> Result<OrderIntent, ProgramError> {
        OrderIntent::try_from(self.intent_bytes()).map_err(|_| ProgramError::InvalidAccountData)
    }

    /// The order UID: the hash of the intent's canonical bytes. Computed over
    /// the stored bytes directly, so no intent decoding is required.
    /// It doesn't check that the underlying intent is valid, though this does
    /// not happen for accounts created by the program.
    pub fn intent_uid(&self) -> Hash {
        intent::hash_bytes(self.intent_bytes())
    }
}

impl<'a> OrderAccount<Ref<'a, [u8]>> {
    /// Attach to an account's borrowed data.
    pub fn from_account(account: &'a AccountView) -> Result<Self, ProgramError> {
        Self::attach(account.try_borrow()?)
    }

    /// Attach to the order at the given PDA and confirm the PDA is derivable
    /// from its own data: both the UID and the bump feeding the derivation come
    /// from the stored body.
    pub fn load_from_pda(
        order_pda: &'a AccountView,
        program_id: &Address,
    ) -> Result<Self, ProgramError> {
        let order = Self::from_account(order_pda)?;
        if !is_pda_with_signer_seeds(
            order_pda.address(),
            program_id,
            order_pda_signer_seeds(&order.intent_uid(), order.bump_slice()),
        ) {
            return Err(SettlementError::AccountNotDerivable.into());
        }
        Ok(order)
    }
}

impl<T: DerefMut<Target = [u8]>> OrderAccount<T> {
    /// Writes a full order body into `bytes` and return the accessor over it.
    /// `encoded_intent` is stored verbatim and unvalidated: an order's PDA is
    /// derived from the hash of these exact bytes, so their canonicality is the
    /// caller's responsibility.
    ///
    /// Fails with [`ProgramError::AccountDataTooSmall`] if the buffer isn't
    /// exactly one order body long.
    pub fn initialize(
        mut bytes: T,
        bump: u8,
        cancelled: bool,
        amount_withdrawn: u64,
        amount_received: u64,
        created_by: &Pubkey,
        encoded_intent: &[u8; EncodedOrderIntent::SIZE],
    ) -> Result<Self, ProgramError> {
        {
            let body: &mut [u8; SIZE] = (&mut *bytes)
                .try_into()
                .map_err(|_| ProgramError::AccountDataTooSmall)?;
            let slots = order_slots_mut(body);
            *slots.discriminator = [DISCRIMINATOR];
            *slots.bump = [bump];
            *slots.cancelled = [cancelled as u8];
            *slots.amount_withdrawn = amount_withdrawn.to_le_bytes();
            *slots.amount_received = amount_received.to_le_bytes();
            *slots.created_by = created_by.to_bytes();
            *slots.intent = *encoded_intent;
        }
        Ok(Self(bytes))
    }

    fn body_mut(&mut self) -> &mut [u8; SIZE] {
        let bytes: &mut [u8] = &mut self.0;
        bytes
            .first_chunk_mut::<SIZE>()
            .expect("body length is guaranteed by any constructor of `OrderAccount`")
    }

    /// Overwrite the two cumulative fill amounts in place.
    pub fn set_amounts(&mut self, amounts: FillAmounts) {
        let slots = order_slots_mut(self.body_mut());
        *slots.amount_withdrawn = amounts.withdrawn.to_le_bytes();
        *slots.amount_received = amounts.received.to_le_bytes();
    }
}

/// Test scaffolding for building order-account bytes, shared by this crate's
/// tests and its consumers' via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::prelude::*;

    use super::{EncodedOrderIntent, OrderAccount, OrderIntent, Pubkey, SIZE};
    use crate::data::intent::fixtures::{arb_order_intent, sample_intent};

    // Hardcoded but verified in a sanity-check test.
    pub const DISCRIMINATOR_OFFSET: usize = 0;
    pub const CANCELLED_OFFSET: usize = 2;
    pub const INTENT_OFFSET: usize = 51;

    /// The fields an order account encodes, as plain owned values, so a test can
    /// assert the accessor reads back exactly what was written.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct OrderFields {
        pub bump: u8,
        pub cancelled: bool,
        pub amount_withdrawn: u64,
        pub amount_received: u64,
        pub created_by: Pubkey,
        pub intent: OrderIntent,
    }

    impl OrderFields {
        /// Encode these fields into canonical order-account bytes.
        pub fn encode(&self) -> [u8; SIZE] {
            let mut bytes = [0u8; SIZE];
            OrderAccount::initialize(
                &mut bytes[..],
                self.bump,
                self.cancelled,
                self.amount_withdrawn,
                self.amount_received,
                &self.created_by,
                &EncodedOrderIntent::from(&self.intent),
            )
            .expect("a full-length buffer initializes");
            bytes
        }
    }
    /// Hand-picked example order fields wrapping [`sample_intent`]: the single
    /// source for the sample account the tests build on.
    pub fn sample_order_fields(cancelled: bool) -> OrderFields {
        OrderFields {
            bump: 0xbd,
            cancelled,
            amount_withdrawn: 0x0112_2334_4556_6778,
            amount_received: 0x899a_abbc_cdde_eff0,
            created_by: Pubkey::new_from_array([0x42; 32]),
            intent: sample_intent(Default::default()),
        }
    }

    /// The canonical bytes of [`sample_order_fields`].
    pub fn sample_order_bytes(cancelled: bool) -> [u8; SIZE] {
        sample_order_fields(cancelled).encode()
    }

    /// Any valid set of order fields.
    pub fn arb_order_account() -> impl Strategy<Value = OrderFields> {
        (
            any::<u8>(),
            any::<bool>(),
            any::<u64>(),
            any::<u64>(),
            any::<[u8; 32]>(),
            arb_order_intent(),
        )
            .prop_map(
                |(bump, cancelled, amount_withdrawn, amount_received, created_by, intent)| {
                    OrderFields {
                        bump,
                        cancelled,
                        amount_withdrawn,
                        amount_received,
                        created_by: Pubkey::new_from_array(created_by),
                        intent,
                    }
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{
        sample_order_bytes, sample_order_fields, OrderFields, CANCELLED_OFFSET,
        DISCRIMINATOR_OFFSET, INTENT_OFFSET,
    };
    use super::*;
    use crate::data::intent::fixtures::{sample_intent, FLAGS_OFFSET};
    use crate::data::intent::Flags;

    #[test]
    fn widths_match_order_fields() {
        use core::mem::size_of_val;

        // `size_of_val` only consults the field type, so any value works.
        let OrderFields {
            bump,
            cancelled,
            amount_withdrawn,
            amount_received,
            created_by,
            // Stored encoded, so its width matches `EncodedOrderIntent`, not the
            // decoded `OrderIntent`.
            intent: _intent,
        } = sample_order_fields(false);

        assert_eq!(WIDTH_BUMP, size_of_val(&bump));
        assert_eq!(WIDTH_CANCELLED, size_of_val(&cancelled));
        assert_eq!(WIDTH_AMOUNT_WITHDRAWN, size_of_val(&amount_withdrawn));
        assert_eq!(WIDTH_AMOUNT_RECEIVED, size_of_val(&amount_received));
        assert_eq!(WIDTH_CREATED_BY, size_of_val(&created_by));
        assert_eq!(WIDTH_INTENT, size_of::<EncodedOrderIntent>());
    }

    #[test]
    fn reads_fields_it_was_initialized_with() {
        for cancelled in [false, true] {
            let fields = sample_order_fields(cancelled);
            let bytes = fields.encode();
            let OrderFields {
                bump,
                cancelled,
                amount_withdrawn,
                amount_received,
                created_by,
                intent,
            } = fields;
            let order = OrderAccount::attach(&bytes[..]).expect("sample must attach");

            assert_eq!(order.bump(), bump);
            assert_eq!(order.cancelled().expect("valid cancelled"), cancelled);
            assert_eq!(
                order.filled_amounts(),
                FillAmounts {
                    withdrawn: amount_withdrawn,
                    received: amount_received,
                }
            );
            assert_eq!(order.created_by(), created_by);
            assert_eq!(order.intent().expect("valid intent"), intent);
            assert_eq!(order.intent_uid(), intent.uid());
        }
    }

    #[test]
    fn set_amounts_writes_only_the_two_amount_fields() {
        let mut bytes = sample_order_bytes(false);

        let filled = FillAmounts {
            withdrawn: 0xdead_beef_dead_beef,
            received: 0x0123_4567_89ab_cdef,
        };
        let mut account = OrderAccount::attach(&mut bytes[..]).expect("sample must attach");
        let current = account.filled_amounts();
        assert_ne!(
            filled.withdrawn, current.withdrawn,
            "sanity check: new withdrawn is different"
        );
        assert_ne!(
            filled.received, current.received,
            "sanity check: new received is different"
        );

        account.set_amounts(filled.clone());

        // Indistinguishable from re-stamping the whole account with just the
        // two amounts changed: every other byte is untouched.
        let expected = OrderFields {
            amount_withdrawn: filled.withdrawn,
            amount_received: filled.received,
            ..sample_order_fields(false)
        }
        .encode();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn fill_progress_tracks_the_exact_side_only() {
        const SELL_AMOUNT: u64 = 1_000;
        const BUY_AMOUNT: u64 = 2_000;

        let intent = |kind| OrderIntent {
            sell_amount: SELL_AMOUNT,
            buy_amount: BUY_AMOUNT,
            ..sample_intent(Flags {
                kind,
                ..Default::default()
            })
        };

        // (kind, withdrawn, received, expected)
        let cases = [
            (OrderKind::Sell, SELL_AMOUNT, 0, true), // fully filled SELL order (stolen money, generally impossible)
            (OrderKind::Buy, 0, BUY_AMOUNT, true),   // fully filled BUY order (free money)
            (OrderKind::Sell, u64::MAX, 0, true), // sell fill past the intent amount (should be impossible)
            (OrderKind::Sell, u64::MAX, u64::MAX, true), // sell fill past the intent amount (should be impossible)
            (OrderKind::Buy, 0, u64::MAX, true),         // buy fill past the intent amount
            (OrderKind::Buy, u64::MAX, u64::MAX, true),  // buy fill past the intent amount
            (OrderKind::Sell, 0, 0, false),              // unfilled order
            (OrderKind::Sell, SELL_AMOUNT - 1, BUY_AMOUNT, false), // not fully filled SELL order
            (OrderKind::Buy, SELL_AMOUNT, BUY_AMOUNT - 1, false), // not fully filled BUY order with fully filled sell side (generally should be impossible)
        ];
        for (kind, withdrawn, received, expected) in cases {
            let (filled, order_amount) = fill_progress(
                &intent(kind),
                FillAmounts {
                    withdrawn,
                    received,
                },
            );
            assert_eq!(
                filled >= order_amount,
                expected,
                "{kind:?} order withdrawn={withdrawn} received={received}",
            );
        }
    }

    #[test]
    fn sanity_check_offsets() {
        fn first_differing_byte(lhs: &[u8], rhs: &[u8]) -> Option<usize> {
            lhs.iter().zip(rhs).position(|(l, r)| l != r)
        }

        let base = sample_order_bytes(false);
        let cancelled = sample_order_bytes(true);
        assert_eq!(
            first_differing_byte(&base, &cancelled).expect("should differ in the cancelled byte"),
            CANCELLED_OFFSET
        );

        // Differs only in the embedded intent.
        let encoded_intent: [u8; EncodedOrderIntent::SIZE] =
            (&EncodedOrderIntent::from(&sample_order_fields(false).intent)).into();
        // Hack: xoring each byte makes sure all bytes are different.
        // In general, it isn't guaranteed that the result encodes to a
        // valid intent, but in this case we know it because the only byte
        // that may fail decoding is the flags byte, and `^0x01` only flips
        // its `created_on_chain` flag bits, never a reserved one.
        let bitwise_different_encoded_intent: [u8; EncodedOrderIntent::SIZE] =
            encoded_intent.map(|b| b ^ 0x01);
        let changed_intent_field =
            OrderIntent::try_from(&bitwise_different_encoded_intent).expect("hack should work");
        let changed_intent = OrderFields {
            intent: changed_intent_field,
            ..sample_order_fields(false)
        }
        .encode();
        assert_eq!(
            first_differing_byte(&base, &changed_intent).expect("should differ in the intent slot"),
            INTENT_OFFSET
        );
    }

    #[test]
    fn attach_rejects_wrong_discriminator() {
        let mut bytes = sample_order_bytes(false);
        bytes[DISCRIMINATOR_OFFSET] ^= 0xff;
        assert_eq!(
            OrderAccount::attach(&bytes[..]).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn attach_rejects_wrong_length() {
        let bytes = sample_order_bytes(false);
        assert_eq!(
            OrderAccount::attach(&bytes[..SIZE - 1]).err(),
            Some(ProgramError::InvalidAccountData),
        );
        let too_long = [bytes.as_slice(), [0].as_slice()].concat();
        assert_eq!(
            OrderAccount::attach(&too_long[..]).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn cancelled_rejects_non_boolean_byte() {
        let mut bytes = sample_order_bytes(false);
        for bad in 0x02u8..=0xff {
            bytes[CANCELLED_OFFSET] = bad;
            let order =
                OrderAccount::attach(&bytes[..]).expect("attach ignores the cancelled byte");
            assert_eq!(order.cancelled(), Err(ProgramError::InvalidAccountData));
        }
    }

    #[test]
    fn intent_propagates_invalid_intent() {
        let mut bytes = sample_order_bytes(false);
        // Set a reserved bit of the flags byte inside the intent slot: the
        // intent decoder rejects it and the read surfaces `InvalidAccountData`.
        bytes[INTENT_OFFSET + FLAGS_OFFSET] = 0xff;
        let order = OrderAccount::attach(&bytes[..]).expect("attach ignores the intent slot");
        assert_eq!(order.intent(), Err(ProgramError::InvalidAccountData));
    }

    mod load_from_pda {
        use super::*;
        use crate::fixtures::PROGRAM_ID;
        use crate::instruction::fixtures::fake_account_with_data;
        use crate::pda::order::find_order_pda;

        /// [`sample_order_bytes`] carrying its own canonical bump, plus the
        /// address of the PDA it belongs at.
        fn canonical_bytes(cancelled: bool) -> ([u8; SIZE], Address) {
            let fields = sample_order_fields(cancelled);
            let (pda_address, bump) = find_order_pda(&PROGRAM_ID, &fields.intent.uid());
            let bytes = OrderFields { bump, ..fields }.encode();
            (bytes, pda_address)
        }

        #[test]
        fn accepts_the_canonical_pda() {
            let (bytes, pda_address) = canonical_bytes(false);
            let order_pda = fake_account_with_data(pda_address, &bytes[..]);

            let order = OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID)
                .expect("canonical PDA must load");
            assert_eq!(order.bump(), bytes[1]);
        }

        #[test]
        fn rejects_a_non_canonical_address() {
            let (bytes, _) = canonical_bytes(false);
            // An address unrelated to the intent's canonical seeds.
            let wrong_address = Pubkey::new_from_array([0x42; 32]);
            let order_pda = fake_account_with_data(wrong_address, &bytes[..]);

            assert_eq!(
                OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID).err(),
                Some(SettlementError::AccountNotDerivable.into()),
            );
        }

        #[test]
        fn rejects_a_stored_bump_that_does_not_derive_the_address() {
            let (mut bytes, pda_address) = canonical_bytes(false);
            // Any bump other than the canonical one either derives a different
            // address or fails to derive one at all (falling on curve); either
            // way, the PDA can no longer be proven canonical.
            bytes[1] = bytes[1].wrapping_sub(1);
            let order_pda = fake_account_with_data(pda_address, &bytes[..]);

            assert_eq!(
                OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID).err(),
                Some(SettlementError::AccountNotDerivable.into()),
            );
        }

        #[test]
        fn attach_propagates_decode_errors() {
            let (mut bytes, pda_address) = canonical_bytes(false);
            bytes[DISCRIMINATOR_OFFSET] ^= 0xff;
            let order_pda = fake_account_with_data(pda_address, &bytes);

            assert_eq!(
                OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID).err(),
                Some(ProgramError::InvalidAccountData),
            );
        }
    }

    // Property-based tests, non-deterministic.
    mod proptest {
        use ::proptest::{prelude::*, test_runner::TestCaseError};

        use super::*;
        use crate::data::{intent::fixtures::arb_flags_byte, order::fixtures::arb_order_account};

        proptest! {
            // For any order fields, the bytes `OrderFields::encode` writes read
            // back through the accessor as exactly those fields.
            #[test]
            fn reads_match_written_fields(fields in arb_order_account()) {
                let bytes = fields.encode();
                let order = OrderAccount::attach(&bytes[..])
                    .map_err(|e| TestCaseError::fail(format!("attach failed: {e:?}")))?;
                let OrderFields {
                    bump,
                    cancelled,
                    amount_withdrawn,
                    amount_received,
                    created_by,
                    intent,
                } = fields;
                prop_assert_eq!(order.bump(), bump);
                prop_assert_eq!(order.cancelled().expect("valid cancelled"), cancelled);
                prop_assert_eq!(
                    order.filled_amounts(),
                    FillAmounts {
                        withdrawn: amount_withdrawn,
                        received: amount_received,
                    }
                );
                prop_assert_eq!(order.created_by(), created_by);
                prop_assert_eq!(order.intent_uid(), intent.uid());
                prop_assert_eq!(order.intent().expect("valid intent"), intent);
            }

            // For any bytes whose `cancelled` byte and intent flags are valid,
            // reading every field back and re-encoding reproduces the same bytes.
            #[test]
            fn bytes_roundtrip(
                mut bytes in any::<[u8; SIZE]>(),
                cancelled in any::<bool>(),
                flags in arb_flags_byte(),
            ) {
                bytes[DISCRIMINATOR_OFFSET] = DISCRIMINATOR;
                bytes[CANCELLED_OFFSET] = cancelled as u8;
                bytes[INTENT_OFFSET + FLAGS_OFFSET] = flags;

                let order = OrderAccount::attach(&bytes[..])
                    .map_err(|e| TestCaseError::fail(format!("attach failed: {e:?}")))?;
                let filled = order.filled_amounts();
                let reencoded = OrderFields {
                    bump: order.bump(),
                    cancelled: order
                        .cancelled()
                        .map_err(|e| TestCaseError::fail(format!("cancelled: {e:?}")))?,
                    amount_withdrawn: filled.withdrawn,
                    amount_received: filled.received,
                    created_by: order.created_by(),
                    intent: order
                        .intent()
                        .map_err(|e| TestCaseError::fail(format!("intent: {e:?}")))?,
                }
                .encode();
                prop_assert_eq!(reencoded, bytes);
            }

            // Overwriting the amounts in place equals writing the same fields
            // with `OrderFields::encode` from scratch.
            #[test]
            fn set_amounts_matches_a_fresh_encode(
                fields in arb_order_account(),
                new_withdrawn in any::<u64>(),
                new_received in any::<u64>(),
            ) {
                let mut bytes = fields.encode();
                OrderAccount::attach(&mut bytes[..])
                    .map_err(|e| TestCaseError::fail(format!("attach failed: {e:?}")))?
                    .set_amounts(FillAmounts {
                        withdrawn: new_withdrawn,
                        received: new_received,
                    });
                let expected = OrderFields {
                    amount_withdrawn: new_withdrawn,
                    amount_received: new_received,
                    ..fields
                }
                .encode();
                prop_assert_eq!(bytes, expected);
            }
        }
    }
}
