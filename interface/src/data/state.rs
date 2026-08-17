//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores the protocol's authority
//! configuration: for every [`Role`] it holds the current holder and, while a
//! transfer is in flight, the proposed next holder.

use core::mem::size_of;

use arrayref::{array_refs, mut_array_refs};
use derive_more::Deref;
use solana_account_view::AccountView;
use solana_address::Address;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::pda::state::state_pda_seeds;
use crate::{Role, SettlementAccount, SettlementError};

/// Idiomatic representation of the state PDA's body.
///
/// Each authority is stored as a pair: its current holder and its
/// `pending_*` proposed next holder. A pending slot equal to [`Pubkey::default`]
/// (the all-zero address) means there is no outstanding transfer proposal for
/// that role; a real authority is never the zero address, so the sentinel is
/// unambiguous.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateAccount {
    /// Current [`Role::Manager`].
    pub manager: Pubkey,
    /// Proposed next [`Role::Manager`], or [`Pubkey::default`] if none.
    pub pending_manager: Pubkey,
    /// Current [`Role::ReclaimAuthority`].
    pub reclaim_authority: Pubkey,
    /// Proposed next [`Role::ReclaimAuthority`], or [`Pubkey::default`] if none.
    pub pending_reclaim_authority: Pubkey,
}

/// Canonical representation of a [`StateAccount`]: the discriminator byte
/// followed by the current holder and the pending proposed holder of each role.
///
/// ```text
///  ┌──── discriminator
///  ┌┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┐
///  ││            manager            │        pending_manager        │       reclaim_authority       │   pending_reclaim_authority   │
///  └┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┘
/// 0 1                               33                              65                              97                              129
/// ```
#[derive(Clone, Debug, Deref, Eq, PartialEq)]
pub struct EncodedStateAccount([u8; Self::SIZE]);

impl EncodedStateAccount {
    // Per-field widths, derived from the `StateAccount` field types.
    const W_DISCRIMINATOR: usize = size_of::<u8>();
    const W_MANAGER: usize = size_of::<Pubkey>();
    const W_PENDING_MANAGER: usize = size_of::<Pubkey>();
    const W_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();
    const W_PENDING_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();

    pub const SIZE: usize = 129;

    /// Single-byte account discriminator. See [`crate::SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();
}

/// Writes the canonical [`EncodedStateAccount`] encoding of `account` into
/// `buffer`.
pub fn write_account(buffer: &mut [u8; EncodedStateAccount::SIZE], account: &StateAccount) {
    let StateAccount {
        manager,
        pending_manager,
        reclaim_authority,
        pending_reclaim_authority,
    } = account;
    let (
        discriminator_slot,
        manager_slot,
        pending_manager_slot,
        reclaim_authority_slot,
        pending_reclaim_authority_slot,
    ) = mut_array_refs![
        buffer,
        EncodedStateAccount::W_DISCRIMINATOR,
        EncodedStateAccount::W_MANAGER,
        EncodedStateAccount::W_PENDING_MANAGER,
        EncodedStateAccount::W_RECLAIM_AUTHORITY,
        EncodedStateAccount::W_PENDING_RECLAIM_AUTHORITY
    ];
    *discriminator_slot = [EncodedStateAccount::DISCRIMINATOR];
    *manager_slot = manager.to_bytes();
    *pending_manager_slot = pending_manager.to_bytes();
    *reclaim_authority_slot = reclaim_authority.to_bytes();
    *pending_reclaim_authority_slot = pending_reclaim_authority.to_bytes();
}

impl StateAccount {
    /// Load and decode the settlement state at `state_pda`, confirming it is the
    /// canonical state PDA under `program_id`.
    ///
    /// The state PDA stores no bump, so its address is re-derived and compared,
    /// the same provenance check the settlement handlers use when signing for
    /// it. Decoding validates the account discriminator.
    pub fn load_from_pda(
        state_pda: &AccountView,
        program_id: &Address,
    ) -> Result<Self, ProgramError> {
        let (expected, _bump) = Address::find_program_address(&state_pda_seeds(), program_id);
        if state_pda.address() != &expected {
            return Err(SettlementError::StateAccountMismatch.into());
        }

        let data = state_pda.try_borrow()?;
        let bytes: &[u8; EncodedStateAccount::SIZE] = (&*data)
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        StateAccount::try_from(*bytes)
    }

    /// Current holder of `role`.
    pub fn authority(&self, role: Role) -> Pubkey {
        match role {
            Role::Manager => self.manager,
            Role::ReclaimAuthority => self.reclaim_authority,
        }
    }

    /// Mutable slot holding the current holder of `role`.
    pub fn authority_mut(&mut self, role: Role) -> &mut Pubkey {
        match role {
            Role::Manager => &mut self.manager,
            Role::ReclaimAuthority => &mut self.reclaim_authority,
        }
    }

    /// Proposed next holder of `role`; [`Pubkey::default`] if none.
    pub fn pending(&self, role: Role) -> Pubkey {
        match role {
            Role::Manager => self.pending_manager,
            Role::ReclaimAuthority => self.pending_reclaim_authority,
        }
    }

    /// Mutable slot holding the proposed next holder of `role`.
    pub fn pending_mut(&mut self, role: Role) -> &mut Pubkey {
        match role {
            Role::Manager => &mut self.pending_manager,
            Role::ReclaimAuthority => &mut self.pending_reclaim_authority,
        }
    }
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
        let (discriminator, manager, pending_manager, reclaim_authority, pending_reclaim_authority) = array_refs![
            &bytes,
            EncodedStateAccount::W_DISCRIMINATOR,
            EncodedStateAccount::W_MANAGER,
            EncodedStateAccount::W_PENDING_MANAGER,
            EncodedStateAccount::W_RECLAIM_AUTHORITY,
            EncodedStateAccount::W_PENDING_RECLAIM_AUTHORITY
        ];

        if *discriminator != [EncodedStateAccount::DISCRIMINATOR] {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(StateAccount {
            manager: Pubkey::new_from_array(*manager),
            pending_manager: Pubkey::new_from_array(*pending_manager),
            reclaim_authority: Pubkey::new_from_array(*reclaim_authority),
            pending_reclaim_authority: Pubkey::new_from_array(*pending_reclaim_authority),
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
            reclaim_authority: pubkey_from_seed("sample_account's reclaim authority"),
            pending_manager: pubkey_from_seed("sample_account's pending manager"),
            pending_reclaim_authority: pubkey_from_seed(
                "sample_account's pending reclaim authority",
            ),
        }
    }

    /// Generates one test per [`Role`], asserting that both its read and
    /// mutable accessors are consistent with the coresponding role's
    /// `current`/`pending` fields.
    macro_rules! role_accessor_tests {
        ($($name:ident: $role:expr => {current: $current:ident, pending: $pending:ident}),+ $(,)?) => {$(
            #[test]
            fn $name() {
                let mut account = sample_account();

                assert_eq!(account.authority($role), account.$current);
                assert_eq!(account.pending($role), account.$pending);

                let new_current = pubkey_from_seed("role_accessor_tests's new current");
                let new_pending = pubkey_from_seed("role_accessor_tests's new pending");
                *account.authority_mut($role) = new_current;
                *account.pending_mut($role) = new_pending;
                assert_eq!(account.$current, new_current);
                assert_eq!(account.$pending, new_pending);
            }
        )+};
    }

    role_accessor_tests! {
        manager_accessors_match_named_fields:
            Role::Manager => {current: manager, pending: pending_manager},
        reclaim_authority_accessors_match_named_fields:
            Role::ReclaimAuthority => {current: reclaim_authority, pending: pending_reclaim_authority},
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

    #[test]
    fn widths_match_field_sizes() {
        use core::mem::{size_of, size_of_val};

        // Any `StateAccount` works: `size_of_val` only consults the field type,
        // never the data.
        let StateAccount {
            manager,
            pending_manager,
            reclaim_authority,
            pending_reclaim_authority,
        } = sample_account();

        assert_eq!(EncodedStateAccount::W_MANAGER, size_of_val(&manager));
        assert_eq!(
            EncodedStateAccount::W_PENDING_MANAGER,
            size_of_val(&pending_manager)
        );
        assert_eq!(
            EncodedStateAccount::W_RECLAIM_AUTHORITY,
            size_of_val(&reclaim_authority)
        );
        assert_eq!(
            EncodedStateAccount::W_PENDING_RECLAIM_AUTHORITY,
            size_of_val(&pending_reclaim_authority)
        );

        assert_eq!(EncodedStateAccount::SIZE, size_of::<EncodedStateAccount>());
    }

    mod load_from_pda {
        use super::*;
        use crate::instruction::fixtures::fake_account_with_data;
        use crate::pda::state::find_state_pda;

        const PROGRAM_ID: Address = Address::new_from_array([0xc0; 32]);

        #[test]
        fn accepts_the_canonical_pda() {
            let account = sample_account();
            let (pda_address, _bump) = find_state_pda(&PROGRAM_ID);
            let state_pda = fake_account_with_data(
                pda_address,
                &EncodedStateAccount::from(account.clone())[..],
            );

            let loaded = StateAccount::load_from_pda(&state_pda, &PROGRAM_ID)
                .expect("canonical PDA must load");
            assert_eq!(loaded, account);
        }

        #[test]
        fn rejects_a_non_canonical_address() {
            // Any address other than the canonical state PDA for this program.
            let wrong_address = pubkey_from_seed("a non-canonical state PDA");
            let state_pda = fake_account_with_data(
                wrong_address,
                &EncodedStateAccount::from(sample_account())[..],
            );

            let err = StateAccount::load_from_pda(&state_pda, &PROGRAM_ID)
                .expect_err("a non-canonical address must be rejected");
            assert_eq!(err, SettlementError::StateAccountMismatch.into());
        }

        #[test]
        fn propagates_decode_errors() {
            let (pda_address, _bump) = find_state_pda(&PROGRAM_ID);
            let mut bytes: [u8; EncodedStateAccount::SIZE] =
                EncodedStateAccount::from(sample_account()).into();
            bytes[DISCRIMINATOR_OFFSET] = 0xff;
            let state_pda = fake_account_with_data(pda_address, &bytes);

            let err = StateAccount::load_from_pda(&state_pda, &PROGRAM_ID)
                .expect_err("a corrupt account must fail to decode");
            assert_eq!(err, ProgramError::InvalidAccountData);
        }
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn account_encode_roundtrip(
                manager in any::<[u8; 32]>(),
                pending_manager in any::<[u8; 32]>(),
                reclaim_authority in any::<[u8; 32]>(),
                pending_reclaim_authority in any::<[u8; 32]>(),
            ) {
                let account = StateAccount {
                    manager: Pubkey::new_from_array(manager),
                    reclaim_authority: Pubkey::new_from_array(reclaim_authority),
                    pending_manager: Pubkey::new_from_array(pending_manager),
                    pending_reclaim_authority: Pubkey::new_from_array(pending_reclaim_authority),
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
