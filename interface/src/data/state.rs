//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores the protocol's authority
//! configuration: the `manager` and the `reclaim_authority` accounts (see
//! [`StateAccount`]).

use core::mem::size_of;

use arrayref::{array_refs, mut_array_refs};
use derive_more::Deref;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::SettlementAccount;

/// Idiomatic representation of the state PDA's body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateAccount {
    /// The account authorized to add and remove solvers, as well as reassign
    /// all the roles in the settlement program.
    pub manager: Pubkey,
    /// The account authorized to reclaim rent for buffers. It must sign the
    /// reclaim and chooses where the rent goes.
    pub reclaim_authority: Pubkey,
}

/// Canonical 65-byte representation of a [`StateAccount`]: the discriminator
/// byte followed by `manager`'s and `reclaim_authority`'s bytes.
///
/// ```text
///  ┌──── discriminator
///  ┌┬───────────────────────────────┬───────────────────────────────┐
///  ││            manager            │       reclaim_authority       │
///  └┴───────────────────────────────┴───────────────────────────────┘
/// 0 1                               33                              65
/// ```
#[derive(Clone, Debug, Deref, Eq, PartialEq)]
pub struct EncodedStateAccount([u8; Self::SIZE]);

impl EncodedStateAccount {
    // Per-field widths, derived from the `StateAccount` field types.
    const W_DISCRIMINATOR: usize = size_of::<u8>();
    const W_MANAGER: usize = size_of::<Pubkey>();
    const W_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();

    pub const SIZE: usize = 65;

    /// Single-byte account discriminator. See [`crate::SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();
}

/// Writes the canonical [`EncodedStateAccount`] encoding of `account` into
/// `buffer`.
pub fn write_account(buffer: &mut [u8; EncodedStateAccount::SIZE], account: &StateAccount) {
    let StateAccount {
        manager,
        reclaim_authority,
    } = account;
    let (discriminator_slot, manager_slot, reclaim_authority_slot) = mut_array_refs![
        buffer,
        EncodedStateAccount::W_DISCRIMINATOR,
        EncodedStateAccount::W_MANAGER,
        EncodedStateAccount::W_RECLAIM_AUTHORITY
    ];
    *discriminator_slot = [EncodedStateAccount::DISCRIMINATOR];
    *manager_slot = manager.to_bytes();
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
        write_account(&mut out, &account);
        Self(out)
    }
}

impl From<StateAccount> for [u8; EncodedStateAccount::SIZE] {
    fn from(account: StateAccount) -> Self {
        EncodedStateAccount::from(account).into()
    }
}

impl TryFrom<[u8; EncodedStateAccount::SIZE]> for StateAccount {
    type Error = ProgramError;

    fn try_from(bytes: [u8; EncodedStateAccount::SIZE]) -> Result<Self, Self::Error> {
        let (discriminator, manager, reclaim_authority) = array_refs![
            &bytes,
            EncodedStateAccount::W_DISCRIMINATOR,
            EncodedStateAccount::W_MANAGER,
            EncodedStateAccount::W_RECLAIM_AUTHORITY
        ];

        if *discriminator != [EncodedStateAccount::DISCRIMINATOR] {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(StateAccount {
            manager: Pubkey::new_from_array(*manager),
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
    use crate::tests::pubkey_from_seed;

    /// Byte offset of the account discriminator within the encoding.
    const DISCRIMINATOR_OFFSET: usize = 0;

    fn sample_account() -> StateAccount {
        StateAccount {
            manager: pubkey_from_seed("sample_account's manager"),
            reclaim_authority: pubkey_from_seed("sample_account's reclaim_authority"),
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
        write_account(&mut buffer, &account);
        let direct = EncodedStateAccount(buffer);
        let via_state_account = EncodedStateAccount::from(account);
        assert_eq!(direct, via_state_account);
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn account_encode_roundtrip(
                reclaim_authority in any::<[u8; 32]>(),
                manager in any::<[u8; 32]>(),
            ) {
                let account = StateAccount {
                    reclaim_authority: Pubkey::new_from_array(reclaim_authority),
                    manager: Pubkey::new_from_array(manager),
                };
                let encoded = EncodedStateAccount::from(account.clone());
                let decoded = StateAccount::try_from(encoded).expect("should decode after encoding");
                prop_assert_eq!(decoded, account);
            }

            #[test]
            fn account_decode_roundtrip(
                mut bytes in any::<[u8; EncodedStateAccount::SIZE]>(),
            ) {
                bytes[DISCRIMINATOR_OFFSET] = EncodedStateAccount::DISCRIMINATOR;
                let encoded = EncodedStateAccount(bytes);

                let decoded = StateAccount::try_from(encoded.clone()).expect("should decode from valid bytes");
                let re_encoded = EncodedStateAccount::from(decoded);

                prop_assert_eq!(re_encoded, encoded);
            }
        }
    }
}
