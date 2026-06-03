//! Shared scaffolding for the settlement program's unit tests.
//!
//! These functions aren't imported by the program directly, they are only used
//! in unit tests.

use pinocchio::{account::RuntimeAccount, AccountView, Address};

/// Build an `AccountView` whose `.address()` returns `address` and whose
/// data region is empty.
///
/// This is trickier to do than it should be. There's no safe initializer for
/// `AccountView` in Pinocchio. The only initializer is:
/// https://docs.rs/solana-account-view/2.0.0/solana_account_view/struct.AccountView.html#method.new_unchecked
///
/// `AccountView::new_unchecked` requires (1) a pointer to an initialized
/// `RuntimeAccount`, (2) immediately followed by exactly `data_len` bytes of
/// data. We satisfy (1) via `Box::new(RuntimeAccount::default())` (every
/// field is zero-initialized, then we overwrite `address`), and (2) by
/// setting `data_len = 0` so the trailing-data clause is vacuously true
/// regardless of what's actually in memory after the box.
///
/// [`Box::leak`] keeps the backing alive for the rest of the test process:
/// a dropped `Box` or a returned stack slot would leave the pointer
/// dangling. We ignore the memory leak since this function is only intended to
/// use in tests.
/// https://doc.rust-lang.org/std/boxed/struct.Box.html#method.leak
///
/// Every `AccountView` method is safe to call on the result. Header
/// accessors read fields out of the `RuntimeAccount`. Data-region accessors
/// hand out a zero-length slice, which [`core::slice::from_raw_parts`] (the
/// primitive underneath them) defines as sound for any non-null, aligned
/// pointer. This is true for us because the pointer itself comes boxed data
/// and not some manual allocation.
/// https://docs.rs/crate/solana-account-view/2.0.0/source/src/lib.rs#98-295
/// https://doc.rust-lang.org/beta/core/slice/fn.from_raw_parts.html
pub fn fake_account(address: Address) -> AccountView {
    let backing = Box::leak(Box::new(RuntimeAccount::default()));
    backing.address = address;
    backing.data_len = 0;
    unsafe { AccountView::new_unchecked(backing as *mut RuntimeAccount) }
}

pub fn fake_account_from_array(address_array: [u8; 32]) -> AccountView {
    fake_account(Address::new_from_array(address_array))
}
