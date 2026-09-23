//! Zero-copy access to an order intent's canonical bytes.

use cow_settlement_interface::data::{
    intent::{check_bytes, hash_bytes, intent_slots, EncodedOrderIntent, Flags, OrderKind},
    order::{FillAmounts, OrderAccount},
};
use pinocchio::error::ProgramError;
use solana_hash::Hash;

/// A zero-copy accessor over an intent's canonical bytes, one getter per wire
/// field. The settlement program reads intents through it, so it spends no
/// compute copying fields it only compares.
///
/// The flags byte is the only one that can fail to decode, so [`Self::attach`]
/// validates it; every getter, [`Self::flags`] included, is infallible.
#[derive(Debug, Eq, PartialEq)]
pub struct OrderIntentAccessor<'a>(&'a [u8; EncodedOrderIntent::SIZE]);

impl<'a> OrderIntentAccessor<'a> {
    /// Wrap an intent's bytes. Fails with
    /// [`ProgramError::InvalidInstructionData`] if the flags byte sets a
    /// reserved bit; every other byte combination attaches.
    #[inline]
    pub fn attach(bytes: &'a [u8; EncodedOrderIntent::SIZE]) -> Result<Self, ProgramError> {
        check_bytes(bytes)?;
        Ok(Self(bytes))
    }

    /// Attach to the intent stored in an order account, in place. Fails with
    /// [`ProgramError::InvalidAccountData`] if the stored flags byte sets a
    /// reserved bit.
    pub fn from_order<T: core::ops::Deref<Target = [u8]>>(
        order: &'a OrderAccount<T>,
    ) -> Result<Self, ProgramError> {
        Self::attach(order.intent_bytes()).map_err(|_| ProgramError::InvalidAccountData)
    }

    /// Account authorized to create and invalidate this order and whose
    /// signature authenticates it; see `OrderIntent::owner`.
    pub fn owner(&self) -> &'a [u8; 32] {
        intent_slots(self.0).owner
    }

    /// Token account the sell-side funds are pulled from; see
    /// `OrderIntent::sell_token_account`.
    pub fn sell_token_account(&self) -> &'a [u8; 32] {
        intent_slots(self.0).sell_token
    }

    /// Mint of the sell token.
    pub fn sell_mint(&self) -> &'a [u8; 32] {
        intent_slots(self.0).sell_mint
    }

    /// Token account that receives the buy-side proceeds; see
    /// `OrderIntent::buy_token_account`.
    pub fn buy_token_account(&self) -> &'a [u8; 32] {
        intent_slots(self.0).buy_token
    }

    /// Mint of the buy token.
    pub fn buy_mint(&self) -> &'a [u8; 32] {
        intent_slots(self.0).buy_mint
    }

    /// Amount of the sell token; see `OrderIntent::sell_amount`.
    #[inline]
    pub fn sell_amount(&self) -> u64 {
        u64::from_le_bytes(*intent_slots(self.0).sell_amount)
    }

    /// Amount of the buy token; see `OrderIntent::buy_amount`.
    #[inline]
    pub fn buy_amount(&self) -> u64 {
        u64::from_le_bytes(*intent_slots(self.0).buy_amount)
    }

    /// Unix timestamp after which the order expires.
    pub fn valid_to(&self) -> u32 {
        u32::from_le_bytes(*intent_slots(self.0).valid_to)
    }

    /// The settings packed in the flags byte. The byte is decoded on every
    /// call rather than once in [`Self::attach`], so a caller only pays for
    /// the flags it reads.
    #[inline]
    pub fn flags(&self) -> Flags {
        Flags::try_from(*intent_slots(self.0).flags)
            .unwrap_or_else(|_| unreachable!("attach rejects reserved bits"))
    }

    /// Compute the UID of the order, which is the sha256 hash of its bytes.
    pub fn uid(&self) -> Hash {
        hash_bytes(self.0)
    }
}

/// Extract the values relevant for understanding the fill of an order.
/// Returns a tuple. First return value is the amount currently filled, and the
/// second return value is the amount that has been requested to be filled by
/// the intent.
#[inline]
pub fn fill_progress(intent: &OrderIntentAccessor, fill: FillAmounts) -> (u64, u64) {
    match intent.flags().kind {
        OrderKind::Sell => (fill.withdrawn, intent.sell_amount()),
        OrderKind::Buy => (fill.received, intent.buy_amount()),
    }
}

#[cfg(test)]
mod tests {
    use cow_settlement_interface::data::intent::fixtures::{sample_intent, FLAGS_OFFSET};
    use cow_settlement_interface::data::intent::OrderIntent;
    use cow_settlement_interface::data::order::fixtures::{sample_order_bytes, INTENT_OFFSET};

    use super::*;

    #[test]
    fn attach_rejects_reserved_flag_bits() {
        let mut bytes: [u8; EncodedOrderIntent::SIZE] =
            *EncodedOrderIntent::from(&sample_intent(Default::default()));
        for flags in u8::MIN..=u8::MAX {
            bytes[FLAGS_OFFSET] = flags;
            assert_eq!(
                OrderIntentAccessor::attach(&bytes).is_ok(),
                OrderIntent::try_from(&bytes).is_ok(),
                "flags {flags:#04x}: accessor and decoder must agree",
            );
        }
    }

    #[test]
    fn from_order_propagates_invalid_intent() {
        let mut bytes = sample_order_bytes(false);
        // Set a reserved bit of the flags byte inside the intent slot: the
        // intent accessor rejects it and the read surfaces `InvalidAccountData`.
        bytes[INTENT_OFFSET + FLAGS_OFFSET] = 0xff;
        let order = OrderAccount::attach(&bytes[..]).expect("attach ignores the intent slot");
        assert_eq!(
            OrderIntentAccessor::from_order(&order),
            Err(ProgramError::InvalidAccountData)
        );
    }

    #[test]
    fn fill_progress_tracks_the_exact_side_only() {
        const SELL_AMOUNT: u64 = 1_000;
        const BUY_AMOUNT: u64 = 2_000;

        let intent = |kind| {
            EncodedOrderIntent::from(&OrderIntent {
                sell_amount: SELL_AMOUNT,
                buy_amount: BUY_AMOUNT,
                ..sample_intent(Flags {
                    kind,
                    ..Default::default()
                })
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
            let encoded = intent(kind);
            let (filled, order_amount) = fill_progress(
                &OrderIntentAccessor::attach(&encoded).expect("sample must attach"),
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

    mod proptest {
        use ::proptest::{prelude::*, test_runner::TestCaseError};
        use cow_settlement_interface::{data::intent::fixtures::arb_order_intent, token_program::NATIVE_SOL_MINT};

        use super::*;

        proptest! {
            // For any `OrderIntent`, each accessor getter reads back the wire
            // field the encoding wrote, and the UID over the attached bytes
            // matches the owned intent's.
            #[test]
            fn accessor_reads_the_encoded_fields(intent in arb_order_intent()) {

                let OrderIntent {
                    owner,
                    sell,
                    buy,
                    sell_amount,
                    buy_amount,
                    valid_to,
                    flags,
                    app_data: _,
                } = intent;

                let encoded = EncodedOrderIntent::from(&intent);
                let accessor = OrderIntentAccessor::attach(&encoded)
                    .map_err(|e| TestCaseError::fail(format!("attach failed: {e:?}")))?;

                let buy_mint = buy.mint().unwrap_or(NATIVE_SOL_MINT);
                let buy_account = buy.account();

                prop_assert_eq!(accessor.uid(), intent.uid());
                prop_assert_eq!(accessor.owner(), owner.as_array());
                prop_assert_eq!(accessor.sell_token_account(), sell.token_account.as_array());
                prop_assert_eq!(accessor.sell_mint(), sell.mint.as_array());
                prop_assert_eq!(accessor.buy_token_account(), buy_account.as_array());
                prop_assert_eq!(accessor.buy_mint(), buy_mint.as_array());
                prop_assert_eq!(accessor.sell_amount(), sell_amount);
                prop_assert_eq!(accessor.buy_amount(), buy_amount);
                prop_assert_eq!(accessor.valid_to(), valid_to);
                prop_assert_eq!(accessor.flags(), flags);
            }
        }
    }
}
