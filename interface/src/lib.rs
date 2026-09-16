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
    use std::sync::LazyLock;

    use crate::pda::state::state_pda_seeds;
    use crate::Pubkey;

    /// Deterministically generate a [`Pubkey`] by hashing a seed string, for
    /// building fixtures with stable, readable addresses.
    pub fn pubkey_from_seed(seed: &str) -> Pubkey {
        Pubkey::new_from_array(solana_sha256_hasher::hash(seed.as_bytes()).to_bytes())
    }

    /// A deterministic stand-in program id shared by handler tests, so each
    /// doesn't define its own. This is an arbitrary placeholder, not the
    /// declared on-chain id.
    pub static PROGRAM_ID: LazyLock<Pubkey> = LazyLock::new(|| pubkey_from_seed("program id"));

    /// The canonical settlement state PDA for [`PROGRAM_ID`], shared so handler
    /// tests don't each re-derive it.
    pub static STATE_PDA: LazyLock<Pubkey> =
        LazyLock::new(|| Pubkey::find_program_address(&state_pda_seeds(), &PROGRAM_ID).0);
}
