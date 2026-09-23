//! Order intents and their canonical byte representation.
//!
//! The intent has two representations:
//!
//! - [`OrderIntent`] is the idiomatic Rust representation. Everything outside
//!   the settlement program uses it.
//! - [`EncodedOrderIntent`] is the canonical byte representation: the only
//!   thing sent on the wire and also the data encoding used to generate the
//!   order UID. There, `kind` and the intent's booleans share a single flags
//!   byte.
//!
//! Conversion is asymmetric: encoding an [`OrderIntent`] is infallible, but
//! decoding raw bytes returns `Result` and rejects a flags byte carrying a bit
//! the encoding doesn't define.

use core::mem::size_of;

use arrayref::{array_refs, mut_array_refs};
use derive_more::Deref;
use solana_hash::Hash;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::token_program::{is_native_sol, NATIVE_SOL_MINT};

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
    /// How the order is authenticated: `true` if the owner creates it
    /// themselves with a `CreateOrder` instruction they sign; `false` if it's
    /// authenticated off-chain by an Ed25519 signature, which lets anyone
    /// holding that signature create the order.
    pub created_on_chain: bool,

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
    const CREATED_ON_CHAIN: u8 = 1 << 0;
    const KIND: u8 = 1 << 1;
    const PARTIALLY_FILLABLE: u8 = 1 << 2;

    /// Every bit the encoding defines; the others are reserved.
    const DEFINED: u8 = Self::CREATED_ON_CHAIN | Self::KIND | Self::PARTIALLY_FILLABLE;
}

impl From<Flags> for [u8; 1] {
    /// The canonical flags byte. Reserved bits are left clear.
    fn from(flags: Flags) -> Self {
        let mut byte = 0;
        if flags.created_on_chain {
            byte |= Flags::CREATED_ON_CHAIN;
        }
        if flags.kind == OrderKind::Buy {
            byte |= Flags::KIND;
        }
        if flags.partially_fillable {
            byte |= Flags::PARTIALLY_FILLABLE;
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
            created_on_chain: byte & Self::CREATED_ON_CHAIN != 0,
            kind: if byte & Self::KIND == 0 {
                OrderKind::Sell
            } else {
                OrderKind::Buy
            },
            partially_fillable: byte & Self::PARTIALLY_FILLABLE != 0,
        })
    }
}

/// SPL tokens of `mint`, held in `token_account`.
///
/// A side of a trade that a token program moves. It's the payload of
/// [`Asset::TokenProgram`], and a type of its own so a side that can only be
/// this — an order's sell side — can say so.
// A default side is a fixture's starting point, never an order anyone should
// build, so it exists only where the fixtures do. Same for [`Asset`] and
// [`OrderIntent`], which bottom out here.
#[cfg_attr(any(test, feature = "test-fixtures"), derive(Default))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenAsset {
    pub mint: Pubkey,
    pub token_account: Pubkey,
}

/// One side of a trade: what moves, and the account it moves through.
///
/// The wire spells a side as a `(mint, account)` pair; this is the same pair
/// with the one combination that isn't a token account named for what it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Asset {
    /// Native SOL, moving as lamports on this plain account rather than
    /// through a token account.
    Native(Pubkey),

    /// Tokens moved by a token program.
    TokenProgram(TokenAsset),
}

#[cfg(any(test, feature = "test-fixtures"))]
impl Default for Asset {
    /// Native SOL on the all-zero address, the side an all-zero encoding
    /// carries: [`NATIVE_SOL_MINT`] is itself all-zero.
    fn default() -> Self {
        Asset::Native(Pubkey::default())
    }
}

impl From<TokenAsset> for Asset {
    fn from(token: TokenAsset) -> Self {
        Asset::TokenProgram(token)
    }
}

/// Returned by [`Asset::mint`] for native SOL, which moves as lamports and has
/// no mint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeSolHasNoMint;

impl Asset {
    /// The mint of a token side. Native SOL has none: its wire marker is
    /// [`NATIVE_SOL_MINT`], not a mint.
    pub fn mint(&self) -> Result<Pubkey, NativeSolHasNoMint> {
        match self {
            Asset::Native(_) => Err(NativeSolHasNoMint),
            Asset::TokenProgram(token) => Ok(token.mint),
        }
    }

    /// The account this side moves through: the token account, or the plain
    /// address lamports are credited to.
    pub fn account(&self) -> Pubkey {
        match self {
            Asset::Native(account) => *account,
            Asset::TokenProgram(token) => token.token_account,
        }
    }

    /// Classify the `(mint, account)` pair the wire carries, for callers that
    /// have a side in that shape rather than a chosen variant.
    pub fn classify(mint: Pubkey, account: Pubkey) -> Self {
        if is_native_sol(mint.as_array()) {
            Asset::Native(account)
        } else {
            Asset::TokenProgram(TokenAsset {
                mint,
                token_account: account,
            })
        }
    }
}

/// Order intent. Its canonical encoding, [`EncodedOrderIntent`], is the exact wire format of create_order's `intent`
/// argument and the exact bytes hashed (SHA-256) to produce the order UID used in the order PDA's seeds.
#[cfg_attr(any(test, feature = "test-fixtures"), derive(Default))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderIntent {
    /// Account authorized to create and invalidate this order and whose
    /// signature authenticates it. For off-chain orders this is the Ed25519
    /// signer; for on-chain creation it must be the transaction signer.
    pub owner: Pubkey,

    /// What the order sells, and the token account the funds are pulled from.
    /// That account implicitly encodes the spender: the settlement state PDA
    /// must hold the SPL `delegate` on it for the order to be settleable, and
    /// it must be owned by the intent owner. An intent that doesn't satisfy
    /// this property will be rejected.
    pub sell: TokenAsset,

    /// What the order buys, and the account that receives the proceeds. That
    /// account implicitly encodes the recipient
    pub buy: Asset,

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

/// Canonical 213-byte representation of an [`OrderIntent`]. The wire format and
/// the order UID preimage.
///
/// Layout: one character per byte, cell widths proportional to field size,
/// each divider belongs to the cell on its right. The byte range is
/// annotated below. Amounts and `valid_to` are little-endian encoded.
///
/// ```text
///                                                                                                                                                                           flags ────┐
/// ┌───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────┬───────┬───┬┬───────────────────────────────┐
/// │                               │                               │                               │                               │                               │sell_  │buy_   │val││                               │
/// │             owner             │      sell_token_account       │           sell_mint           │       buy_token_account       │           buy_mint            │       │       │id_││           app_data            │
/// │                               │                               │                               │                               │                               │amount │amount │to ││                               │
/// └───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────┴───────┴───┴┴───────────────────────────────┘
/// 0                               32                              64                              96                              128                             160     168     176 180                           213
///                                                                                                                                                                                      181
/// ```
///
#[derive(Clone, Debug, Deref, Eq, PartialEq)]
pub struct EncodedOrderIntent([u8; Self::SIZE]);

impl EncodedOrderIntent {
    // Per-field widths, derived from the `OrderIntent` field types.
    const WIDTH_OWNER: usize = size_of::<Pubkey>();
    const WIDTH_SELL_TOKEN: usize = size_of::<Pubkey>();
    const WIDTH_SELL_MINT: usize = size_of::<Pubkey>();
    const WIDTH_BUY_TOKEN: usize = size_of::<Pubkey>();
    const WIDTH_BUY_MINT: usize = size_of::<Pubkey>();
    const WIDTH_SELL_AMOUNT: usize = size_of::<u64>();
    const WIDTH_BUY_AMOUNT: usize = size_of::<u64>();
    const WIDTH_VALID_TO: usize = size_of::<u32>();
    const WIDTH_FLAGS: usize = size_of::<u8>();
    const WIDTH_APP_DATA: usize = size_of::<[u8; 32]>();

    pub const SIZE: usize = 213;

    /// Canonical hash of the bytes.
    pub fn hash(&self) -> Hash {
        hash_bytes(&self.0)
    }
}

/// Given a slice of intent bytes, verify that it can encode a valid intent.
#[must_use = "ignoring the result skips the validation"]
#[inline]
pub fn check_bytes(bytes: &[u8; EncodedOrderIntent::SIZE]) -> Result<(), ProgramError> {
    Flags::try_from(*intent_slots(bytes).flags)?;

    Ok(())
}

pub fn hash_bytes(bytes: &[u8; EncodedOrderIntent::SIZE]) -> Hash {
    solana_sha256_hasher::hashv(&[bytes.as_slice()])
}

impl From<&EncodedOrderIntent> for [u8; EncodedOrderIntent::SIZE] {
    fn from(encoded: &EncodedOrderIntent) -> Self {
        encoded.0
    }
}

/// A borrowed view over an intent's bytes, split into named slots so each
/// field can be named. The slots hold raw encoded bytes, not decoded values.
#[doc(hidden)]
pub struct IntentSlots<'a> {
    pub owner: &'a [u8; EncodedOrderIntent::WIDTH_OWNER],
    pub sell_token: &'a [u8; EncodedOrderIntent::WIDTH_SELL_TOKEN],
    pub sell_mint: &'a [u8; EncodedOrderIntent::WIDTH_SELL_MINT],
    pub buy_token: &'a [u8; EncodedOrderIntent::WIDTH_BUY_TOKEN],
    pub buy_mint: &'a [u8; EncodedOrderIntent::WIDTH_BUY_MINT],
    pub sell_amount: &'a [u8; EncodedOrderIntent::WIDTH_SELL_AMOUNT],
    pub buy_amount: &'a [u8; EncodedOrderIntent::WIDTH_BUY_AMOUNT],
    pub valid_to: &'a [u8; EncodedOrderIntent::WIDTH_VALID_TO],
    pub flags: &'a [u8; EncodedOrderIntent::WIDTH_FLAGS],
    pub app_data: &'a [u8; EncodedOrderIntent::WIDTH_APP_DATA],
}

/// Split an intent's bytes into its named slots.
#[inline]
#[doc(hidden)]
pub fn intent_slots(bytes: &[u8; EncodedOrderIntent::SIZE]) -> IntentSlots<'_> {
    let (
        owner,
        sell_token,
        sell_mint,
        buy_token,
        buy_mint,
        sell_amount,
        buy_amount,
        valid_to,
        flags,
        app_data,
    ) = array_refs![
        bytes,
        EncodedOrderIntent::WIDTH_OWNER,
        EncodedOrderIntent::WIDTH_SELL_TOKEN,
        EncodedOrderIntent::WIDTH_SELL_MINT,
        EncodedOrderIntent::WIDTH_BUY_TOKEN,
        EncodedOrderIntent::WIDTH_BUY_MINT,
        EncodedOrderIntent::WIDTH_SELL_AMOUNT,
        EncodedOrderIntent::WIDTH_BUY_AMOUNT,
        EncodedOrderIntent::WIDTH_VALID_TO,
        EncodedOrderIntent::WIDTH_FLAGS,
        EncodedOrderIntent::WIDTH_APP_DATA
    ];
    IntentSlots {
        owner,
        sell_token,
        sell_mint,
        buy_token,
        buy_mint,
        sell_amount,
        buy_amount,
        valid_to,
        flags,
        app_data,
    }
}

impl From<&OrderIntent> for EncodedOrderIntent {
    /// Lays each side out the way the wire spells it.
    fn from(intent: &OrderIntent) -> Self {
        // `mut_array_refs` checks that `SIZE` is consistent with the sum of
        // the widths.
        let mut out = [0u8; Self::SIZE];
        let (
            owner,
            sell_token,
            sell_mint,
            buy_token,
            buy_mint,
            sell_amount,
            buy_amount,
            valid_to,
            flags,
            app_data,
        ) = mut_array_refs![
            &mut out,
            EncodedOrderIntent::WIDTH_OWNER,
            EncodedOrderIntent::WIDTH_SELL_TOKEN,
            EncodedOrderIntent::WIDTH_SELL_MINT,
            EncodedOrderIntent::WIDTH_BUY_TOKEN,
            EncodedOrderIntent::WIDTH_BUY_MINT,
            EncodedOrderIntent::WIDTH_SELL_AMOUNT,
            EncodedOrderIntent::WIDTH_BUY_AMOUNT,
            EncodedOrderIntent::WIDTH_VALID_TO,
            EncodedOrderIntent::WIDTH_FLAGS,
            EncodedOrderIntent::WIDTH_APP_DATA
        ];
        *owner = intent.owner.to_bytes();
        *sell_token = intent.sell.token_account.to_bytes();
        *sell_mint = intent.sell.mint.to_bytes();
        *buy_token = intent.buy.account().to_bytes();
        *buy_mint = match intent.buy {
            Asset::Native(_) => NATIVE_SOL_MINT,
            Asset::TokenProgram(token) => token.mint,
        }
        .to_bytes();
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

    /// Decode an intent's canonical bytes. Fails with
    /// [`ProgramError::InvalidInstructionData`] if the flags byte sets a
    /// reserved bit; every other byte combination decodes.
    fn try_from(bytes: &[u8; EncodedOrderIntent::SIZE]) -> Result<Self, Self::Error> {
        // It's important that the byte representation of an intent is unique.
        // This function should be injective: there shouldn't be two byte
        // sequences that decode to the same order intent.
        // If this were to happen, then the user intent may not be recognized
        // as valid or it might be possible to replay the same order more
        // than once.
        let slots = intent_slots(bytes);
        Ok(OrderIntent {
            owner: Pubkey::new_from_array(*slots.owner),
            sell: TokenAsset { mint: Pubkey::new_from_array(*slots.sell_mint), token_account: Pubkey::new_from_array(*slots.sell_token) },
            buy: Asset::classify(Pubkey::new_from_array(*slots.buy_mint), Pubkey::new_from_array(*slots.buy_token)),
            sell_amount: u64::from_le_bytes(*slots.sell_amount),
            buy_amount: u64::from_le_bytes(*slots.buy_amount),
            valid_to: u32::from_le_bytes(*slots.valid_to),
            flags: Flags::try_from(*slots.flags)?,
            app_data: *slots.app_data,
        })
    }
}

impl OrderIntent {
    /// SHA-256 of the canonical bytes. Doubles as the order UID and the
    /// middle seed of the order PDA.
    pub fn uid(&self) -> Hash {
        EncodedOrderIntent::from(self).hash()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::{prelude::*, strategy::Union};

    use super::{Asset, Flags, OrderIntent, OrderKind, Pubkey, TokenAsset};

    /// Every valid [`OrderKind`].
    pub const ALL_ORDER_KINDS: [OrderKind; 2] = [OrderKind::Sell, OrderKind::Buy];

    // Hardcoded but verified in a sanity-check test.
    pub const FLAGS_OFFSET: usize = 180;

    pub fn sample_intent(flags: Flags) -> OrderIntent {
        OrderIntent {
            owner: Pubkey::new_from_array([0x11; 32]),
            sell: TokenAsset {
                token_account: Pubkey::new_from_array([0x22; 32]),
                mint: Pubkey::new_from_array([0x33; 32]),
            },
            buy: Asset::TokenProgram(TokenAsset {
                token_account: Pubkey::new_from_array([0x44; 32]),
                mint: Pubkey::new_from_array([0x55; 32]),
            }),
            sell_amount: 0x0123_4567_89ab_cdef,
            buy_amount: 0xfedc_ba98_7654_3210,
            valid_to: 0xdead_beef,
            flags,
            app_data: [0x66; 32],
        }
    }

    /// Any valid [`OrderKind`].
    pub fn arb_order_kind() -> impl Strategy<Value = OrderKind> {
        Union::new(ALL_ORDER_KINDS.map(Just))
    }

    /// Any valid [`Flags`].
    pub fn arb_flags() -> impl Strategy<Value = Flags> {
        (any::<bool>(), arb_order_kind(), any::<bool>()).prop_map(
            |(created_on_chain, kind, partially_fillable)| Flags {
                created_on_chain,
                kind,
                partially_fillable,
            },
        )
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
    ///
    /// Sides are drawn as the `(mint, account)` pairs the wire carries and
    /// classified, which never produces the `TokenProgram`-with-a-native-mint
    /// spelling no caller should write.
    pub fn arb_order_intent() -> impl Strategy<Value = OrderIntent> {
        (
            any::<[u8; 32]>(),
            any::<[u8; 32]>(),
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
                |(
                    owner,
                    sell_tok,
                    sell_mint,
                    buy_tok,
                    buy_mint,
                    sell_amount,
                    buy_amount,
                    valid_to,
                    flags,
                    app,
                )| {
                    OrderIntent {
                        owner: Pubkey::new_from_array(owner),
                        sell: TokenAsset {
                            mint: Pubkey::new_from_array(sell_mint),
                            token_account: Pubkey::new_from_array(sell_tok),
                        },
                        buy: Asset::classify(
                            // Ensure there are some cases where the system program (buy native SOL) is selected
                            // To prevent interrupting common base cases that proptest is likely covering (ex. all 0s), select "random" bytes that must be certain values
                            if buy_mint[4] % 2 == 0 && buy_mint[14] % 2 == 1 {
                                solana_system_interface::program::ID
                            } else {
                                Pubkey::new_from_array(buy_mint)
                            },
                            Pubkey::new_from_array(buy_tok),
                        ),
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
    use crate::data::intent::fixtures::FLAGS_OFFSET;

    use super::fixtures::sample_intent;
    use super::*;

    // Every shape an `OrderIntent` can take on its validated axes: the
    // `created_on_chain` flag bit, the `kind` enum, and the
    // `partially_fillable` flag bit.
    fn all_flag_shapes() -> impl Iterator<Item = OrderIntent> {
        [false, true].into_iter().flat_map(|created_on_chain| {
            fixtures::ALL_ORDER_KINDS.into_iter().flat_map(move |kind| {
                [false, true].into_iter().map(move |partially_fillable| {
                    sample_intent(Flags {
                        created_on_chain,
                        kind,
                        partially_fillable,
                    })
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
            EncodedOrderIntent::WIDTH_SELL_TOKEN,
            size_of_val(&intent.sell.token_account)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_SELL_MINT,
            size_of_val(&intent.sell.mint)
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_BUY_TOKEN,
            size_of_val(&intent.buy.account())
        );
        assert_eq!(
            EncodedOrderIntent::WIDTH_BUY_MINT,
            size_of_val(&NATIVE_SOL_MINT)
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
            created_on_chain: false,
            kind: OrderKind::Sell,
            partially_fillable: false,
        };
        assert_eq!(byte(cleared), 0);

        let set_one_by_one = [
            (
                Flags::CREATED_ON_CHAIN,
                Flags {
                    created_on_chain: true,
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
            (
                Flags::PARTIALLY_FILLABLE,
                Flags {
                    partially_fillable: true,
                    ..cleared
                },
            ),
        ];
        let mut seen = 0u8;
        for (bit, flags) in set_one_by_one {
            assert_eq!(bit.count_ones(), 1, "a flag must occupy a single bit");
            assert_eq!(seen & bit, 0, "two flags must not share a bit");
            assert!(
                bit > seen,
                "each flag must be more significant than the ones before it"
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
            let decoded = OrderIntent::try_from(&*encoded).expect("example must decode");
            assert_eq!(decoded, intent);
        }
    }

    #[test]
    fn decode_accepts_defined_flag_bits_only() {
        let encoded = EncodedOrderIntent::from(&sample_intent(Default::default()));
        let mut bytes: [u8; EncodedOrderIntent::SIZE] = *encoded;
        for flags in u8::MIN..=u8::MAX {
            bytes[FLAGS_OFFSET] = flags;
            let decoded = OrderIntent::try_from(&bytes);
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
        // Compared as hex: `Hash`'s `Display` and `Debug` are both base58,
        // which is not how we represent order UIDs elsewhere.
        fn hex(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
        let intent = sample_intent(Flags {
            created_on_chain: true,
            kind: OrderKind::Buy,
            partially_fillable: true,
        });
        assert_eq!(
            hex(intent.uid().as_ref()),
            "de4096c6c100056f1e4636ea4fafefad40fc1d0b37692fe3ca1e0db3644b86bd",
        );
    }

    #[test]
    fn encoding_regression() {
        let encoded = EncodedOrderIntent::from(&sample_intent(Flags {
            created_on_chain: true,
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
            // sell_token_account ([0x22; 32])
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
            // sell_mint ([0x33; 32])
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33,
            // buy_token_account ([0x44; 32])
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
            // buy_mint ([0x55; 32])
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55,
            // sell_amount (0x0123_4567_89ab_cdef, LE u64)
            0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01,
            // buy_amount (0xfedc_ba98_7654_3210, LE u64)
            0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe,
            // valid_to (0xdead_beef, LE u32)
            0xef, 0xbe, 0xad, 0xde,
            // flags (created_on_chain | kind (Buy = 1) | partially_fillable)
            0b00000111,
            // app_data ([0x66; 32])
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
        ];
        assert_eq!(encoding, expected);
    }

    #[test]
    fn an_asset_lowers_to_the_pair_the_wire_carries() {
        let mint = Pubkey::new_from_array([0x55; 32]);
        let account = Pubkey::new_from_array([0x44; 32]);

        for (buy, expected_mint) in [
            (
                Asset::from(TokenAsset {
                    mint,
                    token_account: account,
                }),
                mint,
            ),
            (Asset::Native(account), NATIVE_SOL_MINT),
        ] {
            let encoded = EncodedOrderIntent::from(&OrderIntent {
                buy,
                ..sample_intent(Default::default())
            });
            assert_eq!(intent_slots(&encoded).buy_mint, expected_mint.as_array());
            assert_eq!(intent_slots(&encoded).buy_token, account.as_array());
        }
    }

    #[test]
    fn only_a_token_side_has_a_mint() {
        let mint = Pubkey::new_from_array([0x55; 32]);
        let account = Pubkey::new_from_array([0x44; 32]);
        let token = Asset::from(TokenAsset {
            mint,
            token_account: account,
        });
        assert_eq!(token.mint(), Ok(mint));
        assert_eq!(Asset::Native(account).mint(), Err(NativeSolHasNoMint));
    }

    #[test]
    fn a_token_side_naming_the_native_marker_is_native_sol() {
        let account = Pubkey::new_from_array([0x44; 32]);
        let spelled_long = TokenAsset {
            mint: NATIVE_SOL_MINT,
            token_account: account,
        };
        assert_eq!(
            Asset::classify(spelled_long.mint, spelled_long.token_account),
            Asset::Native(account),
        );
    }

    #[test]
    fn default_intent_is_the_all_zero_encoding() {
        let intent = OrderIntent::default();
        let encoded = EncodedOrderIntent::from(&intent);
        assert_eq!(*encoded, [0u8; EncodedOrderIntent::SIZE]);
    }

    #[test]
    fn a_native_sell_mint_decodes_to_the_pair_it_names() {
        let intent = OrderIntent {
            sell: TokenAsset {
                mint: NATIVE_SOL_MINT,
                token_account: Pubkey::new_from_array([0x22; 32]),
            },
            ..sample_intent(Default::default())
        };
        let encoded = EncodedOrderIntent::from(&intent);
        assert_eq!(intent_slots(&encoded).sell_mint, NATIVE_SOL_MINT.as_array());
        assert_eq!(intent_slots(&encoded).sell_token, &[0x22; 32]);
        assert_eq!(OrderIntent::try_from(&*encoded).expect("must decode"), intent);
    }

    // Property-based tests, non-deterministic.
    mod proptest {
        use ::proptest::{prelude::*, test_runner::TestCaseError};

        use super::*;
        use crate::data::intent::fixtures::{
            arb_flags_byte, arb_invalid_flags_byte, arb_order_intent, FLAGS_OFFSET,
        };

        proptest! {

            #[test]
            fn unique_intents_do_not_share_uid(intent_a in arb_order_intent(), intent_b in arb_order_intent()) {
                prop_assume!(intent_a != intent_b);
                prop_assert_ne!(intent_a.uid(), intent_b.uid());
            }

            // For any `OrderIntent`, encoding an intent into an encoded
            // intent and then decoding it returns the same intent.
            #[test]
            fn intent_roundtrip(intent in arb_order_intent()) {
                let encoded = EncodedOrderIntent::from(&intent);
                let decoded = OrderIntent::try_from(&*encoded)
                    .map_err(|e| TestCaseError::fail(format!("decode failed: {e:?}")))?;
                prop_assert_eq!(decoded, intent);
            }

            // For any bytes whose flags slot is valid, decoding and then
            // re-encoding produces back the original bytes.
            #[test]
            fn bytes_roundtrip(
                mut bytes in any::<[u8; EncodedOrderIntent::SIZE]>(),
                flags in arb_flags_byte(),
            ) {
                bytes[FLAGS_OFFSET] = flags;
                let intent = OrderIntent::try_from(&bytes)
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
                    OrderIntent::try_from(&bytes),
                    Err(ProgramError::InvalidInstructionData),
                );
            }
        }
    }
}
