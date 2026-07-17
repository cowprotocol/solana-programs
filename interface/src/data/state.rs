//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores a single piece of
//! protocol configuration: the `receiver` account that collects reclaimed
//! buffer funds (see `ReclaimBuffer`). Unlike [`crate::data::order`], both
//! directions of the encoding are infallible: every 32-byte sequence is a
//! valid [`Pubkey`].

use derive_more::Deref;
use solana_pubkey::Pubkey;

/// Idiomatic representation of the state PDA's body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateAccount {
    /// Account configured at `Initialize` time that collects reclaimed
    /// buffer funds: it must sign `ReclaimBuffer`, receives each closed
    /// buffer's rent lamports directly, and receives any leftover token
    /// balance via its associated token account for that mint.
    pub receiver: Pubkey,
}

/// Canonical 32-byte representation of a [`StateAccount`]: exactly
/// `receiver`'s bytes, the whole of the state PDA's data area.
#[derive(Clone, Copy, Debug, Deref, Eq, PartialEq)]
pub struct EncodedStateAccount([u8; Self::SIZE]);

impl EncodedStateAccount {
    pub const SIZE: usize = 32;
}

impl From<EncodedStateAccount> for [u8; EncodedStateAccount::SIZE] {
    fn from(encoded: EncodedStateAccount) -> Self {
        encoded.0
    }
}

impl From<StateAccount> for EncodedStateAccount {
    fn from(account: StateAccount) -> Self {
        Self(account.receiver.to_bytes())
    }
}

impl From<[u8; EncodedStateAccount::SIZE]> for StateAccount {
    fn from(bytes: [u8; EncodedStateAccount::SIZE]) -> Self {
        StateAccount {
            receiver: Pubkey::new_from_array(bytes),
        }
    }
}

impl From<EncodedStateAccount> for StateAccount {
    fn from(encoded: EncodedStateAccount) -> Self {
        StateAccount::from(encoded.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let account = StateAccount {
            receiver: Pubkey::new_from_array([0x42; 32]),
        };
        let encoded = EncodedStateAccount::from(account);
        let decoded = StateAccount::from(encoded);
        assert_eq!(decoded, account);
    }

    #[test]
    fn encoding_is_exactly_the_receiver_bytes() {
        let receiver = Pubkey::new_from_array([0x7; 32]);
        let encoded = EncodedStateAccount::from(StateAccount { receiver });
        assert_eq!(*encoded, receiver.to_bytes());
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn account_roundtrip(bytes in any::<[u8; 32]>()) {
                let account = StateAccount::from(bytes);
                let encoded = EncodedStateAccount::from(account);
                let decoded = StateAccount::from(encoded);
                prop_assert_eq!(decoded, account);
            }
        }
    }
}
