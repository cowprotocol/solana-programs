//! Program-derived addresses under the settlement program.
//!
//! Every PDA shares the [`SETTLEMENT_SEED`] prefix; each submodule defines
//! the additional seeds and the derivation helper for one kind of PDA.

pub mod order;

/// First seed of every PDA derived under the settlement program: the state
/// PDA, the per-token buffer PDAs, and the per-order PDAs all share this
/// prefix.
pub const SETTLEMENT_SEED: &[u8] = b"settlement";
