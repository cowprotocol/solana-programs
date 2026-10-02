//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::{AccountMeta, Instruction};
pub use solana_pubkey::Pubkey;

solana_pubkey::declare_id!("C7PXyLpLQBh3Ce7e9DNj3rDVUvwqa5orDwQG5hs1rfNi");

pub mod data;
pub mod error;
pub mod instruction;
pub mod pda;
pub mod role;
pub mod token_program;

pub use error::SettlementError;
pub use instruction::{recover_discriminator, SettlementInstruction};
pub use pda::SettlementAccount;
pub use role::Role;

/// Test fixtures for building settlement values with stable, readable
/// addresses. Exposed under the `test-fixtures` feature (and unconditionally
/// for this crate's own `cargo test`) so other crates can reuse them.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use core::num::NonZeroU64;

    use crate::Pubkey;

    /// Deterministically generate a [`Pubkey`] by hashing a seed string, for
    /// building fixtures with stable, readable addresses.
    pub fn pubkey_from_seed(seed: &str) -> Pubkey {
        Pubkey::new_from_array(solana_sha256_hasher::hash(seed.as_bytes()).to_bytes())
    }

    /// Wrap a known-non-zero `u64` in a [`NonZeroU64`], as `amount.nz()`.
    /// Panics on zero, with `#[track_caller]` pointing the panic at the call
    /// site rather than here.
    pub trait IntoNonZero {
        fn nz(self) -> NonZeroU64;
    }

    impl IntoNonZero for u64 {
        #[track_caller]
        fn nz(self) -> NonZeroU64 {
            NonZeroU64::new(self).expect("value must be non-zero")
        }
    }
}
