//! Off-chain instruction builders for the settlement program.
//!
//! Each submodule builds the [`solana_instruction::Instruction`] for specific
//! settlement instructions, encoding their discriminator (see
//! [`crate::SettlementInstruction`]) and laying out the required accounts.

use solana_account_view::AccountView;
use solana_program_error::ProgramError;

use crate::{recover_discriminator, SettlementInstruction};

pub mod create_buffer;
pub mod create_order;
pub mod initialize;
pub mod reclaim_order;
pub mod settle;

/// Shared components for parsing generic instruction input.
///
/// Implementations declare which [`SettlementInstruction`] discriminator they
/// belong to and parse the remaining instruction data and accounts. The
/// discriminator check is shared via the default [`parse`] implementation; an
/// impl only needs to provide [`parse_body`].
pub trait InstructionInputParsing<'a>: Sized {
    const DISCRIMINATOR: SettlementInstruction;

    fn parse_body(
        instruction_data: &'a [u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError>;

    fn parse(
        instruction_data: &'a [u8],
        accounts: &'a mut [AccountView],
    ) -> Result<Self, ProgramError> {
        match recover_discriminator(instruction_data)? {
            (discriminator, remaining_data) if discriminator == Self::DISCRIMINATOR => {
                Self::parse_body(remaining_data, accounts)
            }
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

/// Account-building scaffolding shared by the parser unit tests in this crate
/// and the settlement program's own tests.
///
/// Exposed under the `test-fixtures` feature (and unconditionally for this
/// crate's own `cargo test`) so both crates can build [`AccountView`]s without
/// duplicating the unsafe initializer below.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use solana_account_view::{AccountView, RuntimeAccount};
    use solana_address::Address;

    /// Build an `AccountView` based on the input `RuntimeAccount` and whose
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
    pub fn fake_account_from(runtime_account: RuntimeAccount) -> AccountView {
        let backing = Box::leak(Box::new(runtime_account));
        unsafe { AccountView::new_unchecked(backing as *mut RuntimeAccount) }
    }

    pub fn fake_account_with_data(address: Address, data: &[u8]) -> AccountView {
        // The RuntimeAccount struct actually functions as a header. If any data is included in the account, it should be placed in the bytes following the header.
        // For this, we need to allocate some data on the heap to hold both the account and the data we want to store.
        // We use Box::leak to prevent the memory from being deallocated after this function, which is fine for tests.
        const HEADER: usize = core::mem::size_of::<RuntimeAccount>();

        let buf = Box::leak(Box::<[u8]>::new_uninit_slice(
            HEADER
                .checked_add(data.len())
                .expect("overflow when allocating account data"),
        ));
        let base = buf.as_mut_ptr() as *mut u8;

        unsafe {
            std::ptr::write(
                base as *mut RuntimeAccount,
                RuntimeAccount {
                    address,
                    borrow_state: solana_account_view::NOT_BORROWED, // allows for code to borrow this account to read its data
                    data_len: data.len() as u64,
                    ..Default::default()
                },
            );

            // mostly equivalent to C's `memcpy`
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(HEADER), data.len());
        }

        unsafe { AccountView::new_unchecked(buf.as_mut_ptr() as *mut RuntimeAccount) }
    }

    pub fn fake_account(address: Address) -> AccountView {
        fake_account_with_data(address, &[])
    }

    pub fn fake_account_from_array(address_array: [u8; 32]) -> AccountView {
        fake_account(Address::new_from_array(address_array))
    }

    /// Build `N` fake accounts with sequential addresses (`[1; 32]`, `[2; 32]`, …).
    pub fn fake_sequential_accounts<const N: usize>() -> [AccountView; N] {
        core::array::from_fn(|i| fake_account_from_array([(i as u8).wrapping_add(1); 32]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_parsing_rejects_different_discriminator() {
        struct TestInputParsing {}
        impl<'a> InstructionInputParsing<'a> for TestInputParsing {
            const DISCRIMINATOR: SettlementInstruction = SettlementInstruction::BeginSettle;

            fn parse_body(
                _instruction_data: &'a [u8],
                _accounts: &'a mut [AccountView],
            ) -> Result<Self, ProgramError> {
                Ok(Self {})
            }
        }

        let mut data = [0; 42];
        let different_discriminator = SettlementInstruction::CreateOrder;
        assert_ne!(TestInputParsing::DISCRIMINATOR, different_discriminator);
        data[0] = different_discriminator.discriminator();
        assert_eq!(
            TestInputParsing::parse(&data, &mut []).err(),
            Some(ProgramError::InvalidInstructionData),
        );
    }
}
