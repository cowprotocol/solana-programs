//! Settlement state account: its byte layout and the zero-copy accessor over it.
//!
//! The state PDA (see [`crate::pda::state`]) stores the protocol's authority
//! configuration in a fixed header (a discriminator byte followed by the holder
//! of each [`Role`]), then the list of approved solvers, packed and sorted
//! ascending by address.
//!
//! ```text
//!  ┌──── discriminator
//!  ┌┬───────────────────────────────┬───────────────────────────────┬───────────────────────────────┬───── ... ─────┬───────────────────────────────┐
//!  ││            manager            │       reclaim_authority       │           solver[0]           │ other solvers │          solver[N-1]          │
//!  └┴───────────────────────────────┴───────────────────────────────┴───────────────────────────────┴───── ... ─────┴───────────────────────────────┘
//! 0 1                               33                              65                              97
//!  └───────────────────────────── header ──────────────────────────┘└─────────────────────────────── N sorted solvers ─────────────────────────────┘
//! ```
//!
//! [`StateAccount`] is a zero-copy accessor over an account's bytes, generic
//! over the borrow (`&[u8]`, `&mut [u8]`): the reads are available for any
//! borrow, the in-place role write only for a mutable one. It reads and updates
//! the account data directly, so the program never copies the account into an
//! owned struct.

use core::mem::size_of;
use core::ops::{Deref, DerefMut};

use arrayref::{array_refs, mut_array_refs};
use solana_account_view::{AccountView, Ref};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;

use crate::{Role, SettlementAccount, SettlementError};

/// Single-byte account discriminator at the front of the header.
pub const DISCRIMINATOR: u8 = SettlementAccount::SettlementState.discriminator();

/// Byte width of the discriminator.
const WIDTH_DISCRIMINATOR: usize = size_of::<u8>();

/// Byte width of an account address (a [`Pubkey`]): each role holder in the
/// header and each solver in the list is one.
pub const WIDTH_PUBKEY: usize = size_of::<Pubkey>();

/// Length of the fixed header: the discriminator byte followed by one holder
/// per [`Role`].
pub const WIDTH_HEADER: usize = WIDTH_DISCRIMINATOR + 2 * WIDTH_PUBKEY;

/// A borrowed view over the bytes of a state acc, split into its
/// discriminator and per-role slots so each can be named. The slots hold raw
/// encoded bytes, not decoded [`Pubkey`]s.
struct HeaderSlots<'a> {
    discriminator: &'a [u8; WIDTH_DISCRIMINATOR],
    manager: &'a [u8; WIDTH_PUBKEY],
    reclaim_authority: &'a [u8; WIDTH_PUBKEY],
}

/// The mutable counterpart of [`HeaderSlots`], for in-place writes.
struct HeaderSlotsMut<'a> {
    discriminator: &'a mut [u8; WIDTH_DISCRIMINATOR],
    manager: &'a mut [u8; WIDTH_PUBKEY],
    reclaim_authority: &'a mut [u8; WIDTH_PUBKEY],
}

/// Split the header into its named slots.
fn header_slots(header: &[u8; WIDTH_HEADER]) -> HeaderSlots<'_> {
    let (discriminator, manager, reclaim_authority) =
        array_refs![header, WIDTH_DISCRIMINATOR, WIDTH_PUBKEY, WIDTH_PUBKEY];
    HeaderSlots {
        discriminator,
        manager,
        reclaim_authority,
    }
}

/// [`header_slots`] over a mutable header, for in-place writes.
fn header_slots_mut(header: &mut [u8; WIDTH_HEADER]) -> HeaderSlotsMut<'_> {
    let (discriminator, manager, reclaim_authority) =
        mut_array_refs![header, WIDTH_DISCRIMINATOR, WIDTH_PUBKEY, WIDTH_PUBKEY];
    HeaderSlotsMut {
        discriminator,
        manager,
        reclaim_authority,
    }
}

/// The parameters used to initialize the state account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateInitArgs {
    /// The [`Role::Manager`] holder.
    pub manager: Pubkey,
    /// The [`Role::ReclaimAuthority`] holder.
    pub reclaim_authority: Pubkey,
}

/// A zero-copy accessor over a settlement state account's canonical byte
/// representation: the discriminator byte followed by the current holder of
/// each [`Role`].
///
/// `T` is the borrow backing it, anything that dereferences to the account's
/// bytes: `&[u8]` grant read access; `&mut [u8]` grants write access.
pub struct StateAccount<T>(T);

impl<T: Deref<Target = [u8]>> StateAccount<T> {
    /// Wrap an account's bytes, checking they begin with the settlement-state
    /// discriminator and are at least a full header long. Every accessor relies
    /// on that guarantee not to panic.
    pub fn attach(bytes: T) -> Result<Self, ProgramError> {
        let header = bytes
            .first_chunk::<WIDTH_HEADER>()
            .ok_or(ProgramError::InvalidAccountData)?;
        if header_slots(header).discriminator != &[DISCRIMINATOR] {
            return Err(ProgramError::InvalidAccountData);
        }
        Ok(Self(bytes))
    }

    fn header(&self) -> &[u8; WIDTH_HEADER] {
        self.0
            .first_chunk::<WIDTH_HEADER>()
            .expect("header length is guaranteed by any constructor of `StateAccount`")
    }

    /// Current holder of `role`.
    pub fn authority(&self, role: Role) -> Pubkey {
        let slots = header_slots(self.header());
        let holder = match role {
            Role::Manager => slots.manager,
            Role::ReclaimAuthority => slots.reclaim_authority,
        };
        Pubkey::new_from_array(*holder)
    }

    /// The sorted solver list that follows the header, as fixed-width entries.
    /// A trailing partial entry (only possible on a corrupt account) is ignored
    /// and an uninitialized/too-small account returns an empty slice.
    fn solver_region(&self) -> &[[u8; WIDTH_PUBKEY]] {
        let region = self.0.get(WIDTH_HEADER..).unwrap_or_default();
        let (solvers, _partial) = region.as_chunks::<WIDTH_PUBKEY>();
        solvers
    }

    /// Locate `solver` in the sorted solver list, mirroring
    /// [`slice::binary_search`]: `Ok(index)` if present, `Err(index)` with the
    /// slot it would occupy otherwise. The list is sorted by address bytes.
    pub fn solver_search(&self, solver: &Pubkey) -> Result<usize, usize> {
        let seek = solver.to_bytes();
        self.solver_region().binary_search(&seek)
    }

    /// Whether `solver` is in the solver list.
    pub fn is_solver(&self, solver: &Pubkey) -> bool {
        self.solver_search(solver).is_ok()
    }

    /// The stored solvers, in order (sorted ascending by address).
    pub fn solvers(&self) -> impl Iterator<Item = Pubkey> + '_ {
        self.solver_region()
            .iter()
            .map(|raw| Pubkey::new_from_array(*raw))
    }

    /// The account's data length after growing it by one solver slot: the size
    /// it must be resized to before [`insert_solver`](Self::insert_solver) can
    /// fill that new slot.
    ///
    /// Returns [`ProgramError::ArithmeticOverflow`] if that length overflows
    /// `usize`, which a caller handling data from a real account can treat as
    /// unreachable: the runtime caps account data at
    /// [`MAX_PERMITTED_DATA_LENGTH`](solana_system_interface::MAX_PERMITTED_DATA_LENGTH).
    pub fn grown_len(&self) -> Result<usize, ProgramError> {
        self.0
            .len()
            .checked_add(WIDTH_PUBKEY)
            .ok_or(ProgramError::ArithmeticOverflow)
    }
}

impl<'a> StateAccount<Ref<'a, [u8]>> {
    pub fn from_account(account: &'a AccountView) -> Result<Self, ProgramError> {
        Self::attach(account.try_borrow()?)
    }
}

impl<T: DerefMut<Target = [u8]>> StateAccount<T> {
    /// Stamp a fresh header (discriminator + role holders) into a zeroed account
    /// and return the accessor over it.
    ///
    /// The generated data is a full header long, which is needed for the read
    /// accessors not to panic.
    pub fn initialize(mut bytes: T, args: &StateInitArgs) -> Result<Self, ProgramError> {
        {
            let slots = header_slots_mut(
                bytes
                    .first_chunk_mut::<WIDTH_HEADER>()
                    .ok_or(ProgramError::AccountDataTooSmall)?,
            );
            *slots.discriminator = [DISCRIMINATOR];
            *slots.manager = args.manager.to_bytes();
            *slots.reclaim_authority = args.reclaim_authority.to_bytes();
        }
        Ok(Self(bytes))
    }

    fn header_mut(&mut self) -> &mut [u8; WIDTH_HEADER] {
        let bytes: &mut [u8] = &mut self.0;
        bytes
            .first_chunk_mut::<WIDTH_HEADER>()
            .expect("header length is guaranteed by `new`")
    }

    /// Set `role`'s holder to `new` in place.
    pub fn set_authority(&mut self, role: Role, new: &Pubkey) {
        let slots = header_slots_mut(self.header_mut());
        let holder = match role {
            Role::Manager => slots.manager,
            Role::ReclaimAuthority => slots.reclaim_authority,
        };
        *holder = new.to_bytes();
    }

    /// Insert `solver` into the sorted solver list, or fail with
    /// [`SettlementError::SolverAlreadyExists`] if it is already stored.
    ///
    /// The account must already be grown by one solver slot
    /// ([`grown_len`](Self::grown_len)): the trailing slot is spare capacity
    /// for the new entry, so the live list is every entry but that last one.
    /// The solver is placed in sorted order by shifting all entries and
    /// inserting the new solver in the initial slot.
    /// An error is returned if the solver is already included.
    pub fn insert_solver(&mut self, solver: &Pubkey) -> Result<(), ProgramError> {
        let (_spare, occupied) = self
            .solver_region()
            .split_last()
            .expect("account grown by one solver slot before insertion");
        let index = match occupied.binary_search(&solver.to_bytes()) {
            Ok(_) => return Err(SettlementError::SolverAlreadyExists.into()),
            Err(index) => index,
        };

        // Shift the entries at and after `index` up one slot into the spare,
        // then write the solver into the gap that opens at `index`.
        let data: &mut [u8] = &mut self.0;
        let occupied_end = data
            .len()
            .checked_sub(WIDTH_PUBKEY)
            .expect("account grown by one solver slot before insertion");
        let gap = WIDTH_HEADER
            .checked_add(
                index
                    .checked_mul(WIDTH_PUBKEY)
                    .expect("insertion index bound by account length"),
            )
            .expect("insertion offset bound by account length");
        let gap_end = gap
            .checked_add(WIDTH_PUBKEY)
            .expect("insertion slot bound by account length");

        data.copy_within(gap..occupied_end, gap_end);
        data[gap..gap_end].copy_from_slice(&solver.to_bytes());
        Ok(())
    }

    /// Remove `solver` from the sorted solver list, or fail with
    /// [`SettlementError::SolverNotFound`] if it isn't stored. Returns the
    /// length the account must be resized down to.
    ///
    /// The entries after the removed one are shifted one slot left to close the
    /// gap; the now-stale trailing slot is left in place for the caller to drop
    /// by resizing the account down to the returned length.
    pub fn remove_solver(&mut self, solver: &Pubkey) -> Result<usize, ProgramError> {
        let index = match self.solver_region().binary_search(&solver.to_bytes()) {
            Ok(index) => index,
            Err(_) => return Err(SettlementError::SolverNotFound.into()),
        };

        // Shift the entries after `index` down one slot; the trailing slot is
        // left unchanged.
        let data: &mut [u8] = &mut self.0;
        let len = data.len();
        let offset = WIDTH_HEADER
            .checked_add(
                index
                    .checked_mul(WIDTH_PUBKEY)
                    .expect("removal index bound by data length"),
            )
            .expect("removal offset bound by data length");
        let slot_end = offset
            .checked_add(WIDTH_PUBKEY)
            .expect("removal slot bound by data length");

        data.copy_within(slot_end..len, offset);

        let updated_length = len
            .checked_sub(WIDTH_PUBKEY)
            .expect("a solver was removed, so the length doesn't underflow");
        Ok(updated_length)
    }
}

/// Test scaffolding for building state-account bytes, shared by this crate's
/// tests and the settlement program's via the `test-fixtures` feature.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use proptest::prelude::*;
    use solana_pubkey::Pubkey;

    use super::{StateAccount, StateInitArgs, WIDTH_HEADER};

    /// The bytes of a state account: the `header` followed by `solvers`, stored
    /// sorted ascending by address as the on-chain list always is (so callers can
    /// pass them in any order).
    pub fn state_account_bytes(header: &StateInitArgs, solvers: &[Pubkey]) -> Vec<u8> {
        let mut sorted = solvers.to_vec();
        sorted.sort();
        let mut bytes = vec![0u8; WIDTH_HEADER];
        StateAccount::initialize(&mut bytes[..], header).expect("header fits");
        for solver in &sorted {
            bytes.extend_from_slice(&solver.to_bytes());
        }
        bytes
    }

    /// Any valid [`StateInitArgs`].
    pub fn arb_init_params() -> impl Strategy<Value = StateInitArgs> {
        (any::<[u8; 32]>(), any::<[u8; 32]>()).prop_map(|(manager, reclaim_authority)| {
            StateInitArgs {
                manager: Pubkey::new_from_array(manager),
                reclaim_authority: Pubkey::new_from_array(reclaim_authority),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::*;
    use crate::fixtures::pubkey_from_seed;

    /// Byte offset of the discriminator within the account.
    const DISCRIMINATOR_OFFSET: usize = 0;

    static SAMPLE_INIT_ARGS: LazyLock<StateInitArgs> = LazyLock::new(|| StateInitArgs {
        manager: pubkey_from_seed("SAMPLE_INIT_ARGS's sample manager"),
        reclaim_authority: pubkey_from_seed("SAMPLE_INIT_ARGS's sample reclaim authority"),
    });

    /// State account bytes stamped with [`SAMPLE_INIT_ARGS`].
    fn header_bytes() -> [u8; WIDTH_HEADER] {
        let mut bytes = [0u8; WIDTH_HEADER];
        StateAccount::initialize(&mut bytes[..], &SAMPLE_INIT_ARGS).expect("header fits");
        bytes
    }

    /// [`SAMPLE_INIT_ARGS`] followed by `solvers`, stored sorted ascending by address
    /// as the on-chain list always is, so callers can pass them in any order.
    fn state_bytes(solvers: &[Pubkey]) -> Vec<u8> {
        super::fixtures::state_account_bytes(&SAMPLE_INIT_ARGS, solvers)
    }

    #[test]
    fn header_has_the_canonical_wire_layout() {
        assert_eq!(WIDTH_HEADER, 65);

        let bytes = header_bytes();
        assert_eq!(bytes[0], SettlementAccount::SettlementState.discriminator());
        assert_eq!(&bytes[1..33], &SAMPLE_INIT_ARGS.manager.to_bytes()[..]);
        assert_eq!(
            &bytes[33..65],
            &SAMPLE_INIT_ARGS.reclaim_authority.to_bytes()[..]
        );
    }

    #[test]
    fn reads_role_holders_from_the_header() {
        let bytes = header_bytes();
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        assert_eq!(state.authority(Role::Manager), SAMPLE_INIT_ARGS.manager);
        assert_eq!(
            state.authority(Role::ReclaimAuthority),
            SAMPLE_INIT_ARGS.reclaim_authority
        );
    }

    #[test]
    fn new_rejects_wrong_discriminator() {
        let mut bytes = header_bytes();
        bytes[DISCRIMINATOR_OFFSET] = 0xff;
        assert_eq!(
            StateAccount::attach(&bytes[..]).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    #[test]
    fn new_rejects_too_short_buffer() {
        let bytes = header_bytes();
        assert_eq!(
            StateAccount::attach(&bytes[..WIDTH_HEADER - 1]).err(),
            Some(ProgramError::InvalidAccountData),
        );
    }

    /// Sets `target`'s holder and asserts it changed while every other role's
    /// holder stayed put.
    fn assert_set_authority_updates_only(target: Role) {
        let new_holder = pubkey_from_seed("set_authority's new holder");

        let mut bytes = header_bytes();
        let before = Role::ALL.map(|role| {
            StateAccount::attach(&bytes[..])
                .expect("valid header")
                .authority(role)
        });

        StateAccount::attach(&mut bytes[..])
            .expect("valid header")
            .set_authority(target, &new_holder);

        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        for (role, prior) in Role::ALL.into_iter().zip(before) {
            let expected = if role == target { new_holder } else { prior };
            assert_eq!(state.authority(role), expected);
        }
    }

    /// Generates one test, `set_authority_updates_only_<role>`, for `$role`.
    macro_rules! set_authority_test {
        ($name:ident: $role:expr) => {
            #[test]
            fn $name() {
                assert_set_authority_updates_only($role);
            }
        };
    }

    set_authority_test!(set_authority_updates_only_manager: Role::Manager);
    set_authority_test!(set_authority_updates_only_reclaim_authority: Role::ReclaimAuthority);

    #[test]
    fn new_accepts_a_longer_account_and_reads_the_header() {
        let mut bytes = header_bytes().to_vec();
        bytes.push(0x42);

        let state = StateAccount::attach(&bytes[..]).expect("header with trailing bytes is valid");
        assert_eq!(state.authority(Role::Manager), SAMPLE_INIT_ARGS.manager);
        assert_eq!(
            state.authority(Role::ReclaimAuthority),
            SAMPLE_INIT_ARGS.reclaim_authority
        );
    }

    /// Three solvers in arbitrary order; `state_bytes` stores them sorted, as the
    /// on-chain list always is. Returns both the stored bytes and the sorted
    /// order the reads should reflect.
    fn sample_solvers() -> (Vec<u8>, [Pubkey; 3]) {
        let solvers = [
            pubkey_from_seed("sample_solver's solver a"),
            pubkey_from_seed("sample_solver's solver b"),
            pubkey_from_seed("sample_solver's solver c"),
        ];
        let bytes = state_bytes(&solvers);
        let mut sorted = solvers;
        sorted.sort();
        (bytes, sorted)
    }

    #[test]
    fn solvers_is_empty_without_any_stored() {
        let bytes = state_bytes(&[]);
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        assert_eq!(state.solvers().count(), 0);
    }

    #[test]
    fn solvers_lists_stored_solvers_sorted() {
        let (bytes, sorted) = sample_solvers();
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        assert_eq!(state.solvers().collect::<Vec<_>>(), sorted);
    }

    #[test]
    fn solver_search_on_empty_list() {
        let bytes = state_bytes(&[]);
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        let absent = pubkey_from_seed("absent solver");
        assert_eq!(state.solver_search(&absent), Err(0));
    }

    #[test]
    fn solver_search_finds_present() {
        let (bytes, sorted) = sample_solvers();
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        for (index, solver) in sorted.iter().enumerate() {
            assert_eq!(state.solver_search(solver), Ok(index));
        }
    }

    #[test]
    fn solver_search_locates_absent() {
        let (bytes, sorted) = sample_solvers();
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        // An absent solver isn't found; its reported slot is where inserting it
        // would keep the list sorted.
        let absent = pubkey_from_seed("absent solver");
        let slot = state.solver_search(&absent).expect_err("absent solver");
        assert_eq!(slot, sorted.partition_point(|s| s < &absent));
    }

    #[test]
    fn initialize_rejects_too_small_buffer() {
        let mut bytes = [0u8; WIDTH_HEADER - 1];
        assert_eq!(
            StateAccount::initialize(&mut bytes[..], &SAMPLE_INIT_ARGS).err(),
            Some(ProgramError::AccountDataTooSmall),
        );
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            /// `is_solver` is true for every stored solver and false for one that
            /// isn't stored.
            #[test]
            fn is_solver_reflects_membership(
                header in fixtures::arb_init_params(),
                // Unique and already sorted, being a `BTreeSet`.
                raw_solvers in prop::collection::btree_set(any::<[u8; 32]>(), 0..50),
                raw_absent in any::<[u8; 32]>(),
            ) {
                prop_assume!(!raw_solvers.contains(&raw_absent));
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                let absent = Pubkey::new_from_array(raw_absent);

                let bytes = fixtures::state_account_bytes(&header, &stored);
                let state = StateAccount::attach(&bytes[..]).expect("valid header");

                for solver in &stored {
                    prop_assert!(state.is_solver(solver));
                }
                prop_assert!(!state.is_solver(&absent));
            }

            /// The encode roundtrip: any two role holders written with
            /// `initialize` read back unchanged.
            #[test]
            fn account_encode_roundtrip(header in fixtures::arb_init_params()) {
                let mut bytes = [0u8; WIDTH_HEADER];
                StateAccount::initialize(&mut bytes[..], &header).expect("header fits");

                let state = StateAccount::attach(&bytes[..]).expect("valid header");
                let StateInitArgs { manager, reclaim_authority } = header;
                prop_assert_eq!(state.authority(Role::Manager), manager);
                prop_assert_eq!(state.authority(Role::ReclaimAuthority), reclaim_authority);
            }

            #[test]
            fn insert_solver_inserts_an_absent_solver(
                header in fixtures::arb_init_params(),
                // Unique and already sorted, being a `BTreeSet`.
                raw_solvers in prop::collection::btree_set(any::<[u8; 32]>(), 0..50),
                raw_new in any::<[u8; 32]>(),
            ) {
                prop_assume!(!raw_solvers.contains(&raw_new));
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                let new = Pubkey::new_from_array(raw_new);

                // Grow by one slot, exactly as the handler resizes the account
                // before delegating the insert.
                let mut bytes = fixtures::state_account_bytes(&header, &stored);
                let grown_len = StateAccount::attach(&bytes[..])
                    .expect("valid header")
                    .grown_len()
                    .expect("grown length fits");
                prop_assert_eq!(grown_len, bytes.len().strict_add(WIDTH_PUBKEY));
                bytes.resize(grown_len, 0);
                StateAccount::attach(&mut bytes[..])
                    .expect("valid header")
                    .insert_solver(&new)
                    .expect("absent solver inserts");

                let mut expected = stored;
                expected.push(new);
                expected.sort();
                let state = StateAccount::attach(&bytes[..]).expect("valid header");
                prop_assert_eq!(state.solvers().collect::<Vec<_>>(), expected);
            }

            #[test]
            fn insert_solver_rejects_an_existing_solver(
                header in fixtures::arb_init_params(),
                // Unique and already sorted, being a `BTreeSet`.
                raw_solvers in prop::collection::btree_set(any::<[u8; 32]>(), 1..50),
                pick in any::<prop::sample::Index>(),
            ) {
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                let existing = stored[pick.index(stored.len())];

                // Grow by one slot as the handler does, then try to re-add.
                let mut bytes = fixtures::state_account_bytes(&header, &stored);
                bytes.resize(bytes.len().strict_add(WIDTH_PUBKEY), 0);
                prop_assert_eq!(
                    StateAccount::attach(&mut bytes[..])
                        .expect("valid header")
                        .insert_solver(&existing),
                    Err(SettlementError::SolverAlreadyExists.into()),
                );

                // Nothing was written: the stored solvers still read back in
                // order (the trailing spare slot is left as the zero pubkey).
                let state = StateAccount::attach(&bytes[..]).expect("valid header");
                prop_assert_eq!(state.solvers().take(stored.len()).collect::<Vec<_>>(), stored);
            }

            #[test]
            fn remove_solver_drops_a_present_solver(
                header in fixtures::arb_init_params(),
                // Unique and already sorted, being a `BTreeSet`.
                raw_solvers in prop::collection::btree_set(any::<[u8; 32]>(), 1..50),
                pick in any::<prop::sample::Index>(),
            ) {
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                let index = pick.index(stored.len());
                let removed = stored[index];

                // Remove the solver, then shrink to the length it reports,
                // exactly as the handler resizes the account.
                let mut bytes = fixtures::state_account_bytes(&header, &stored);
                let shrunk_len = StateAccount::attach(&mut bytes[..])
                    .expect("valid header")
                    .remove_solver(&removed)
                    .expect("a present solver is removed");
                prop_assert_eq!(shrunk_len, bytes.len().strict_sub(WIDTH_PUBKEY));
                bytes.truncate(shrunk_len);

                let mut expected = stored;
                expected.remove(index);
                let state = StateAccount::attach(&bytes[..]).expect("valid header");
                prop_assert_eq!(state.solvers().collect::<Vec<_>>(), expected);
                prop_assert_eq!(state.solver_search(&removed), Err(index));
            }

            #[test]
            fn remove_solver_rejects_an_absent_solver(
                header in fixtures::arb_init_params(),
                // Unique and already sorted, being a `BTreeSet`.
                raw_solvers in prop::collection::btree_set(any::<[u8; 32]>(), 0..50),
                raw_absent in any::<[u8; 32]>(),
            ) {
                prop_assume!(!raw_solvers.contains(&raw_absent));
                let stored: Vec<Pubkey> =
                    raw_solvers.into_iter().map(Pubkey::new_from_array).collect();
                let absent = Pubkey::new_from_array(raw_absent);

                let mut bytes = fixtures::state_account_bytes(&header, &stored);
                prop_assert_eq!(
                    StateAccount::attach(&mut bytes[..])
                        .expect("valid header")
                        .remove_solver(&absent),
                    Err(SettlementError::SolverNotFound.into()),
                );

                // Nothing was removed: the stored solvers still read back in order.
                let state = StateAccount::attach(&bytes[..]).expect("valid header");
                prop_assert_eq!(state.solvers().collect::<Vec<_>>(), stored);
            }
        }
    }
}
