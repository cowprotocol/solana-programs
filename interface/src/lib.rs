//! Shared types and instruction builders for the CoW Protocol settlement program.

pub use solana_instruction::{AccountMeta, Instruction};
pub use solana_pubkey::Pubkey;

solana_pubkey::declare_id!("FYp8R5K4B3B1Kfr7QuWzMz4TwoT7wptjYtxgCrY5sRXb");

pub mod data;
pub mod error;
pub mod instruction;
pub mod pda;
pub mod role;

pub use error::SettlementError;
pub use instruction::{recover_discriminator, SettlementInstruction};
pub use pda::SettlementAccount;
pub use role::Role;

/// Test fixtures for building settlement values with stable, readable
/// addresses. Exposed under the `test-fixtures` feature (and unconditionally
/// for this crate's own `cargo test`) so other crates can reuse them.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod fixtures {
    use crate::Pubkey;

    /// Deterministically generate a [`Pubkey`] by hashing a seed string, for
    /// building fixtures with stable, readable addresses.
    pub fn pubkey_from_seed(seed: &str) -> Pubkey {
        Pubkey::new_from_array(solana_sha256_hasher::hash(seed.as_bytes()).to_bytes())
    }
}
