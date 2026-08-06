//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores a single piece of
//! protocol configuration: the `reclaim_authority` account (see
//! [`StateAccount::reclaim_authority`]).

use core::mem::size_of;

use arrayref::{array_refs, mut_array_refs};
use derive_more::Deref;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::SettlementAccount;

/// Idiomatic representation of the state PDA's body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateAccount {
    /// The account authorized to reclaim rent for buffers. It must sign the
    /// reclaim and chooses where the rent goes.
    pub reclaim_authority: Pubkey,
}

/// Canonical 33-byte representation of a [`StateAccount`]: the discriminator
/// byte followed by `reclaim_authority`'s bytes.
///
/// ```text
///  ┌──── discriminator
///  ┌┬───────────────────────────────┐
///  ││       reclaim_authority       │
///  └┴───────────────────────────────┘
/// 0 1                               33
/// ```
#[derive(Clone, Debug, Deref, Eq, PartialEq)]
pub struct EncodedStateAccount([u8; Self::SIZE]);

impl EncodedStateAccount {
    // Per-field widths, derived from the `StateAccount` field types.
    const W_DISCRIMINATOR: usize = size_of::<u8>();
    const W_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();

    pub const SIZE: usize = 33;

    /// Single-byte account discriminator. See [`crate::SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();
}

/// Writes the canonical [`EncodedStateAccount`] encoding of the given fields
/// into `buffer`.
pub fn write_account(buffer: &mut [u8; EncodedStateAccount::SIZE], reclaim_authority: &Pubkey) {
    let (discriminator_slot, reclaim_authority_slot) = mut_array_refs![
        buffer,
        EncodedStateAccount::W_DISCRIMINATOR,
        EncodedStateAccount::W_RECLAIM_AUTHORITY
    ];
    *discriminator_slot = [EncodedStateAccount::DISCRIMINATOR];
    *reclaim_authority_slot = reclaim_authority.to_bytes();
}

impl From<EncodedStateAccount> for [u8; EncodedStateAccount::SIZE] {
    fn from(encoded: EncodedStateAccount) -> Self {
        encoded.0
    }
}

impl From<StateAccount> for EncodedStateAccount {
    fn from(account: StateAccount) -> Self {
        let mut out = [0u8; Self::SIZE];
        write_account(&mut out, &account.reclaim_authority);
        Self(out)
    }
}

impl TryFrom<[u8; EncodedStateAccount::SIZE]> for StateAccount {
    type Error = ProgramError;

    fn try_from(bytes: [u8; EncodedStateAccount::SIZE]) -> Result<Self, Self::Error> {
        let (discriminator, reclaim_authority) = array_refs![
            &bytes,
            EncodedStateAccount::W_DISCRIMINATOR,
            EncodedStateAccount::W_RECLAIM_AUTHORITY
        ];

        if *discriminator != [EncodedStateAccount::DISCRIMINATOR] {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(StateAccount {
            reclaim_authority: Pubkey::new_from_array(*reclaim_authority),
        })
    }
}

impl TryFrom<EncodedStateAccount> for StateAccount {
    type Error = ProgramError;

    fn try_from(encoded: EncodedStateAccount) -> Result<Self, Self::Error> {
        StateAccount::try_from(encoded.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_account() -> StateAccount {
        StateAccount {
            reclaim_authority: Pubkey::new_from_array([0x42; 32]),
        }
    }

    #[test]
    fn decode_rejects_wrong_discriminator() {
        let mut bytes: [u8; EncodedStateAccount::SIZE] =
            EncodedStateAccount::from(sample_account()).into();
        bytes[0] ^= 0xff;
        let err = StateAccount::try_from(bytes).expect_err("wrong discriminator must be rejected");
        assert_eq!(err, ProgramError::InvalidAccountData);
    }

    #[test]
    fn direct_write_account_matches_state_account_encoding() {
        let account = sample_account();
        let mut buffer = [0u8; EncodedStateAccount::SIZE];
        write_account(&mut buffer, &account.reclaim_authority);
        let direct = EncodedStateAccount(buffer);
        let via_state_account = EncodedStateAccount::from(account);
        assert_eq!(direct, via_state_account);
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn account_encode_roundtrip(bytes in any::<[u8; 32]>()) {
                let account = StateAccount { reclaim_authority: Pubkey::new_from_array(bytes) };
                let encoded = EncodedStateAccount::from(account.clone());
                let decoded = StateAccount::try_from(encoded).expect("should decode after encoding");
                prop_assert_eq!(decoded, account);
            }

            #[test]
            fn account_decode_roundtrip(bytes in any::<[u8; 32]>()) {
                let mut encoded = [0u8; EncodedStateAccount::SIZE];
                encoded[0] = EncodedStateAccount::DISCRIMINATOR;
                encoded[1..].copy_from_slice(&bytes);
                let encoded = EncodedStateAccount(encoded);

                let decoded = StateAccount::try_from(encoded.clone()).expect("should decode from valid bytes");
                let re_encoded = EncodedStateAccount::from(decoded);

                prop_assert_eq!(re_encoded, encoded);
            }
        }
    }
}
