//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores the protocol's authority
//! configuration: for every [`Role`] it holds the current holder.

use bytemuck::{Pod, Zeroable};
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
///
/// Every field is byte-granular, so the struct has alignment 1 and no padding:
/// its in-memory image is exactly the 65-byte canonical encoding, which is what
/// lets the program reinterpret the account's data slice as this type in place
/// (no copy, no alignment concern) and mutate a single role's slot directly.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct EncodedStateAccount {
    discriminator: u8,
    manager: [u8; 32],
    reclaim_authority: [u8; 32],
}

const _: () = assert!(core::mem::size_of::<EncodedStateAccount>() == EncodedStateAccount::SIZE);
const _: () = assert!(core::mem::align_of::<EncodedStateAccount>() == 1);

impl core::ops::Deref for EncodedStateAccount {
    type Target = [u8; EncodedStateAccount::SIZE];

    fn deref(&self) -> &Self::Target {
        bytemuck::cast_ref(self)
    }
}

impl EncodedStateAccount {
    pub const SIZE: usize = 65;

    /// Single-byte account discriminator. See [`SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();

    /// Reinterpret canonical bytes as an encoded state account in place, no copy.
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> &Self {
        bytemuck::cast_ref(bytes)
    }

    /// [`Self::from_bytes`] over a mutable buffer, for in-place writes.
    pub fn from_bytes_mut(bytes: &mut [u8; Self::SIZE]) -> &mut Self {
        bytemuck::cast_mut(bytes)
    }

    /// Current holder of `role`, read directly from the encoded bytes.
    ///
    /// The bytes are assumed to be a valid encoding of the canonical state PDA:
    /// only `Initialize` can create an account at that address, so an account of
    /// the right size there is necessarily one this program wrote.
    pub fn authority(bytes: &[u8; Self::SIZE], role: Role) -> Pubkey {
        let state = Self::from_bytes(bytes);
        let slot = match role {
            Role::Manager => &state.manager,
            Role::ReclaimAuthority => &state.reclaim_authority,
        };
        Pubkey::new_from_array(*slot)
    }

    /// Mutable slot holding `role`'s current holder, for an in-place update that
    /// leaves the discriminator and the other roles untouched.
    ///
    /// The bytes are assumed to be a valid encoding of the canonical state PDA:
    /// only `Initialize` can create an account at that address, so an account of
    /// the right size there is necessarily one this program wrote.
    pub fn authority_mut(bytes: &mut [u8; Self::SIZE], role: Role) -> &mut [u8; 32] {
        let state = Self::from_bytes_mut(bytes);
        match role {
            Role::Manager => &mut state.manager,
            Role::ReclaimAuthority => &mut state.reclaim_authority,
        }
    }
}

/// Writes the canonical [`EncodedStateAccount`] encoding of `account` into
/// `buffer`.
pub fn write_account(buffer: &mut [u8; EncodedStateAccount::SIZE], account: &StateAccount) {
    let state = EncodedStateAccount::from_bytes_mut(buffer);
    state.discriminator = EncodedStateAccount::DISCRIMINATOR;
    state.manager = account.manager.to_bytes();
    state.reclaim_authority = account.reclaim_authority.to_bytes();
}

impl From<EncodedStateAccount> for [u8; EncodedStateAccount::SIZE] {
    fn from(encoded: EncodedStateAccount) -> Self {
        *bytemuck::cast_ref(&encoded)
    }
}

impl From<StateAccount> for EncodedStateAccount {
    fn from(account: StateAccount) -> Self {
        EncodedStateAccount {
            discriminator: Self::DISCRIMINATOR,
            manager: account.manager.to_bytes(),
            reclaim_authority: account.reclaim_authority.to_bytes(),
        }
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
        let state = EncodedStateAccount::from_bytes(&bytes);

        if state.discriminator != EncodedStateAccount::DISCRIMINATOR {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(StateAccount {
            manager: Pubkey::new_from_array(state.manager),
            reclaim_authority: Pubkey::new_from_array(state.reclaim_authority),
        })
    }
}

impl TryFrom<EncodedStateAccount> for StateAccount {
    type Error = ProgramError;

    fn try_from(encoded: EncodedStateAccount) -> Result<Self, Self::Error> {
        StateAccount::try_from(<[u8; EncodedStateAccount::SIZE]>::from(encoded))
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
        let direct = *EncodedStateAccount::from_bytes(&buffer);
        let via_state_account = EncodedStateAccount::from(account);
        assert_eq!(direct, via_state_account);
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
                let encoded = *EncodedStateAccount::from_bytes(&bytes);

                let decoded = StateAccount::try_from(encoded).expect("should decode from valid bytes");
                let re_encoded = EncodedStateAccount::from(decoded);

                prop_assert_eq!(re_encoded, encoded);
            }
        }
    }
}
