//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores the protocol's authority
//! configuration: for every [`Role`] it holds the current holder.

use core::mem::size_of;

use arrayref::{array_refs, mut_array_refs};
use derive_more::Deref;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::{Role, SettlementAccount};

/// Idiomatic representation of the state PDA's body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateAccount {
    /// Current [`Role::Manager`].
    pub manager: Pubkey,
    /// Current [`Role::ReclaimAuthority`].
    pub reclaim_authority: Pubkey,
}

/// Canonical representation of a [`StateAccount`]: the discriminator byte
/// followed by the current holder of each role.
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

/// A borrowed view over an [`EncodedStateAccount`]'s bytes, split into its
/// discriminator and per-role slots so each can be named. The slots hold raw
/// encoded bytes, not decoded [`Pubkey`]s.
struct StateAccountRef<'a> {
    discriminator: &'a [u8; EncodedStateAccount::W_DISCRIMINATOR],
    manager: &'a [u8; EncodedStateAccount::W_MANAGER],
    reclaim_authority: &'a [u8; EncodedStateAccount::W_RECLAIM_AUTHORITY],
}

/// The mutable counterpart of [`StateAccountRef`], for in-place writes.
struct StateAccountRefMut<'a> {
    discriminator: &'a mut [u8; EncodedStateAccount::W_DISCRIMINATOR],
    manager: &'a mut [u8; EncodedStateAccount::W_MANAGER],
    reclaim_authority: &'a mut [u8; EncodedStateAccount::W_RECLAIM_AUTHORITY],
}

impl EncodedStateAccount {
    // Per-field widths, derived from the `StateAccount` field types.
    const W_DISCRIMINATOR: usize = size_of::<u8>();
    const W_MANAGER: usize = size_of::<Pubkey>();
    const W_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();

    pub const SIZE: usize = 65;

    /// Single-byte account discriminator. See [`SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();

    /// Borrow the encoding split into its discriminator and per-role slots.
    /// Naming the layout here once keeps every reader and writer below (and the
    /// codec) in step with it.
    fn slots(bytes: &[u8; Self::SIZE]) -> StateAccountRef<'_> {
        let (discriminator, manager, reclaim_authority) = array_refs![
            bytes,
            EncodedStateAccount::W_DISCRIMINATOR,
            EncodedStateAccount::W_MANAGER,
            EncodedStateAccount::W_RECLAIM_AUTHORITY
        ];
        StateAccountRef {
            discriminator,
            manager,
            reclaim_authority,
        }
    }

    /// [`Self::slots`] over a mutable buffer, for in-place writes.
    fn slots_mut(bytes: &mut [u8; Self::SIZE]) -> StateAccountRefMut<'_> {
        let (discriminator, manager, reclaim_authority) = mut_array_refs![
            bytes,
            EncodedStateAccount::W_DISCRIMINATOR,
            EncodedStateAccount::W_MANAGER,
            EncodedStateAccount::W_RECLAIM_AUTHORITY
        ];
        StateAccountRefMut {
            discriminator,
            manager,
            reclaim_authority,
        }
    }

    /// Current holder of `role`, read directly from the encoded bytes.
    ///
    /// The bytes are assumed to be a valid encoding of the canonical state PDA:
    /// only `Initialize` can create an account at that address, so an account of
    /// the right size there is necessarily one this program wrote.
    pub fn authority(bytes: &[u8; Self::SIZE], role: Role) -> Pubkey {
        let state = Self::slots(bytes);
        let slot = match role {
            Role::Manager => state.manager,
            Role::ReclaimAuthority => state.reclaim_authority,
        };
        Pubkey::new_from_array(*slot)
    }

    /// Mutable slot holding `role`'s current holder, for an in-place update that
    /// leaves the discriminator and the other roles untouched.
    ///
    /// The bytes are assumed to be a valid encoding of the canonical state PDA:
    /// only `Initialize` can create an account at that address, so an account of
    /// the right size there is necessarily one this program wrote.
    pub fn authority_mut(
        bytes: &mut [u8; Self::SIZE],
        role: Role,
    ) -> &mut [u8; size_of::<Pubkey>()] {
        let state = Self::slots_mut(bytes);
        match role {
            Role::Manager => state.manager,
            Role::ReclaimAuthority => state.reclaim_authority,
        }
    }
}

/// Writes the canonical [`EncodedStateAccount`] encoding of `account` into
/// `buffer`.
pub fn write_account(buffer: &mut [u8; EncodedStateAccount::SIZE], account: &StateAccount) {
    let StateAccount {
        manager,
        reclaim_authority,
    } = account;
    let slots = EncodedStateAccount::slots_mut(buffer);
    *slots.discriminator = [EncodedStateAccount::DISCRIMINATOR];
    *slots.manager = manager.to_bytes();
    *slots.reclaim_authority = reclaim_authority.to_bytes();
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
        let slots = EncodedStateAccount::slots(&bytes);

        if *slots.discriminator != [EncodedStateAccount::DISCRIMINATOR] {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(StateAccount {
            manager: Pubkey::new_from_array(*slots.manager),
            reclaim_authority: Pubkey::new_from_array(*slots.reclaim_authority),
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
    use crate::fixtures::pubkey_from_seed;

    /// Byte offset of the account discriminator within the encoding.
    const DISCRIMINATOR_OFFSET: usize = 0;

    fn sample_account() -> StateAccount {
        StateAccount {
            manager: pubkey_from_seed("sample_account's manager"),
            reclaim_authority: pubkey_from_seed("sample_account's reclaim authority"),
        }
    }

    /// Generates one test for a [`Role`], asserting that the encoded read
    /// accessor returns the role's named field and the mutable accessor updates
    /// only that field in place.
    macro_rules! role_accessor_test {
        ($name:ident: $role:expr => $field:ident) => {
            #[test]
            fn $name() {
                let account = sample_account();
                let mut bytes: [u8; EncodedStateAccount::SIZE] =
                    EncodedStateAccount::from(account.clone()).into();

                assert_eq!(
                    EncodedStateAccount::authority(&bytes, $role),
                    account.$field
                );

                let new_authority = pubkey_from_seed("role_accessor_test's new authority");
                *EncodedStateAccount::authority_mut(&mut bytes, $role) = new_authority.to_bytes();

                let mut expected = account;
                expected.$field = new_authority;
                assert_eq!(
                    StateAccount::try_from(bytes).expect("should decode"),
                    expected
                );
            }
        };
    }

    role_accessor_test!(manager_accessors_match_named_fields: Role::Manager => manager);
    role_accessor_test!(reclaim_authority_accessors_match_named_fields: Role::ReclaimAuthority => reclaim_authority);

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

    #[test]
    fn widths_match_field_sizes() {
        use core::mem::{size_of, size_of_val};

        // Any `StateAccount` works: `size_of_val` only consults the field type,
        // never the data.
        let StateAccount {
            manager,
            reclaim_authority,
        } = sample_account();

        assert_eq!(EncodedStateAccount::W_MANAGER, size_of_val(&manager));
        assert_eq!(
            EncodedStateAccount::W_RECLAIM_AUTHORITY,
            size_of_val(&reclaim_authority)
        );

        assert_eq!(EncodedStateAccount::SIZE, size_of::<EncodedStateAccount>());
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn account_encode_roundtrip(
                manager in any::<[u8; 32]>(),
                reclaim_authority in any::<[u8; 32]>(),
            ) {
                let account = StateAccount {
                    manager: Pubkey::new_from_array(manager),
                    reclaim_authority: Pubkey::new_from_array(reclaim_authority),
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
