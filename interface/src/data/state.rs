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

use crate::{Role, SettlementAccount};

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

/// The role holders that make up a state account's header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
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
        self.solver_region()
            .binary_search_by(|probe| probe.cmp(&seek))
    }

    /// The stored solvers, in order (sorted ascending by address).
    pub fn solvers(&self) -> impl Iterator<Item = Pubkey> + '_ {
        self.solver_region()
            .iter()
            .map(|raw| Pubkey::new_from_array(*raw))
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
    pub fn initialize(mut bytes: T, header: &Header) -> Result<Self, ProgramError> {
        {
            let slots = header_slots_mut(
                bytes
                    .first_chunk_mut::<WIDTH_HEADER>()
                    .ok_or(ProgramError::AccountDataTooSmall)?,
            );
            *slots.discriminator = [DISCRIMINATOR];
            *slots.manager = header.manager.to_bytes();
            *slots.reclaim_authority = header.reclaim_authority.to_bytes();
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
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use super::*;
    use crate::fixtures::pubkey_from_seed;

    /// Byte offset of the discriminator within the account.
    const DISCRIMINATOR_OFFSET: usize = 0;

    static SAMPLE_HEADER: LazyLock<Header> = LazyLock::new(|| Header {
        manager: pubkey_from_seed("SAMPLE_HEADER's sample manager"),
        reclaim_authority: pubkey_from_seed("SAMPLE_HEADER's sample reclaim authority"),
    });

    /// State account bytes stamped with [`SAMPLE_HEADER`].
    fn header_bytes() -> [u8; WIDTH_HEADER] {
        let mut bytes = [0u8; WIDTH_HEADER];
        StateAccount::initialize(&mut bytes[..], &SAMPLE_HEADER).expect("header fits");
        bytes
    }

    /// [`header_bytes`] followed by `solvers`, stored sorted ascending by address
    /// as the on-chain list always is, so callers can pass them in any order.
    fn state_bytes(solvers: &[Pubkey]) -> Vec<u8> {
        let mut solvers = solvers.to_vec();
        solvers.sort();
        let mut bytes = header_bytes().to_vec();
        for solver in &solvers {
            bytes.extend_from_slice(&solver.to_bytes());
        }
        bytes
    }

    #[test]
    fn header_has_the_canonical_wire_layout() {
        assert_eq!(WIDTH_HEADER, 65);

        let bytes = header_bytes();
        assert_eq!(bytes[0], SettlementAccount::SettlementState.discriminator());
        assert_eq!(&bytes[1..33], &SAMPLE_HEADER.manager.to_bytes()[..]);
        assert_eq!(
            &bytes[33..65],
            &SAMPLE_HEADER.reclaim_authority.to_bytes()[..]
        );
    }

    #[test]
    fn reads_role_holders_from_the_header() {
        let bytes = header_bytes();
        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        assert_eq!(state.authority(Role::Manager), SAMPLE_HEADER.manager);
        assert_eq!(
            state.authority(Role::ReclaimAuthority),
            SAMPLE_HEADER.reclaim_authority
        );
    }

    #[test]
    fn initialize_round_trips_a_header() {
        let header = Header {
            manager: pubkey_from_seed("manager"),
            reclaim_authority: pubkey_from_seed("reclaim authority"),
        };

        let mut bytes = [0u8; WIDTH_HEADER];
        StateAccount::initialize(&mut bytes[..], &header).expect("header fits");

        let state = StateAccount::attach(&bytes[..]).expect("valid header");
        let read_back = Header {
            manager: state.authority(Role::Manager),
            reclaim_authority: state.authority(Role::ReclaimAuthority),
        };
        assert_eq!(read_back, header);
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
        assert_eq!(state.authority(Role::Manager), SAMPLE_HEADER.manager);
        assert_eq!(
            state.authority(Role::ReclaimAuthority),
            SAMPLE_HEADER.reclaim_authority
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
            StateAccount::initialize(&mut bytes[..], &SAMPLE_HEADER).err(),
            Some(ProgramError::AccountDataTooSmall),
        );
    }

    mod proptest {
        use ::proptest::prelude::*;

        use super::*;

        proptest! {
            /// The encode roundtrip: any two role holders written with
            /// `initialize` read back unchanged.
            #[test]
            fn account_encode_roundtrip(
                manager in any::<[u8; 32]>(),
                reclaim_authority in any::<[u8; 32]>(),
            ) {
                let header = Header {
                    manager: Pubkey::new_from_array(manager),
                    reclaim_authority: Pubkey::new_from_array(reclaim_authority),
                };

                let mut bytes = [0u8; WIDTH_HEADER];
                StateAccount::initialize(&mut bytes[..], &header).expect("header fits");

                let state = StateAccount::attach(&bytes[..]).expect("valid header");
                prop_assert_eq!(state.authority(Role::Manager), header.manager);
                prop_assert_eq!(state.authority(Role::ReclaimAuthority), header.reclaim_authority);
            }
        }
    }
}
