//! Settlement state PDA body and its canonical byte representation.
//!
//! The state PDA (see [`crate::pda::state`]) stores the protocol's authority
//! configuration: for every [`Role`] it holds the current holder and, while a
//! transfer is in flight, the proposed next holder.
//!
//! The account is never decoded into an owned struct. Handlers borrow the raw
//! account data as an [`EncodedStateAccount`] and read or write individual
//! fields in place through its byte accessors, so nothing copies the (large,
//! solver-padded) body.

use core::mem::size_of;

use arrayref::{array_mut_ref, array_ref, mut_array_refs};
use solana_account_view::AccountView;
use solana_address::Address;
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::pda::state::state_pda_seeds;
use crate::{Role, SettlementAccount, SettlementError};

/// Canonical byte layout of the settlement state PDA: the discriminator byte,
/// the current holder and the pending proposed holder of each role, and a
/// trailing region reserved for the eventual on-chain solver list.
///
/// A pending slot equal to [`Pubkey::default`] (the all-zero address) means
/// there is no outstanding transfer proposal for that role; a real authority is
/// never the zero address, so the sentinel is unambiguous.
///
/// It is a transparent wrapper over the account bytes: [`from_account_data`] and
/// [`from_account_data_mut`] reinterpret a borrowed buffer as `Self` with no
/// copy, and the accessors read and write fields directly within it.
///
/// [`from_account_data`]: Self::from_account_data
/// [`from_account_data_mut`]: Self::from_account_data_mut
///
/// ```text
///  ┌──── discriminator
///  ┌┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬─────────────────────┐
///  ││            manager            │        pending_manager        │       reclaim_authority       │   pending_reclaim_authority   │  solvers (0xff …)    │
///  └┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴─────────────────────┘
/// 0 1                               33                              65                              97                              129                   3329
/// ```
#[repr(transparent)]
#[derive(Debug, Eq, PartialEq)]
pub struct EncodedStateAccount([u8; Self::SIZE]);

impl EncodedStateAccount {
    // Per-field widths, derived from the field types.
    const W_DISCRIMINATOR: usize = size_of::<u8>();
    const W_MANAGER: usize = size_of::<Pubkey>();
    const W_PENDING_MANAGER: usize = size_of::<Pubkey>();
    const W_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();
    const W_PENDING_RECLAIM_AUTHORITY: usize = size_of::<Pubkey>();

    /// Number of solver slots reserved after the authorities. This trailing
    /// region is a placeholder for the eventual on-chain solver list; for now
    /// it is filled with `0xff` so the account occupies its real deployed size.
    const SOLVER_SLOTS: usize = 100;
    /// Width of the reserved solver region.
    const W_SOLVERS: usize = Self::SOLVER_SLOTS * size_of::<Pubkey>();

    // Byte offset of each field, chained from the widths above.
    const O_MANAGER: usize = Self::W_DISCRIMINATOR;
    const O_PENDING_MANAGER: usize = Self::O_MANAGER + Self::W_MANAGER;
    const O_RECLAIM_AUTHORITY: usize = Self::O_PENDING_MANAGER + Self::W_PENDING_MANAGER;
    const O_PENDING_RECLAIM_AUTHORITY: usize =
        Self::O_RECLAIM_AUTHORITY + Self::W_RECLAIM_AUTHORITY;
    const O_SOLVERS: usize = Self::O_PENDING_RECLAIM_AUTHORITY + Self::W_PENDING_RECLAIM_AUTHORITY;

    pub const SIZE: usize = Self::O_SOLVERS + Self::W_SOLVERS;

    /// Single-byte account discriminator. See [`crate::SettlementAccount`].
    pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();

    /// Byte offset of the current holder of `role`.
    const fn authority_offset(role: Role) -> usize {
        match role {
            Role::Manager => Self::O_MANAGER,
            Role::ReclaimAuthority => Self::O_RECLAIM_AUTHORITY,
        }
    }

    /// Byte offset of the pending proposed holder of `role`.
    const fn pending_offset(role: Role) -> usize {
        match role {
            Role::Manager => Self::O_PENDING_MANAGER,
            Role::ReclaimAuthority => Self::O_PENDING_RECLAIM_AUTHORITY,
        }
    }

    /// Reinterpret account `data` as the state encoding, validating its length
    /// and discriminator. Borrows in place — the body is never copied.
    pub fn from_account_data(data: &[u8]) -> Result<&Self, ProgramError> {
        let bytes: &[u8; Self::SIZE] = data
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        if bytes[0] != Self::DISCRIMINATOR {
            return Err(ProgramError::InvalidAccountData);
        }
        // SAFETY: `EncodedStateAccount` is `#[repr(transparent)]` over
        // `[u8; SIZE]`, so a `&[u8; SIZE]` of the right length reinterprets as
        // `&Self` with identical layout.
        Ok(unsafe { &*(bytes as *const [u8; Self::SIZE] as *const Self) })
    }

    /// Mutable counterpart of [`from_account_data`](Self::from_account_data),
    /// for writing fields back into the account in place.
    pub fn from_account_data_mut(data: &mut [u8]) -> Result<&mut Self, ProgramError> {
        let bytes: &mut [u8; Self::SIZE] = data
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?;
        if bytes[0] != Self::DISCRIMINATOR {
            return Err(ProgramError::InvalidAccountData);
        }
        // SAFETY: as in `from_account_data`, with unique access preserved.
        Ok(unsafe { &mut *(bytes as *mut [u8; Self::SIZE] as *mut Self) })
    }

    /// Confirm `state_pda` is the canonical state PDA under `program_id`.
    ///
    /// The state PDA stores no bump, so its address is re-derived and compared,
    /// the same provenance check the settlement handlers use when signing for
    /// it. This only inspects the address; borrow the account separately and
    /// pass its data to [`from_account_data`](Self::from_account_data) (or its
    /// mutable counterpart) to read or edit the body in place.
    pub fn assert_canonical_pda(
        state_pda: &AccountView,
        program_id: &Address,
    ) -> Result<(), ProgramError> {
        let (expected, _bump) = Address::find_program_address(&state_pda_seeds(), program_id);
        if state_pda.address() != &expected {
            return Err(SettlementError::StateAccountMismatch.into());
        }
        Ok(())
    }

    /// Current holder of `role`.
    pub fn authority(&self, role: Role) -> Pubkey {
        Pubkey::new_from_array(*array_ref![
            &self.0,
            EncodedStateAccount::authority_offset(role),
            EncodedStateAccount::W_MANAGER
        ])
    }

    /// Proposed next holder of `role`; [`Pubkey::default`] if none.
    pub fn pending(&self, role: Role) -> Pubkey {
        Pubkey::new_from_array(*array_ref![
            &self.0,
            EncodedStateAccount::pending_offset(role),
            EncodedStateAccount::W_MANAGER
        ])
    }

    /// Record `new_authority` as the pending proposed holder of `role`, written
    /// straight into the account bytes.
    pub fn set_pending(&mut self, role: Role, new_authority: &Pubkey) {
        *array_mut_ref![
            &mut self.0,
            EncodedStateAccount::pending_offset(role),
            EncodedStateAccount::W_MANAGER
        ] = new_authority.to_bytes();
    }

    /// Write the initial layout into a freshly allocated `buffer`: the
    /// discriminator, the two current authorities, cleared pending slots, and
    /// the reserved solver region. This is the only place that lays the whole
    /// encoding down at once; every later change edits a single field in place.
    pub fn write_initial(
        buffer: &mut [u8; Self::SIZE],
        manager: &Pubkey,
        reclaim_authority: &Pubkey,
    ) {
        let (
            discriminator_slot,
            manager_slot,
            pending_manager_slot,
            reclaim_authority_slot,
            pending_reclaim_authority_slot,
            solvers_slot,
        ) = mut_array_refs![
            buffer,
            EncodedStateAccount::W_DISCRIMINATOR,
            EncodedStateAccount::W_MANAGER,
            EncodedStateAccount::W_PENDING_MANAGER,
            EncodedStateAccount::W_RECLAIM_AUTHORITY,
            EncodedStateAccount::W_PENDING_RECLAIM_AUTHORITY,
            EncodedStateAccount::W_SOLVERS
        ];
        *discriminator_slot = [Self::DISCRIMINATOR];
        *manager_slot = manager.to_bytes();
        *pending_manager_slot = [0; EncodedStateAccount::W_PENDING_MANAGER];
        *reclaim_authority_slot = reclaim_authority.to_bytes();
        *pending_reclaim_authority_slot = [0; EncodedStateAccount::W_PENDING_RECLAIM_AUTHORITY];
        solvers_slot.fill(0xff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::pubkey_from_seed;

    /// A valid encoding for `manager`/`reclaim_authority` with no pending
    /// proposals, as [`EncodedStateAccount::write_initial`] lays it out.
    fn encoded(manager: Pubkey, reclaim_authority: Pubkey) -> [u8; EncodedStateAccount::SIZE] {
        let mut buffer = [0u8; EncodedStateAccount::SIZE];
        EncodedStateAccount::write_initial(&mut buffer, &manager, &reclaim_authority);
        buffer
    }

    #[test]
    fn write_initial_lays_out_every_field() {
        let manager = pubkey_from_seed("manager");
        let reclaim_authority = pubkey_from_seed("reclaim authority");
        let bytes = encoded(manager, reclaim_authority);

        assert_eq!(bytes[0], EncodedStateAccount::DISCRIMINATOR);
        let state = EncodedStateAccount::from_account_data(&bytes).expect("valid encoding");
        assert_eq!(state.authority(Role::Manager), manager);
        assert_eq!(state.authority(Role::ReclaimAuthority), reclaim_authority);
        assert_eq!(state.pending(Role::Manager), Pubkey::default());
        assert_eq!(state.pending(Role::ReclaimAuthority), Pubkey::default());
        assert!(
            bytes[EncodedStateAccount::O_SOLVERS..]
                .iter()
                .all(|&b| b == 0xff),
            "the reserved solver region must be filled with 0xff"
        );
    }

    /// One test per [`Role`]: reading the current holder, reading the initially
    /// empty pending slot, and setting the pending slot in place without
    /// disturbing the current holder.
    fn assert_role_accessors_are_consistent(role: Role) {
        let manager = pubkey_from_seed("current manager");
        let reclaim_authority = pubkey_from_seed("current reclaim authority");
        let current = match role {
            Role::Manager => manager,
            Role::ReclaimAuthority => reclaim_authority,
        };

        let mut bytes = encoded(manager, reclaim_authority);
        let state = EncodedStateAccount::from_account_data_mut(&mut bytes).expect("valid encoding");

        assert_eq!(state.authority(role), current);
        assert_eq!(state.pending(role), Pubkey::default());

        let new_pending = pubkey_from_seed("new pending holder");
        state.set_pending(role, &new_pending);
        assert_eq!(state.pending(role), new_pending);
        assert_eq!(
            state.authority(role),
            current,
            "setting the pending slot must not touch the current holder"
        );
    }

    #[test]
    fn manager_accessors_are_consistent() {
        assert_role_accessors_are_consistent(Role::Manager);
    }

    #[test]
    fn reclaim_authority_accessors_are_consistent() {
        assert_role_accessors_are_consistent(Role::ReclaimAuthority);
    }

    #[test]
    fn set_pending_touches_only_its_own_role() {
        let mut bytes = encoded(
            pubkey_from_seed("manager"),
            pubkey_from_seed("reclaim authority"),
        );
        let state = EncodedStateAccount::from_account_data_mut(&mut bytes).expect("valid encoding");

        let new_manager = pubkey_from_seed("new pending manager");
        state.set_pending(Role::Manager, &new_manager);
        assert_eq!(state.pending(Role::Manager), new_manager);
        assert_eq!(
            state.pending(Role::ReclaimAuthority),
            Pubkey::default(),
            "the other role's pending slot must be left empty"
        );
    }

    #[test]
    fn from_account_data_rejects_wrong_discriminator() {
        let mut bytes = encoded(
            pubkey_from_seed("manager"),
            pubkey_from_seed("reclaim authority"),
        );
        bytes[0] ^= 0xff;
        assert_eq!(
            EncodedStateAccount::from_account_data(&bytes).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn from_account_data_rejects_wrong_length() {
        let bytes = [EncodedStateAccount::DISCRIMINATOR; EncodedStateAccount::SIZE - 1];
        assert_eq!(
            EncodedStateAccount::from_account_data(&bytes).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn widths_match_field_sizes() {
        use core::mem::size_of;

        assert_eq!(EncodedStateAccount::W_DISCRIMINATOR, size_of::<u8>());
        assert_eq!(EncodedStateAccount::W_MANAGER, size_of::<Pubkey>());
        assert_eq!(EncodedStateAccount::W_PENDING_MANAGER, size_of::<Pubkey>());
        assert_eq!(
            EncodedStateAccount::W_RECLAIM_AUTHORITY,
            size_of::<Pubkey>()
        );
        assert_eq!(
            EncodedStateAccount::W_PENDING_RECLAIM_AUTHORITY,
            size_of::<Pubkey>()
        );

        assert_eq!(EncodedStateAccount::SIZE, size_of::<EncodedStateAccount>());
    }

    mod assert_canonical_pda {
        use super::*;
        use crate::instruction::fixtures::fake_account_with_data;
        use crate::pda::state::find_state_pda;

        const PROGRAM_ID: Address = Address::new_from_array([0xc0; 32]);

        fn sample() -> [u8; EncodedStateAccount::SIZE] {
            encoded(
                pubkey_from_seed("sample manager"),
                pubkey_from_seed("sample reclaim authority"),
            )
        }

        #[test]
        fn accepts_the_canonical_pda() {
            let manager = pubkey_from_seed("manager");
            let reclaim_authority = pubkey_from_seed("reclaim authority");
            let (pda_address, _bump) = find_state_pda(&PROGRAM_ID);
            let state_pda =
                fake_account_with_data(pda_address, &encoded(manager, reclaim_authority));

            EncodedStateAccount::assert_canonical_pda(&state_pda, &PROGRAM_ID)
                .expect("canonical PDA must be accepted");
            let data = state_pda.try_borrow().expect("account is not borrowed");
            let state = EncodedStateAccount::from_account_data(&data).expect("valid encoding");
            assert_eq!(state.authority(Role::Manager), manager);
            assert_eq!(state.authority(Role::ReclaimAuthority), reclaim_authority);
        }

        #[test]
        fn rejects_a_non_canonical_address() {
            // Any address other than the canonical state PDA for this program.
            let wrong_address = pubkey_from_seed("a non-canonical state PDA");
            let state_pda = fake_account_with_data(wrong_address, &sample());

            let err = EncodedStateAccount::assert_canonical_pda(&state_pda, &PROGRAM_ID)
                .expect_err("a non-canonical address must be rejected");
            assert_eq!(err, SettlementError::StateAccountMismatch.into());
        }

        #[test]
        fn from_account_data_rejects_a_corrupt_discriminator() {
            let mut bytes = sample();
            bytes[0] = 0xff;
            assert_eq!(
                EncodedStateAccount::from_account_data(&bytes).err(),
                Some(ProgramError::InvalidAccountData),
            );
        }
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn set_pending_roundtrips_through_the_bytes(
                manager in any::<[u8; 32]>(),
                reclaim_authority in any::<[u8; 32]>(),
                new_pending in any::<[u8; 32]>(),
            ) {
                let mut bytes = encoded(
                    Pubkey::new_from_array(manager),
                    Pubkey::new_from_array(reclaim_authority),
                );
                let state = EncodedStateAccount::from_account_data_mut(&mut bytes)
                    .expect("valid encoding");

                let new_pending = Pubkey::new_from_array(new_pending);
                state.set_pending(Role::ReclaimAuthority, &new_pending);

                prop_assert_eq!(state.pending(Role::ReclaimAuthority), new_pending);
                prop_assert_eq!(
                    state.authority(Role::ReclaimAuthority),
                    Pubkey::new_from_array(reclaim_authority)
                );
            }
        }
    }
}
