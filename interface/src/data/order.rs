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

use bytemuck::{Pod, Zeroable};
use solana_account_view::AccountView;
use solana_address::Address;
use solana_hash::Hash;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::data::intent::{EncodedOrderIntent, OrderIntent};
use crate::pda::is_pda_with_signer_seeds;
use crate::pda::order::order_pda_signer_seeds;
use crate::{SettlementAccount, SettlementError};

/// Idiomatic representation of an order PDA's body.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct OrderAccount {
    /// Canonical bump of the PDA this account lives at. Written at creation
    /// time so callers don't need to supply it; see [`Self::load_from_pda`].
    pub bump: u8,

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

impl OrderAccount {
    /// Load and decode the order at the given PDA, and confirm the PDA is
    /// derivable from its own data: both the UID and the bump feeding the
    /// derivation come from the stored body.
    pub fn load_from_pda(
        order_pda: &AccountView,
        program_id: &Address,
    ) -> Result<Self, ProgramError> {
        let (account, uid) = {
            let data = order_pda.try_borrow()?;
            let bytes: &[u8; EncodedOrderAccount::SIZE] = (&*data)
                .try_into()
                .map_err(|_| ProgramError::InvalidAccountData)?;
            EncodedOrderAccount::decode_and_hash(bytes)?
        };

        if !is_pda_with_signer_seeds(
            order_pda,
            program_id,
            order_pda_signer_seeds(&uid, &[account.bump]),
        ) {
            return Err(SettlementError::AccountNotDerivable.into());
        }

        Ok(account)
    }
}

/// Canonical 201-byte representation of an [`OrderAccount`]. The bytes
/// written to/read from the order PDA's data area.
///
/// Layout: one character per byte, cell widths proportional to field size,
/// each divider belongs to the cell on its right. Integers are little-endian
/// (Anchor/Borsh convention). The intent slot holds a verbatim
/// [`EncodedOrderIntent`]; see that type's docs for its inner layout.
///
/// ```text
///  ┌───── discriminator
///  │┌──── bump
///  ││┌─── cancelled
///  ┌┬┬┬───────┬───────┬───────────────────────────────┬─────────────────...─────────────────┐
///  ││││amount_│amount_│                               │                                     │
///  ││││with-  │re-    │           created_by          │     intent (EncodedOrderIntent)     │
///  ││││drawn  │ceived │                               │                                     │
///  └┴┴┴───────┴───────┴───────────────────────────────┴─────────────────...─────────────────┘
/// 0 1 2 3      11      19                              51                ...               201
/// ```
///
/// Every field is byte-granular (the embedded [`EncodedOrderIntent`] is itself a
/// byte-granular `Pod`), so the struct has alignment 1 and no padding: its
/// in-memory image is exactly the 201-byte canonical encoding. That lets the
/// program reinterpret the PDA's data slice as this type in place — reading and
/// updating the fill amounts without decoding or re-encoding the whole account.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct EncodedOrderAccount {
    discriminator: u8,
    bump: u8,
    cancelled: u8,
    amount_withdrawn: [u8; 8],
    amount_received: [u8; 8],
    created_by: [u8; 32],
    intent: EncodedOrderIntent,
}

const _: () = assert!(core::mem::size_of::<EncodedOrderAccount>() == EncodedOrderAccount::SIZE);
const _: () = assert!(core::mem::align_of::<EncodedOrderAccount>() == 1);

impl core::ops::Deref for EncodedOrderAccount {
    type Target = [u8; EncodedOrderAccount::SIZE];

    fn deref(&self) -> &Self::Target {
        bytemuck::cast_ref(self)
    }
}

impl EncodedOrderAccount {
    pub const SIZE: usize = 201;

    /// Single-byte account discriminator. See [`SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::OrderAccount.discriminator();

    /// Reinterpret canonical bytes as an encoded order account in place, no copy.
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> &Self {
        bytemuck::cast_ref(bytes)
    }

    /// [`Self::from_bytes`] over a mutable buffer, for in-place writes.
    pub fn from_bytes_mut(bytes: &mut [u8; Self::SIZE]) -> &mut Self {
        bytemuck::cast_mut(bytes)
    }

    /// Whether the account's discriminator marks it as an order account.
    pub fn has_valid_discriminator(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }

    /// Canonical bump stored in the account body.
    pub fn bump(&self) -> u8 {
        self.bump
    }

    /// `cancelled` flag, rejecting an out-of-range byte.
    pub fn cancelled(&self) -> Result<bool, ProgramError> {
        match self.cancelled {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProgramError::InvalidAccountData),
        }
    }

    /// Cumulative sell-token amount withdrawn across settlements.
    pub fn amount_withdrawn(&self) -> u64 {
        u64::from_le_bytes(self.amount_withdrawn)
    }

    /// Cumulative buy-token amount received across settlements.
    pub fn amount_received(&self) -> u64 {
        u64::from_le_bytes(self.amount_received)
    }

    /// Overwrite the cumulative withdrawn amount in place.
    pub fn set_amount_withdrawn(&mut self, amount: u64) {
        self.amount_withdrawn = amount.to_le_bytes();
    }

    /// Overwrite the cumulative received amount in place.
    pub fn set_amount_received(&mut self, amount: u64) {
        self.amount_received = amount.to_le_bytes();
    }

    /// The embedded encoded order intent.
    pub fn intent(&self) -> &EncodedOrderIntent {
        &self.intent
    }

    /// Decode the account body and compute the embedded intent's UID in one
    /// shot, mirroring [`EncodedOrderIntent::decode_and_hash`]. Decoding
    /// validates the discriminator and the intent; returns
    /// [`ProgramError::InvalidAccountData`] on a decode error.
    pub fn decode_and_hash(bytes: &[u8; Self::SIZE]) -> Result<(OrderAccount, Hash), ProgramError> {
        let order_account = OrderAccount::try_from(*bytes)?;
        // The order UID is the hash of the intent's canonical bytes. Decoding
        // succeeded, so the intent slot already holds those exact bytes: hash
        // them in place rather than using `intent.uid()` to avoid re-encoding.
        let intent_uid = Self::from_bytes(bytes).intent.hash();
        Ok((order_account, intent_uid))
    }
}

/// Writes the canonical [`EncodedOrderAccount`] encoding of the given fields
/// into `buffer`. `encoded_intent` must be a canonical [`EncodedOrderIntent`]
/// encoding: validating it is the caller's responsibility.
pub fn write_account(
    buffer: &mut [u8; EncodedOrderAccount::SIZE],
    bump: u8,
    cancelled: bool,
    amount_withdrawn: u64,
    amount_received: u64,
    created_by: &Pubkey,
    encoded_intent: &[u8; EncodedOrderIntent::SIZE],
) {
    let account = EncodedOrderAccount::from_bytes_mut(buffer);
    account.discriminator = EncodedOrderAccount::DISCRIMINATOR;
    account.bump = bump;
    account.cancelled = cancelled as u8;
    account.amount_withdrawn = amount_withdrawn.to_le_bytes();
    account.amount_received = amount_received.to_le_bytes();
    account.created_by = created_by.to_bytes();
    account.intent = *EncodedOrderIntent::from_bytes(encoded_intent);
}

impl From<EncodedOrderAccount> for [u8; EncodedOrderAccount::SIZE] {
    fn from(encoded: EncodedOrderAccount) -> Self {
        *bytemuck::cast_ref(&encoded)
    }
}

impl From<OrderAccount> for EncodedOrderAccount {
    fn from(account: OrderAccount) -> Self {
        EncodedOrderAccount {
            discriminator: Self::DISCRIMINATOR,
            bump: account.bump,
            cancelled: account.cancelled as u8,
            amount_withdrawn: account.amount_withdrawn.to_le_bytes(),
            amount_received: account.amount_received.to_le_bytes(),
            created_by: account.created_by.to_bytes(),
            intent: EncodedOrderIntent::from(&account.intent),
        }
    }
}

impl TryFrom<[u8; EncodedOrderAccount::SIZE]> for OrderAccount {
    type Error = ProgramError;

    fn try_from(bytes: [u8; EncodedOrderAccount::SIZE]) -> Result<Self, Self::Error> {
        let encoded = EncodedOrderAccount::from_bytes(&bytes);

        if !encoded.has_valid_discriminator() {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(OrderAccount {
            bump: encoded.bump,
            cancelled: encoded.cancelled()?,
            amount_withdrawn: encoded.amount_withdrawn(),
            amount_received: encoded.amount_received(),
            created_by: Pubkey::new_from_array(encoded.created_by),
            intent: OrderIntent::try_from(&encoded.intent)
                .map_err(|_| ProgramError::InvalidAccountData)?,
        })
    }
}

impl TryFrom<&[u8]> for OrderAccount {
    type Error = ProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let bytes: &[u8; EncodedOrderAccount::SIZE] = bytes
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        OrderAccount::try_from(*bytes)
    }
}

impl TryFrom<EncodedOrderAccount> for OrderAccount {
    type Error = ProgramError;

    fn try_from(encoded: EncodedOrderAccount) -> Result<Self, Self::Error> {
        OrderAccount::try_from(<[u8; EncodedOrderAccount::SIZE]>::from(encoded))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::prelude::*;

    use super::{OrderAccount, Pubkey};
    use crate::data::intent::{
        fixtures::{arb_order_intent, sample_intent},
        OrderKind,
    };

    // Hardcoded but verified in a sanity-check test.
    pub const DISCRIMINATOR_OFFSET: usize = 0;
    pub const CANCELLED_OFFSET: usize = 2;
    pub const INTENT_OFFSET: usize = 51;

    /// Hand-picked example order account wrapping [`sample_intent`].
    pub fn sample_account(cancelled: bool) -> OrderAccount {
        OrderAccount {
            bump: 0xfd,
            cancelled,
            amount_withdrawn: 0x0112_2334_4556_6778,
            amount_received: 0x899a_abbc_cdde_eff0,
            created_by: Pubkey::new_from_array([0x43; 32]),
            intent: sample_intent(OrderKind::Sell, false),
        }
    }

    /// Any valid [`OrderAccount`].
    pub fn arb_order_account() -> impl Strategy<Value = OrderAccount> {
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
                    OrderAccount {
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
    use super::fixtures::{sample_account, CANCELLED_OFFSET, DISCRIMINATOR_OFFSET, INTENT_OFFSET};
    use super::*;
    use crate::data::intent::{
        fixtures::{sample_intent, KIND_OFFSET, PARTIALLY_FILLABLE_OFFSET},
        OrderKind,
    };

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
    fn sanity_check_offsets() {
        fn first_differing_byte(lhs: &[u8], rhs: &[u8]) -> Option<usize> {
            lhs.iter().zip(rhs).position(|(l, r)| l != r)
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
    fn decode_rejects_wrong_discriminator() {
        let mut bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();
        bytes[0] ^= 0xff;
        let err = OrderAccount::try_from(bytes).expect_err("wrong discriminator must be rejected");
        assert_eq!(err, ProgramError::InvalidAccountData);
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

    #[test]
    fn decode_and_hash_catches_errors() {
        let mut bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();
        // Corrupt the `cancelled` byte to an out-of-range value so the
        // underlying `try_from` rejects it.
        bytes[CANCELLED_OFFSET] = 0xff;
        let err = EncodedOrderAccount::decode_and_hash(&bytes)
            .expect_err("decode_and_hash must propagate the try_from error");
        assert_eq!(err, ProgramError::InvalidAccountData);
    }

    #[test]
    fn try_from_exact_length_slice_decodes() {
        let account = sample_account(false);
        let bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(account.clone()).into();

        let decoded = OrderAccount::try_from(&bytes[..]).expect("exact-length slice must decode");
        assert_eq!(decoded, account);
    }

    #[test]
    fn try_from_wrong_length_slice_is_rejected() {
        let bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();

        assert_eq!(
            OrderAccount::try_from(&bytes[..EncodedOrderAccount::SIZE - 1]),
            Err(ProgramError::InvalidAccountData),
        );

        let too_long = [bytes.as_slice(), [0].as_slice()].concat();
        assert_eq!(
            OrderAccount::try_from(&too_long[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn try_from_slice_forwards_decoding_errors() {
        let mut bytes: [u8; EncodedOrderAccount::SIZE] =
            EncodedOrderAccount::from(sample_account(false)).into();
        bytes[CANCELLED_OFFSET] = 0xff;
        assert_eq!(
            OrderAccount::try_from(&bytes[..]),
            Err(ProgramError::InvalidAccountData),
        );
    }

    mod load_from_pda {
        use super::*;
        use crate::instruction::fixtures::fake_account_with_data;
        use crate::pda::order::find_order_pda;

        const PROGRAM_ID: Address = Address::new_from_array([0xc0; 32]);

        /// [`sample_account`] carrying its own canonical bump, plus the address
        /// of the PDA it belongs at.
        fn canonical_account(cancelled: bool) -> (OrderAccount, Address) {
            let mut account = sample_account(cancelled);
            let (pda_address, bump) = find_order_pda(&PROGRAM_ID, &account.intent.uid());
            account.bump = bump;
            (account, pda_address)
        }

        #[test]
        fn accepts_the_canonical_pda() {
            let (account, pda_address) = canonical_account(false);
            let order_pda = fake_account_with_data(
                pda_address,
                &EncodedOrderAccount::from(account.clone())[..],
            );

            let loaded = OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID)
                .expect("canonical PDA must load");
            assert_eq!(loaded, account);
        }

        #[test]
        fn rejects_a_non_canonical_address() {
            let (account, _) = canonical_account(false);
            // An address unrelated to the intent's canonical seeds.
            let wrong_address = Pubkey::new_from_array([0x42; 32]);
            let order_pda =
                fake_account_with_data(wrong_address, &EncodedOrderAccount::from(account)[..]);

            let err = OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID)
                .expect_err("a non-canonical address must be rejected");
            assert_eq!(err, SettlementError::AccountNotDerivable.into());
        }

        #[test]
        fn rejects_a_stored_bump_that_does_not_derive_the_address() {
            let (mut account, pda_address) = canonical_account(false);
            // Any bump other than the canonical one either derives a
            // different address or fails to derive one at all (falling on
            // curve); either way, the PDA can no longer be proven canonical.
            account.bump = account.bump.wrapping_sub(1);
            let order_pda =
                fake_account_with_data(pda_address, &EncodedOrderAccount::from(account)[..]);

            let err = OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID)
                .expect_err("a non-canonical bump must be rejected");
            assert_eq!(err, SettlementError::AccountNotDerivable.into());
        }

        #[test]
        fn propagates_decode_errors() {
            let (account, pda_address) = canonical_account(false);
            let mut bytes: [u8; EncodedOrderAccount::SIZE] =
                EncodedOrderAccount::from(account).into();
            bytes[CANCELLED_OFFSET] = 0xff;
            let order_pda = fake_account_with_data(pda_address, &bytes);

            let err = OrderAccount::load_from_pda(&order_pda, &PROGRAM_ID)
                .expect_err("a corrupt account must fail to decode");
            assert_eq!(err, ProgramError::InvalidAccountData);
        }
    }

    #[test]
    fn direct_write_account_matches_order_account_decoding() {
        let bump = 254;
        let cancelled = true;
        let amount_withdrawn = 1337;
        let amount_received = 31337;
        let intent = sample_intent(OrderKind::Sell, false);
        let created_by = Pubkey::new_from_array([0x42u8; 32]);

        let mut buffer = [0u8; EncodedOrderAccount::SIZE];
        write_account(
            &mut buffer,
            bump,
            cancelled,
            amount_withdrawn,
            amount_received,
            &created_by,
            &<[u8; EncodedOrderIntent::SIZE]>::from(&EncodedOrderIntent::from(&intent)),
        );
        let direct = *EncodedOrderAccount::from_bytes(&buffer);
        let via_order_account = EncodedOrderAccount::from(OrderAccount {
            bump,
            cancelled,
            amount_withdrawn,
            amount_received,
            created_by,
            intent,
        });

        assert_eq!(direct, via_order_account);
    }

    // Property-based tests, non-deterministic.
    mod proptest {
        use ::proptest::{prelude::*, test_runner::TestCaseError};

        use super::*;
        use crate::data::{intent::fixtures::arb_order_kind, order::fixtures::arb_order_account};

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
                bytes[DISCRIMINATOR_OFFSET] = EncodedOrderAccount::DISCRIMINATOR;
                bytes[CANCELLED_OFFSET] = cancelled as u8;
                bytes[INTENT_OFFSET + KIND_OFFSET] = kind as u8;
                bytes[INTENT_OFFSET + PARTIALLY_FILLABLE_OFFSET] = partially_fillable as u8;
                let account = OrderAccount::try_from(bytes)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(*EncodedOrderAccount::from(account), bytes);
            }

            #[test]
            fn consistent_decode_and_hash(account in arb_order_account()) {
                let encoded = EncodedOrderAccount::from(account.clone());
                let (decoded, hash) = EncodedOrderAccount::decode_and_hash(&encoded)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(hash, account.intent.uid());
                prop_assert_eq!(decoded, account);
            }
        }
    }
}
